mod btrfs;

use std::fs::File;
use std::path::{Path, PathBuf};

use akimi_ext4::{Ext4Error, Ext4Filesystem};
use akimi_model::{FilesystemInfo, FilesystemScan};
use thiserror::Error;

pub use btrfs::BtrfsFilesystem;

#[derive(Debug, Error)]
pub enum FilesystemError {
    #[error(transparent)]
    Ext4(#[from] Ext4Error),
    #[error(transparent)]
    Btrfs(#[from] btrfs::BtrfsError),
    #[error("{path} is not a supported filesystem path")]
    UnsupportedPath { path: PathBuf },
}

impl FilesystemError {
    pub fn is_permission_denied(&self) -> bool {
        match self {
            Self::Ext4(error) => error.is_permission_denied(),
            Self::Btrfs(error) => error.is_permission_denied(),
            Self::UnsupportedPath { .. } => false,
        }
    }
}

pub enum Filesystem {
    Ext4(Ext4Filesystem),
    Btrfs(BtrfsFilesystem),
}

impl Filesystem {
    pub fn is_btrfs(&self) -> bool {
        matches!(self, Self::Btrfs(_))
    }

    pub fn open(path: &Path) -> Result<Self, FilesystemError> {
        if path.is_dir() {
            if BtrfsFilesystem::is_btrfs_path(path) {
                return Ok(Self::Btrfs(BtrfsFilesystem::open(path)?));
            }
            return Err(FilesystemError::UnsupportedPath {
                path: path.to_path_buf(),
            });
        }
        if BtrfsFilesystem::is_btrfs_path(path) {
            return Ok(Self::Btrfs(BtrfsFilesystem::open(path)?));
        }
        Ok(Self::Ext4(Ext4Filesystem::open(path)?))
    }

    pub fn open_descriptor(path: &Path, descriptor: File) -> Result<Self, FilesystemError> {
        Ok(Self::Ext4(Ext4Filesystem::open_descriptor(
            path, descriptor,
        )?))
    }

    pub fn info(&self) -> &FilesystemInfo {
        match self {
            Self::Ext4(filesystem) => filesystem.info(),
            Self::Btrfs(filesystem) => filesystem.info(),
        }
    }

    pub fn scan_with_threads(&mut self, threads: usize) -> Result<FilesystemScan, FilesystemError> {
        match self {
            Self::Ext4(filesystem) => Ok(filesystem.scan_with_threads(threads)?),
            Self::Btrfs(filesystem) => Ok(filesystem.scan_with_threads(threads)?),
        }
    }
}
