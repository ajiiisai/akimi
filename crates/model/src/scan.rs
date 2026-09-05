use std::path::PathBuf;
use std::time::Duration;

use crate::ScanResult;

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
