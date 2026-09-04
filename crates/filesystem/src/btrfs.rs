use std::collections::{HashMap, VecDeque};
use std::fs::{self, Metadata};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use akimi_ext4::{FilesystemInfo, FilesystemScan, ScanStats, ScanTimings, ScanWarnings};
use akimi_model::{NameArena, Node, NodeArena, NodeId, NodeKind, ScanResult};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BtrfsError {
    #[error("btrfs scanning requires a mounted btrfs directory: {0}")]
    RequiresMountedDirectory(PathBuf),
    #[error("cannot inspect btrfs path {path}: {source}")]
    InspectPath {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    Arena(#[from] akimi_model::ArenaError),
    #[error(transparent)]
    Aggregate(#[from] akimi_model::AggregateError),
}

impl BtrfsError {
    pub fn is_permission_denied(&self) -> bool {
        matches!(
            self,
            Self::InspectPath { source, .. }
                if source.kind() == std::io::ErrorKind::PermissionDenied
        )
    }
}

pub struct BtrfsFilesystem {
    root: PathBuf,
    info: FilesystemInfo,
    open_time: Duration,
}

impl BtrfsFilesystem {
    pub(crate) fn is_btrfs_path(path: &Path) -> bool {
        is_btrfs(path)
    }

    pub fn open(root: &Path) -> Result<Self, BtrfsError> {
        let started = Instant::now();
        let metadata = fs::metadata(root).map_err(|source| BtrfsError::InspectPath {
            path: root.to_path_buf(),
            source,
        })?;
        if !metadata.is_dir() || !is_btrfs(root) {
            return Err(BtrfsError::RequiresMountedDirectory(root.to_path_buf()));
        }

        Ok(Self {
            root: root.to_path_buf(),
            info: FilesystemInfo {
                device: root.to_path_buf(),
                filesystem_type: "btrfs",
                block_size: metadata.blksize().max(1) as u32,
                inode_count: 0,
                reported_allocated_inodes: 0,
                feature_compat: 0,
                feature_incompat: 0,
                feature_ro_compat: 0,
                size_accounting:
                    "allocated blocks; reflinks and snapshots may be counted more than once",
            },
            open_time: started.elapsed(),
        })
    }

    pub fn info(&self) -> &FilesystemInfo {
        &self.info
    }

    pub fn scan_with_threads(&mut self, _threads: usize) -> Result<FilesystemScan, BtrfsError> {
        let root_metadata =
            fs::symlink_metadata(&self.root).map_err(|source| BtrfsError::InspectPath {
                path: self.root.clone(),
                source,
            })?;
        let mut names = NameArena::default();
        let root_name = names.push(b"/")?;
        let root_node = node(NodeId::ROOT, root_name, &root_metadata, true);
        let mut nodes = vec![root_node];
        let mut queue = VecDeque::from([(self.root.clone(), NodeId::ROOT)]);
        let mut seen = HashMap::new();
        seen.insert(file_key(&root_metadata), NodeId::ROOT);
        let mut stats = ScanStats {
            directories: 1,
            nodes: 1,
            ..ScanStats::default()
        };
        let mut warnings = ScanWarnings::default();
        let directory_started = Instant::now();

        while let Some((directory, parent)) = queue.pop_front() {
            let entries = match fs::read_dir(&directory) {
                Ok(entries) => entries,
                Err(_) => {
                    warnings.directory_scan_errors += 1;
                    continue;
                }
            };
            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(_) => {
                        warnings.directory_scan_errors += 1;
                        continue;
                    }
                };
                let path = entry.path();
                let metadata = match fs::symlink_metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(_) => {
                        warnings.missing_inode_references += 1;
                        continue;
                    }
                };
                let key = file_key(&metadata);
                let kind = kind(&metadata);
                let owns_allocation = !seen.contains_key(&key);
                if owns_allocation {
                    seen.insert(key, NodeId(nodes.len() as u32));
                }
                let name = names.push(entry.file_name().as_bytes())?;
                nodes.push(node_with_size(
                    parent,
                    name,
                    &metadata,
                    kind,
                    owns_allocation,
                ));
                stats.directory_entries += 1;
                stats.nodes += 1;
                match kind {
                    NodeKind::File => stats.files += 1,
                    NodeKind::Directory => {
                        stats.directories += 1;
                        queue.push_back((path, NodeId((nodes.len() - 1) as u32)));
                    }
                    NodeKind::Symlink => stats.symlinks += 1,
                    NodeKind::Other => stats.other += 1,
                }
                if !owns_allocation {
                    stats.hard_link_entries += 1;
                }
            }
        }

        let directory_scan = directory_started.elapsed();
        let tree_started = Instant::now();
        let result = ScanResult::new(NodeArena::from_parts(nodes, names))?;
        stats.allocated_inodes = seen.len() as u64;
        let tree_build = tree_started.elapsed();
        self.info.inode_count = seen.len() as u64;
        self.info.reported_allocated_inodes = seen.len() as u64;
        Ok(FilesystemScan {
            result,
            stats,
            warnings,
            workers: 1,
            timings: ScanTimings {
                open: self.open_time,
                inode_scan: Duration::ZERO,
                directory_scan,
                tree_build,
                aggregation: Duration::ZERO,
            },
        })
    }
}

fn is_btrfs(path: &Path) -> bool {
    let path = match std::ffi::CString::new(path.as_os_str().as_bytes()) {
        Ok(path) => path,
        Err(_) => return false,
    };
    let mut stats = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: statfs writes the complete struct when it returns zero, and the
    // path is a valid NUL-terminated C string owned by this function.
    let result = unsafe { libc::statfs(path.as_ptr(), stats.as_mut_ptr()) };
    result == 0 && unsafe { stats.assume_init() }.f_type as u64 == 0x9123_683e
}

fn file_key(metadata: &Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}

fn kind(metadata: &Metadata) -> NodeKind {
    let file_type = metadata.file_type();
    if file_type.is_dir() {
        NodeKind::Directory
    } else if file_type.is_file() {
        NodeKind::File
    } else if file_type.is_symlink() {
        NodeKind::Symlink
    } else {
        NodeKind::Other
    }
}

fn node(parent: NodeId, name: akimi_model::NameRef, metadata: &Metadata, root: bool) -> Node {
    node_with_size(parent, name, metadata, NodeKind::Directory, root)
}

fn node_with_size(
    parent: NodeId,
    name: akimi_model::NameRef,
    metadata: &Metadata,
    kind: NodeKind,
    owns_allocation: bool,
) -> Node {
    Node {
        parent,
        inode: metadata.ino(),
        name,
        kind,
        logical_size: metadata.len(),
        allocated_size: owns_allocation
            .then_some(metadata.blocks() * 512)
            .unwrap_or(0),
        links: metadata.nlink() as u32,
        mtime: metadata.mtime(),
    }
}
