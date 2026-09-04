use std::collections::VecDeque;
use std::fs::{self, File, Metadata};
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use akimi_model::{ArenaError, NameArena, NameRef, Node, NodeArena, NodeId, NodeKind, ScanResult};
use thiserror::Error;

use crate::ffi;

const ROOT_INODE: u64 = 2;
/// Directories claimed per work-stealing step. Large enough to amortize the
/// atomic, small enough to keep all workers busy until the end.
const DIRECTORY_BATCH: usize = 64;

#[derive(Debug, Error)]
pub enum Ext4Error {
    #[error("cannot inspect {path}: {source}")]
    InspectPath {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{0} is neither a block device nor a regular filesystem image")]
    UnsupportedPath(PathBuf),
    #[error("{0} is an ext2 or ext3 filesystem; Akimi requires ext4")]
    NotExt4(PathBuf),
    #[error("libext2fs: {message}")]
    Native { message: String, code: Option<i64> },
    #[error("filesystem has no live root inode")]
    MissingRoot,
    #[error("filesystem root inode is not a directory")]
    RootNotDirectory,
    #[error("filesystem scan worker panicked")]
    WorkerPanicked,
    #[error(transparent)]
    Arena(#[from] ArenaError),
    #[error(transparent)]
    Aggregate(#[from] akimi_model::AggregateError),
}

impl Ext4Error {
    pub fn is_permission_denied(&self) -> bool {
        match self {
            Self::InspectPath { source, .. } => {
                source.kind() == std::io::ErrorKind::PermissionDenied
            }
            Self::Native { code, .. } => *code == Some(13),
            _ => false,
        }
    }
}

impl From<ffi::NativeError> for Ext4Error {
    fn from(error: ffi::NativeError) -> Self {
        Self::Native {
            code: error.code(),
            message: error.to_string(),
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FilesystemInfo {
    pub device: PathBuf,
    pub filesystem_type: &'static str,
    pub block_size: u32,
    pub inode_count: u64,
    pub reported_allocated_inodes: u64,
    pub feature_compat: u32,
    pub feature_incompat: u32,
    pub feature_ro_compat: u32,
    pub size_accounting: &'static str,
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ScanTimings {
    pub open: Duration,
    pub inode_scan: Duration,
    pub directory_scan: Duration,
    pub tree_build: Duration,
    pub aggregation: Duration,
}

impl ScanTimings {
    pub fn scan_total(self) -> Duration {
        self.open + self.inode_scan + self.directory_scan + self.tree_build + self.aggregation
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScanStats {
    pub allocated_inodes: u64,
    pub files: u64,
    pub directories: u64,
    pub symlinks: u64,
    pub other: u64,
    pub nodes: u64,
    pub directory_entries: u64,
    pub hard_link_entries: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScanWarnings {
    pub missing_inode_references: u64,
    pub nodes_without_parent: u64,
    pub directory_scan_errors: u64,
    pub directory_aliases: u64,
}

impl ScanWarnings {
    pub fn total(self) -> u64 {
        self.missing_inode_references
            + self.nodes_without_parent
            + self.directory_scan_errors
            + self.directory_aliases
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct FilesystemScan {
    pub result: ScanResult,
    pub stats: ScanStats,
    pub timings: ScanTimings,
    pub warnings: ScanWarnings,
    pub workers: usize,
}

pub struct Ext4Filesystem {
    native: ffi::Handle,
    source: FilesystemSource,
    info: FilesystemInfo,
    first_inode: u32,
    inodes_per_group: u32,
    group_count: u32,
    open_time: Duration,
}

enum FilesystemSource {
    Path(PathBuf),
    Descriptor(File),
}

impl FilesystemSource {
    fn open_native(&self) -> Result<ffi::Handle, Ext4Error> {
        match self {
            Self::Path(path) => ffi::Handle::open(path).map_err(Into::into),
            Self::Descriptor(descriptor) => {
                ffi::Handle::open_fd(descriptor.as_raw_fd()).map_err(Into::into)
            }
        }
    }

    fn metadata(&self) -> std::io::Result<Metadata> {
        match self {
            Self::Path(path) => fs::metadata(path),
            Self::Descriptor(descriptor) => descriptor.metadata(),
        }
    }

    fn worker_source(&self) -> WorkerSource {
        match self {
            Self::Path(path) => WorkerSource::Path(path.clone()),
            Self::Descriptor(descriptor) => WorkerSource::Descriptor(descriptor.as_raw_fd()),
        }
    }
}

#[derive(Clone)]
enum WorkerSource {
    Path(PathBuf),
    Descriptor(std::os::fd::RawFd),
}

impl WorkerSource {
    fn open_native(&self) -> Result<ffi::Handle, Ext4Error> {
        match self {
            Self::Path(path) => ffi::Handle::open(path).map_err(Into::into),
            Self::Descriptor(fd) => ffi::Handle::open_fd(*fd).map_err(Into::into),
        }
    }
}

impl Ext4Filesystem {
    pub fn open(device: &Path) -> Result<Self, Ext4Error> {
        Self::open_source(device, FilesystemSource::Path(device.to_path_buf()))
    }

    /// Opens a filesystem through a descriptor supplied by an external access
    /// broker. The descriptor remains owned for the lifetime of the scan.
    pub fn open_descriptor(device: &Path, descriptor: File) -> Result<Self, Ext4Error> {
        Self::open_source(device, FilesystemSource::Descriptor(descriptor))
    }

    fn open_source(device: &Path, source: FilesystemSource) -> Result<Self, Ext4Error> {
        let started = Instant::now();
        let metadata = source.metadata().map_err(|source| Ext4Error::InspectPath {
            path: device.to_path_buf(),
            source,
        })?;
        let file_type = metadata.file_type();
        if !file_type.is_block_device() && !file_type.is_file() {
            return Err(Ext4Error::UnsupportedPath(device.to_path_buf()));
        }

        let native = source.open_native()?;
        let raw_info = native.info();
        if raw_info.is_ext4 == 0 {
            return Err(Ext4Error::NotExt4(device.to_path_buf()));
        }
        let info = FilesystemInfo {
            device: device.to_path_buf(),
            filesystem_type: "ext4",
            block_size: raw_info.block_size,
            inode_count: raw_info.inode_count,
            reported_allocated_inodes: raw_info.allocated_inode_count,
            feature_compat: raw_info.feature_compat,
            feature_incompat: raw_info.feature_incompat,
            feature_ro_compat: raw_info.feature_ro_compat,
            size_accounting: "allocated blocks",
        };
        Ok(Self {
            native,
            source,
            info,
            first_inode: raw_info.first_inode,
            inodes_per_group: raw_info.inodes_per_group,
            group_count: raw_info.group_count,
            open_time: started.elapsed(),
        })
    }

    pub fn info(&self) -> &FilesystemInfo {
        &self.info
    }

    pub fn scan(&mut self) -> Result<FilesystemScan, Ext4Error> {
        self.scan_with_threads(1)
    }

    pub fn scan_with_threads(&mut self, threads: usize) -> Result<FilesystemScan, Ext4Error> {
        let worker_count = threads.max(1).min(self.group_count as usize);
        let (inodes, directories, inode_scan, directory_scan) = if worker_count == 1 {
            self.scan_serial()?
        } else {
            self.scan_parallel(worker_count)?
        };

        self.finish_scan(
            inodes,
            directories,
            inode_scan,
            directory_scan,
            worker_count,
        )
    }

    fn scan_serial(
        &mut self,
    ) -> Result<(Vec<InodeInfo>, DirectoryScan, Duration, Duration), Ext4Error> {
        let inode_started = Instant::now();
        self.native.load_inode_bitmap()?;
        let capacity = self
            .info
            .reported_allocated_inodes
            .min(self.info.inode_count) as usize
            + 1;
        let mut inodes = Vec::with_capacity(capacity);
        let first_inode = self.first_inode;
        self.native
            .scan_inodes_batched(1, self.info.inode_count, |batch| {
                inodes.extend(
                    batch
                        .iter()
                        .copied()
                        .filter(|raw| raw.inode == ROOT_INODE || raw.inode >= first_inode as u64)
                        .map(InodeInfo::from),
                );
                true
            })?;
        debug_assert!(
            inodes.windows(2).all(|pair| pair[0].inode < pair[1].inode),
            "inode scan must yield ascending inode numbers"
        );
        let inode_scan = inode_started.elapsed();

        let directory_started = Instant::now();
        let mut work = DirWork::with_capacity(
            inodes
                .iter()
                .filter(|inode| inode.kind == NodeKind::Directory)
                .count(),
        );
        for inode in inodes
            .iter()
            .filter(|inode| inode.kind == NodeKind::Directory)
        {
            scan_one_directory(&mut self.native, inode.inode, &mut work)?;
        }
        let directories = merge_dir_chunks(vec![work])?;
        let directory_scan = directory_started.elapsed();
        Ok((inodes, directories, inode_scan, directory_scan))
    }

    fn scan_parallel(
        &self,
        worker_count: usize,
    ) -> Result<(Vec<InodeInfo>, DirectoryScan, Duration, Duration), Ext4Error> {
        let source = self.source.worker_source();
        let first_inode = self.first_inode;
        let inode_count = self.info.inode_count;
        let inodes_per_group = self.inodes_per_group as u64;
        let group_count = self.group_count as usize;
        let total_hint = self.info.reported_allocated_inodes.min(inode_count) as usize + 1;

        // Phase 1: block groups are claimed atomically so workers that hit
        // bitmap-empty groups (skipped without I/O) immediately move on
        // instead of idling behind workers with dense groups.
        let inode_started = Instant::now();
        let group_cursor = AtomicUsize::new(0);
        let inodes = thread::scope(|scope| {
            let mut handles = Vec::with_capacity(worker_count);
            for _ in 0..worker_count {
                let source = source.clone();
                let group_cursor = &group_cursor;
                handles.push(scope.spawn(move || -> Result<Vec<InodeInfo>, Ext4Error> {
                    let mut native = source.open_native()?;
                    native.load_inode_bitmap()?;
                    let mut local = Vec::with_capacity(total_hint / worker_count + 1024);
                    loop {
                        let group = group_cursor.fetch_add(1, Ordering::Relaxed);
                        if group >= group_count {
                            break;
                        }
                        let range_start = group as u64 * inodes_per_group + 1;
                        let range_end = ((group as u64 + 1) * inodes_per_group).min(inode_count);
                        if range_start > range_end {
                            continue;
                        }
                        native.scan_inodes_batched(range_start, range_end, |batch| {
                            local.extend(
                                batch
                                    .iter()
                                    .copied()
                                    .filter(|raw| {
                                        raw.inode == ROOT_INODE || raw.inode >= first_inode as u64
                                    })
                                    .map(InodeInfo::from),
                            );
                            true
                        })?;
                    }
                    Ok(local)
                }));
            }
            let mut inodes = Vec::with_capacity(total_hint);
            for handle in handles {
                inodes.append(&mut handle.join().map_err(|_| Ext4Error::WorkerPanicked)??);
            }
            // Workers claim groups in nondeterministic order; restore the
            // canonical ascending order so parallel and serial scans agree.
            inodes.sort_by_key(|inode| inode.inode);
            Ok::<_, Ext4Error>(inodes)
        })?;
        let inode_scan = inode_started.elapsed();

        // Phase 2: directories are claimed in batches for the same reason;
        // directory sizes vary wildly (a few huge dirs, many tiny ones).
        let directory_started = Instant::now();
        let dir_inodes = inodes
            .iter()
            .filter(|inode| inode.kind == NodeKind::Directory)
            .map(|inode| inode.inode)
            .collect::<Vec<_>>();
        let dir_cursor = AtomicUsize::new(0);
        let chunks = thread::scope(|scope| {
            let mut handles = Vec::with_capacity(worker_count);
            for _ in 0..worker_count {
                let source = source.clone();
                let dir_inodes = &dir_inodes;
                let dir_cursor = &dir_cursor;
                handles.push(scope.spawn(move || -> Result<DirWork, Ext4Error> {
                    let mut native = source.open_native()?;
                    let mut work =
                        DirWork::with_capacity(dir_inodes.len() / worker_count + DIRECTORY_BATCH);
                    loop {
                        let base = dir_cursor.fetch_add(DIRECTORY_BATCH, Ordering::Relaxed);
                        if base >= dir_inodes.len() {
                            break;
                        }
                        let end = (base + DIRECTORY_BATCH).min(dir_inodes.len());
                        for &directory in &dir_inodes[base..end] {
                            scan_one_directory(&mut native, directory, &mut work)?;
                        }
                    }
                    Ok(work)
                }));
            }
            handles
                .into_iter()
                .map(|handle| handle.join().map_err(|_| Ext4Error::WorkerPanicked)?)
                .collect::<Result<Vec<_>, Ext4Error>>()
        })?;
        let directory_scan = directory_started.elapsed();

        let directories = merge_dir_chunks(chunks)?;
        Ok((inodes, directories, inode_scan, directory_scan))
    }

    fn finish_scan(
        &self,
        inodes: Vec<InodeInfo>,
        mut directories: DirectoryScan,
        inode_scan: Duration,
        directory_scan: Duration,
        workers: usize,
    ) -> Result<FilesystemScan, Ext4Error> {
        let mut stats = ScanStats {
            allocated_inodes: inodes.len() as u64,
            directory_entries: directories.entries.len() as u64,
            ..ScanStats::default()
        };
        for inode in &inodes {
            match inode.kind {
                NodeKind::File => stats.files += 1,
                NodeKind::Directory => stats.directories += 1,
                NodeKind::Symlink => stats.symlinks += 1,
                NodeKind::Other => stats.other += 1,
            }
        }

        let root_name = directories.names.push(b"/")?;

        let tree_started = Instant::now();
        let (arena, tree_warnings, hard_link_entries) = build_tree(
            inodes,
            directories.entries,
            directories.ranges,
            directories.names,
            root_name,
        )?;
        directories.warnings.missing_inode_references = tree_warnings.missing_inode_references;
        directories.warnings.nodes_without_parent = tree_warnings.nodes_without_parent;
        directories.warnings.directory_aliases = tree_warnings.directory_aliases;
        stats.hard_link_entries = hard_link_entries;
        stats.nodes = arena.nodes().len() as u64;
        let tree_build = tree_started.elapsed();

        let aggregation_started = Instant::now();
        let result = ScanResult::new(arena)?;
        let aggregation = aggregation_started.elapsed();

        Ok(FilesystemScan {
            result,
            stats,
            warnings: directories.warnings,
            workers,
            timings: ScanTimings {
                open: self.open_time,
                inode_scan,
                directory_scan,
                tree_build,
                aggregation,
            },
        })
    }
}

struct DirectoryScan {
    names: NameArena,
    entries: Vec<DirectoryEntry>,
    ranges: Vec<DirRange>,
    warnings: ScanWarnings,
}

/// Contiguous slice of `entries` belonging to one parent directory.
/// `entries` is stably sorted by `parent`, so ranges are built with one
/// linear pass and looked up with binary search instead of a hash map.
#[derive(Clone, Copy, Debug)]
struct DirRange {
    parent: u64,
    start: usize,
    end: usize,
}

#[derive(Default)]
struct DirWork {
    entries: Vec<DirectoryEntry>,
    names: NameArena,
    warnings: ScanWarnings,
}

impl DirWork {
    fn with_capacity(directories: usize) -> Self {
        Self {
            // Average directories hold a handful of entries; this is only a
            // growth hint, exact sizing happens at merge time.
            entries: Vec::with_capacity(directories.saturating_mul(4)),
            names: NameArena::default(),
            warnings: ScanWarnings::default(),
        }
    }
}

fn scan_one_directory(
    native: &mut ffi::Handle,
    directory: u64,
    work: &mut DirWork,
) -> Result<(), Ext4Error> {
    let mut arena_error = None;
    let result =
        native.scan_directory_batched(directory, |parent, children, offsets, lengths, names| {
            debug_assert_eq!(parent, directory);
            work.entries.reserve(children.len());
            work.names.reserve(names.len() + children.len());
            for (index, &child) in children.iter().enumerate() {
                let start = offsets[index] as usize;
                let end = start + lengths[index] as usize;
                match work.names.push(&names[start..end]) {
                    Ok(name) => work.entries.push(DirectoryEntry {
                        parent_inode: parent,
                        child_inode: child,
                        name,
                    }),
                    Err(error) => {
                        arena_error = Some(error);
                        return false;
                    }
                }
            }
            true
        });
    if let Some(error) = arena_error {
        return Err(error.into());
    }
    if let Err(error) = result {
        if matches!(&error, ffi::NativeError::CallbackPanicked) {
            return Err(error.into());
        }
        work.warnings.directory_scan_errors += 1;
    }
    Ok(())
}

fn merge_dir_chunks(chunks: Vec<DirWork>) -> Result<DirectoryScan, Ext4Error> {
    let entry_count = chunks.iter().map(|chunk| chunk.entries.len()).sum();
    let name_bytes = chunks.iter().map(|chunk| chunk.names.len()).sum();
    let mut names = NameArena::with_capacity(name_bytes);
    let mut entries = Vec::with_capacity(entry_count);
    let mut warnings = ScanWarnings::default();

    for mut chunk in chunks {
        let name_offset = names.append(chunk.names)?;
        for entry in &mut chunk.entries {
            entry.name.offset = entry
                .name
                .offset
                .checked_add(name_offset)
                .ok_or(ArenaError::NamesTooLarge)?;
        }
        entries.append(&mut chunk.entries);
        warnings.directory_scan_errors = warnings
            .directory_scan_errors
            .saturating_add(chunk.warnings.directory_scan_errors);
    }

    // Canonical order: stably sorted by parent, preserving on-disk order
    // inside each directory. Serial scans already produce this order, so
    // parallel scheduling can never change tree shape or which hard link
    // owns the allocation.
    entries.sort_by_key(|entry| entry.parent_inode);

    let mut ranges = Vec::new();
    let mut index = 0;
    while index < entries.len() {
        let parent = entries[index].parent_inode;
        let mut end = index + 1;
        while end < entries.len() && entries[end].parent_inode == parent {
            end += 1;
        }
        ranges.push(DirRange {
            parent,
            start: index,
            end,
        });
        index = end;
    }

    Ok(DirectoryScan {
        names,
        entries,
        ranges,
        warnings,
    })
}

#[derive(Clone, Copy, Debug)]
struct InodeInfo {
    inode: u64,
    kind: NodeKind,
    logical_size: u64,
    allocated_size: u64,
    links: u32,
    mtime: i64,
}

impl From<ffi::Inode> for InodeInfo {
    fn from(raw: ffi::Inode) -> Self {
        let kind = match raw.kind {
            0 => NodeKind::File,
            1 => NodeKind::Directory,
            2 => NodeKind::Symlink,
            _ => NodeKind::Other,
        };
        Self {
            inode: raw.inode,
            kind,
            logical_size: raw.logical_size,
            allocated_size: raw.allocated_size,
            links: raw.links,
            mtime: raw.mtime,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct DirectoryEntry {
    parent_inode: u64,
    child_inode: u64,
    name: NameRef,
}

fn build_tree(
    inodes: Vec<InodeInfo>,
    mut entries: Vec<DirectoryEntry>,
    directory_ranges: Vec<DirRange>,
    names: NameArena,
    root_name: NameRef,
) -> Result<(NodeArena, ScanWarnings, u64), Ext4Error> {
    debug_assert!(
        inodes.windows(2).all(|pair| pair[0].inode < pair[1].inode),
        "inodes must be sorted by inode number"
    );
    let root_index = inodes
        .binary_search_by_key(&ROOT_INODE, |inode| inode.inode)
        .map_err(|_| Ext4Error::MissingRoot)?;
    let root = inodes[root_index];
    if root.kind != NodeKind::Directory {
        return Err(Ext4Error::RootNotDirectory);
    }

    const MISSING_INODE_INDEX: u64 = u64::MAX;
    let mut warnings = ScanWarnings::default();
    for entry in &mut entries {
        match inodes.binary_search_by_key(&entry.child_inode, |inode| inode.inode) {
            Ok(index) => entry.child_inode = index as u64,
            Err(_) => {
                entry.child_inode = MISSING_INODE_INDEX;
                warnings.missing_inode_references += 1;
            }
        }
    }

    let root_node = Node {
        parent: NodeId::ROOT,
        inode: root.inode,
        name: root_name,
        kind: NodeKind::Directory,
        logical_size: root.logical_size,
        allocated_size: root.allocated_size,
        links: root.links,
        mtime: root.mtime,
    };
    let mut nodes = Vec::with_capacity(entries.len().saturating_add(1));
    nodes.push(root_node);
    let mut arena = NodeArena::from_parts(nodes, names);
    let mut seen_directories = vec![false; inodes.len()];
    seen_directories[root_index] = true;
    let mut attributed_inodes = vec![false; inodes.len()];
    attributed_inodes[root_index] = true;
    let mut materialized_inodes = vec![false; inodes.len()];
    materialized_inodes[root_index] = true;
    let mut queue = VecDeque::from([(ROOT_INODE, NodeId::ROOT)]);
    let mut hard_link_entries = 0_u64;

    while let Some((parent_inode, parent_id)) = queue.pop_front() {
        let range = match directory_ranges.binary_search_by_key(&parent_inode, |range| range.parent)
        {
            Ok(position) => {
                let range = &directory_ranges[position];
                range.start..range.end
            }
            Err(_) => continue,
        };
        for entry in &entries[range] {
            debug_assert_eq!(entry.parent_inode, parent_inode);
            if entry.child_inode == MISSING_INODE_INDEX {
                continue;
            }
            let index = entry.child_inode as usize;
            let inode = inodes[index];
            if inode.kind == NodeKind::Directory && seen_directories[index] {
                warnings.directory_aliases += 1;
                continue;
            }

            let owns_allocation = !attributed_inodes[index];
            attributed_inodes[index] = true;
            if !owns_allocation {
                hard_link_entries += 1;
            }
            let node = Node {
                parent: parent_id,
                inode: inode.inode,
                name: entry.name,
                kind: inode.kind,
                logical_size: inode.logical_size,
                allocated_size: if owns_allocation {
                    inode.allocated_size
                } else {
                    0
                },
                links: inode.links,
                mtime: inode.mtime,
            };
            let id = arena.push_node(node)?;
            materialized_inodes[index] = true;
            if inode.kind == NodeKind::Directory {
                seen_directories[index] = true;
                queue.push_back((inode.inode, id));
            }
        }
    }

    warnings.nodes_without_parent = inodes
        .iter()
        .enumerate()
        .filter(|(index, _)| !materialized_inodes[*index])
        .count() as u64;
    Ok((arena, warnings, hard_link_entries))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inode(inode: u64, kind: NodeKind, size: u64, links: u32) -> InodeInfo {
        InodeInfo {
            inode,
            kind,
            logical_size: size,
            allocated_size: size,
            links,
            mtime: 0,
        }
    }

    #[test]
    fn builds_paths_and_attributes_a_hard_link_once() {
        let mut names = NameArena::default();
        let root_name = names.push(b"/").unwrap();
        let a = names.push(b"a").unwrap();
        let b = names.push(b"b").unwrap();
        let first = names.push(b"first").unwrap();
        let second = names.push(b"second").unwrap();
        let entries = vec![
            DirectoryEntry {
                parent_inode: 2,
                child_inode: 12,
                name: a,
            },
            DirectoryEntry {
                parent_inode: 2,
                child_inode: 13,
                name: b,
            },
            DirectoryEntry {
                parent_inode: 12,
                child_inode: 20,
                name: first,
            },
            DirectoryEntry {
                parent_inode: 13,
                child_inode: 20,
                name: second,
            },
        ];
        let ranges = vec![
            DirRange {
                parent: 2,
                start: 0,
                end: 2,
            },
            DirRange {
                parent: 12,
                start: 2,
                end: 3,
            },
            DirRange {
                parent: 13,
                start: 3,
                end: 4,
            },
        ];
        let inodes = vec![
            inode(2, NodeKind::Directory, 1, 2),
            inode(12, NodeKind::Directory, 1, 2),
            inode(13, NodeKind::Directory, 1, 2),
            inode(20, NodeKind::File, 100, 2),
        ];

        let (arena, warnings, hard_links) =
            build_tree(inodes, entries, ranges, names, root_name).unwrap();
        let scan = ScanResult::new(arena).unwrap();

        assert_eq!(scan.arena.display_path(NodeId(3)), "/a/first");
        assert_eq!(scan.arena.display_path(NodeId(4)), "/b/second");
        assert_eq!(scan.result_size(NodeId::ROOT), 103);
        assert_eq!(hard_links, 1);
        assert_eq!(warnings.total(), 0);
    }

    trait ResultSize {
        fn result_size(&self, id: NodeId) -> u64;
    }

    impl ResultSize for ScanResult {
        fn result_size(&self, id: NodeId) -> u64 {
            self.totals[id.index()].recursive_allocated
        }
    }
}
