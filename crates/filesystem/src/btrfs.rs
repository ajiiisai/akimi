use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use akimi_model::{FilesystemInfo, FilesystemScan};
use thiserror::Error;

mod direct;
mod naive;

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
    #[error("direct btrfs reader: {0}")]
    Direct(String),
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
        is_btrfs(path) || is_btrfs_source(path)
    }

    pub fn open(root: &Path) -> Result<Self, BtrfsError> {
        let started = Instant::now();
        let metadata = fs::metadata(root).map_err(|source| BtrfsError::InspectPath {
            path: root.to_path_buf(),
            source,
        })?;
        if metadata.is_dir() && !is_btrfs(root) {
            return Err(BtrfsError::RequiresMountedDirectory(root.to_path_buf()));
        }
        if !metadata.is_dir() && !is_btrfs_source(root) {
            return Err(BtrfsError::RequiresMountedDirectory(root.to_path_buf()));
        }

        Ok(Self {
            root: root.to_path_buf(),
            info: FilesystemInfo {
                device: root.to_path_buf(),
                filesystem_type: "btrfs",
                block_size: if metadata.is_dir() {
                    metadata.blksize().max(1) as u32
                } else {
                    4096
                },
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
        if std::env::var_os("AKIMI_BTRFS_SCANNER").as_deref() == Some(std::ffi::OsStr::new("naive"))
        {
            naive::scan(&self.root, &mut self.info, self.open_time)
        } else {
            direct::scan(&self.root, &mut self.info, self.open_time)
        }
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

fn is_btrfs_source(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() && !metadata.file_type().is_block_device() {
        return false;
    }
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    if file.seek(SeekFrom::Start(0x10040)).is_err() {
        return false;
    }
    let mut magic = [0u8; 8];
    file.read_exact(&mut magic).is_ok() && magic == *b"_BHRfS_M"
}
