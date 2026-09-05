use std::collections::{HashMap, VecDeque};
use std::fs::{self, Metadata};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::{Duration, Instant};

use akimi_model::{FilesystemInfo, FilesystemScan, ScanStats, ScanTimings, ScanWarnings};
use akimi_model::{NameArena, Node, NodeArena, NodeId, NodeKind, ScanResult};

pub fn scan(
    root: &Path,
    info: &mut FilesystemInfo,
    open_time: Duration,
) -> Result<FilesystemScan, super::BtrfsError> {
    let root_metadata =
        fs::symlink_metadata(root).map_err(|source| super::BtrfsError::InspectPath {
            path: root.to_path_buf(),
            source,
        })?;
    let mut names = NameArena::default();
    let root_name = names.push(b"/")?;
    let root_node = node(NodeId::ROOT, root_name, &root_metadata);
    let mut nodes = vec![root_node];
    let mut queue = VecDeque::from([(root.to_path_buf(), NodeId::ROOT)]);
    let mut seen = HashMap::from([(file_key(&root_metadata), NodeId::ROOT)]);
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
    info.inode_count = seen.len() as u64;
    info.reported_allocated_inodes = seen.len() as u64;
    Ok(FilesystemScan {
        result,
        stats,
        warnings,
        workers: 1,
        timings: ScanTimings {
            open: open_time,
            inode_scan: Duration::ZERO,
            directory_scan,
            tree_build,
            aggregation: Duration::ZERO,
        },
    })
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

fn node(parent: NodeId, name: akimi_model::NameRef, metadata: &Metadata) -> Node {
    node_with_size(parent, name, metadata, NodeKind::Directory, true)
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
        allocated_size: if owns_allocation {
            metadata.blocks() * 512
        } else {
            0
        },
        links: metadata.nlink() as u32,
        mtime: metadata.mtime(),
    }
}
