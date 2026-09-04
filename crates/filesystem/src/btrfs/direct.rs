use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use akimi_ext4::{FilesystemInfo, FilesystemScan, ScanStats, ScanTimings, ScanWarnings};
use akimi_model::{NameArena, Node, NodeArena, NodeId, NodeKind, ScanResult};
use thiserror::Error;

const BTRFS_FIRST_FREE_OBJECTID: u64 = 256;
const INODE_ITEM_KEY: u32 = 1;
const INODE_REF_KEY: u32 = 12;
const INODE_EXTREF_KEY: u32 = 13;
const DIR_ITEM_KEY: u32 = 84;
const DIR_INDEX_KEY: u32 = 96;
const SEARCH_BUFFER_SIZE: usize = 1024 * 1024;
const SEARCH_HEADER_SIZE: usize = 32;
const INODE_ITEM_SIZE: usize = 176;
const DIR_ITEM_SIZE: usize = 30;
const FILE_TYPE_MASK: u32 = 0o170000;
const FILE_TYPE_REGULAR: u32 = 0o100000;
const FILE_TYPE_DIRECTORY: u32 = 0o040000;
const FILE_TYPE_SYMLINK: u32 = 0o120000;

unsafe extern "C" {
    fn akimi_btrfs_tree_search(fd: i32, args: *mut SearchArgs) -> i32;
}

#[derive(Debug, Error)]
pub enum DirectError {
    #[error("cannot open btrfs mount {path}: {source}")]
    Open {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("btrfs tree search failed: {0}")]
    Io(#[source] std::io::Error),
    #[error("invalid btrfs tree item")]
    InvalidItem,
    #[error(transparent)]
    Arena(#[from] akimi_model::ArenaError),
    #[error(transparent)]
    Aggregate(#[from] akimi_model::AggregateError),
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SearchKey {
    tree_id: u64,
    min_objectid: u64,
    max_objectid: u64,
    min_offset: u64,
    max_offset: u64,
    min_transid: u64,
    max_transid: u64,
    min_type: u32,
    max_type: u32,
    nr_items: u32,
    unused: u32,
    unused1: u64,
    unused2: u64,
    unused3: u64,
    unused4: u64,
}

#[repr(C)]
struct SearchArgs {
    key: SearchKey,
    buf_size: u64,
    buf: [u64; SEARCH_BUFFER_SIZE / 8],
}

#[derive(Clone, Copy)]
struct InodeMeta {
    inode: u64,
    kind: NodeKind,
    logical_size: u64,
    allocated_size: u64,
    links: u32,
    mtime: i64,
}

#[derive(Clone)]
struct DirectoryEntry {
    parent: u64,
    child: u64,
    name: Vec<u8>,
}

pub fn scan(
    root: &Path,
    info: &mut FilesystemInfo,
    open_time: Duration,
) -> Result<FilesystemScan, super::BtrfsError> {
    scan_inner(root, info, open_time).map_err(|error| match error {
        DirectError::Open { path, source } => super::BtrfsError::InspectPath { path, source },
        DirectError::Io(source) if source.kind() == std::io::ErrorKind::PermissionDenied => {
            super::BtrfsError::InspectPath {
                path: root.to_path_buf(),
                source,
            }
        }
        other => super::BtrfsError::Direct(other.to_string()),
    })
}

fn scan_inner(
    root: &Path,
    info: &mut FilesystemInfo,
    open_time: Duration,
) -> Result<FilesystemScan, DirectError> {
    let file = File::open(root).map_err(|source| DirectError::Open {
        path: root.to_path_buf(),
        source,
    })?;
    let search_started = Instant::now();
    let (inodes, entries) = search_tree(file.as_raw_fd())?;
    let inode_scan = search_started.elapsed();
    let mut names = NameArena::default();
    let root_name = names.push(b"/")?;
    let root_meta = inodes
        .get(&BTRFS_FIRST_FREE_OBJECTID)
        .copied()
        .ok_or(DirectError::InvalidItem)?;
    if root_meta.kind != NodeKind::Directory {
        return Err(DirectError::InvalidItem);
    }

    let mut nodes = vec![Node {
        parent: NodeId::ROOT,
        inode: root_meta.inode,
        name: root_name,
        kind: root_meta.kind,
        logical_size: root_meta.logical_size,
        allocated_size: root_meta.allocated_size,
        links: root_meta.links,
        mtime: root_meta.mtime,
    }];
    let mut queue = VecDeque::from([(BTRFS_FIRST_FREE_OBJECTID, NodeId::ROOT)]);
    let mut seen = HashMap::from([(BTRFS_FIRST_FREE_OBJECTID, NodeId::ROOT)]);
    let mut children = HashMap::<u64, Vec<DirectoryEntry>>::new();
    for entry in entries {
        children.entry(entry.parent).or_default().push(entry);
    }
    let mut warnings = ScanWarnings::default();
    let mut stats = ScanStats {
        directories: 1,
        nodes: 1,
        ..ScanStats::default()
    };

    while let Some((parent_inode, parent_id)) = queue.pop_front() {
        let Some(entries) = children.remove(&parent_inode) else {
            continue;
        };
        for entry in entries {
            if entry.name == b"." || entry.name == b".." {
                continue;
            }
            let Some(meta) = inodes.get(&entry.child).copied() else {
                warnings.missing_inode_references += 1;
                continue;
            };
            let owns_allocation = !seen.contains_key(&entry.child);
            if owns_allocation {
                seen.insert(entry.child, NodeId(nodes.len() as u32));
            }
            let name = names.push(&entry.name)?;
            nodes.push(Node {
                parent: parent_id,
                inode: meta.inode,
                name,
                kind: meta.kind,
                logical_size: meta.logical_size,
                allocated_size: owns_allocation.then_some(meta.allocated_size).unwrap_or(0),
                links: meta.links,
                mtime: meta.mtime,
            });
            stats.directory_entries += 1;
            stats.nodes += 1;
            match meta.kind {
                NodeKind::File => stats.files += 1,
                NodeKind::Directory => {
                    stats.directories += 1;
                    if owns_allocation {
                        queue.push_back((entry.child, NodeId((nodes.len() - 1) as u32)));
                    } else {
                        warnings.directory_aliases += 1;
                    }
                }
                NodeKind::Symlink => stats.symlinks += 1,
                NodeKind::Other => stats.other += 1,
            }
            if !owns_allocation {
                stats.hard_link_entries += 1;
            }
        }
    }

    stats.allocated_inodes = seen.len() as u64;
    info.inode_count = seen.len() as u64;
    info.reported_allocated_inodes = seen.len() as u64;
    let tree_started = Instant::now();
    let result = ScanResult::new(NodeArena::from_parts(nodes, names))?;
    let tree_build = tree_started.elapsed();
    Ok(FilesystemScan {
        result,
        stats,
        warnings,
        workers: 1,
        timings: ScanTimings {
            open: open_time,
            inode_scan,
            directory_scan: Duration::ZERO,
            tree_build,
            aggregation: Duration::ZERO,
        },
    })
}

fn search_tree(fd: i32) -> Result<(HashMap<u64, InodeMeta>, Vec<DirectoryEntry>), DirectError> {
    let mut inodes = HashMap::new();
    let mut entries = Vec::new();
    let mut key = SearchKey {
        tree_id: 0,
        min_objectid: 0,
        max_objectid: u64::MAX,
        min_offset: 0,
        max_offset: u64::MAX,
        min_transid: 0,
        max_transid: u64::MAX,
        min_type: 0,
        max_type: u32::MAX,
        nr_items: u32::MAX,
        ..SearchKey::default()
    };

    loop {
        let mut args = SearchArgs {
            key,
            buf_size: SEARCH_BUFFER_SIZE as u64,
            buf: [0; SEARCH_BUFFER_SIZE / 8],
        };
        let error = unsafe { akimi_btrfs_tree_search(fd, &mut args) };
        if error != 0 {
            return Err(DirectError::Io(std::io::Error::from_raw_os_error(error)));
        }
        let count = args.key.nr_items as usize;
        if count == 0 {
            break;
        }
        let buffer = unsafe {
            std::slice::from_raw_parts(args.buf.as_ptr().cast::<u8>(), SEARCH_BUFFER_SIZE)
        };
        let mut cursor = 0;
        let mut last = None;
        for _ in 0..count {
            if cursor + SEARCH_HEADER_SIZE > buffer.len() {
                return Err(DirectError::InvalidItem);
            }
            let objectid = read_u64(buffer, cursor + 8)?;
            let offset = read_u64(buffer, cursor + 16)?;
            let item_type = read_u32(buffer, cursor + 24)?;
            let length = read_u32(buffer, cursor + 28)? as usize;
            cursor += SEARCH_HEADER_SIZE;
            if cursor + length > buffer.len() {
                return Err(DirectError::InvalidItem);
            }
            let item = &buffer[cursor..cursor + length];
            match item_type {
                INODE_ITEM_KEY => parse_inode(objectid, item, &mut inodes)?,
                DIR_ITEM_KEY | DIR_INDEX_KEY => {
                    parse_directory_items(objectid, item, &mut entries)?
                }
                INODE_REF_KEY | INODE_EXTREF_KEY => {}
                _ => {}
            }
            cursor += length;
            last = Some((objectid, item_type, offset));
        }
        let Some((objectid, item_type, offset)) = last else {
            break;
        };
        key.min_objectid = objectid;
        key.min_type = item_type;
        key.min_offset = offset.checked_add(1).ok_or(DirectError::InvalidItem)?;
    }
    Ok((inodes, entries))
}

fn parse_inode(
    inode: u64,
    item: &[u8],
    inodes: &mut HashMap<u64, InodeMeta>,
) -> Result<(), DirectError> {
    if item.len() < INODE_ITEM_SIZE {
        return Err(DirectError::InvalidItem);
    }
    let mode = read_u32(item, 52)?;
    let kind = match mode & FILE_TYPE_MASK {
        FILE_TYPE_REGULAR => NodeKind::File,
        FILE_TYPE_DIRECTORY => NodeKind::Directory,
        FILE_TYPE_SYMLINK => NodeKind::Symlink,
        _ => NodeKind::Other,
    };
    inodes.insert(
        inode,
        InodeMeta {
            inode,
            kind,
            logical_size: read_u64(item, 16)?,
            allocated_size: read_u64(item, 24)?,
            links: read_u32(item, 40)?,
            mtime: read_i64(item, 144)?,
        },
    );
    Ok(())
}

fn parse_directory_items(
    parent: u64,
    mut item: &[u8],
    entries: &mut Vec<DirectoryEntry>,
) -> Result<(), DirectError> {
    while !item.is_empty() {
        if item.len() < DIR_ITEM_SIZE {
            return Err(DirectError::InvalidItem);
        }
        let child = read_u64(item, 0)?;
        let data_len = read_u16(item, 25)? as usize;
        let name_len = read_u16(item, 27)? as usize;
        let length = DIR_ITEM_SIZE
            .checked_add(data_len)
            .and_then(|length| length.checked_add(name_len))
            .ok_or(DirectError::InvalidItem)?;
        if length > item.len() {
            return Err(DirectError::InvalidItem);
        }
        entries.push(DirectoryEntry {
            parent,
            child,
            name: item[DIR_ITEM_SIZE + data_len..length].to_vec(),
        });
        item = &item[length..];
    }
    Ok(())
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, DirectError> {
    let bytes = bytes
        .get(offset..offset + 2)
        .ok_or(DirectError::InvalidItem)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, DirectError> {
    let bytes = bytes
        .get(offset..offset + 4)
        .ok_or(DirectError::InvalidItem)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, DirectError> {
    let bytes = bytes
        .get(offset..offset + 8)
        .ok_or(DirectError::InvalidItem)?;
    Ok(u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

fn read_i64(bytes: &[u8], offset: usize) -> Result<i64, DirectError> {
    Ok(read_u64(bytes, offset)? as i64)
}
