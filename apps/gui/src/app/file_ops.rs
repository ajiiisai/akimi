use std::fmt;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, PathBuf};

use akimi_model::NodeKind;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DeleteMode {
    Trash,
    Permanent,
}

#[derive(Clone, Debug)]
pub(crate) struct DeleteTarget {
    mount_point: PathBuf,
    relative_path: PathBuf,
    expected_device: u64,
    expected_inode: u64,
    expected_kind: NodeKind,
}

impl DeleteTarget {
    pub(crate) fn new(
        mount_point: PathBuf,
        relative_path: PathBuf,
        expected_device: u64,
        expected_inode: u64,
        expected_kind: NodeKind,
    ) -> Result<Self, DeleteError> {
        if !mount_point.is_absolute() {
            return Err(DeleteError::new("the mount point is not absolute"));
        }
        if relative_path.as_os_str().is_empty()
            || relative_path
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(DeleteError::new("the selected path is not safe to delete"));
        }

        Ok(Self {
            mount_point,
            relative_path,
            expected_device,
            expected_inode,
            expected_kind,
        })
    }

    fn path(&self) -> PathBuf {
        self.mount_point.join(&self.relative_path)
    }
}

#[derive(Debug)]
pub(crate) struct DeleteError {
    message: String,
}

impl DeleteError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for DeleteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for DeleteError {}

pub(crate) fn delete(target: &DeleteTarget, mode: DeleteMode) -> Result<(), DeleteError> {
    let mount_metadata = fs::metadata(&target.mount_point)
        .map_err(|error| io_error("checking the mounted volume", error))?;
    if !mount_metadata.is_dir() || mount_metadata.dev() != target.expected_device {
        return Err(DeleteError::new(
            "the volume changed after the scan; scan it again before deleting",
        ));
    }

    let path = target.path();
    if path == target.mount_point {
        return Err(DeleteError::new("the volume root cannot be deleted"));
    }

    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| io_error("checking the selected item", error))?;
    if metadata.dev() != target.expected_device
        || metadata.ino() != target.expected_inode
        || metadata_kind(&metadata) != target.expected_kind
    {
        return Err(DeleteError::new(
            "the selected item changed after the scan; scan it again before deleting",
        ));
    }

    match mode {
        DeleteMode::Trash => trash::delete(&path)
            .map_err(|error| DeleteError::new(format!("moving the item to Trash: {error}"))),
        DeleteMode::Permanent if target.expected_kind == NodeKind::Directory => {
            fs::remove_dir_all(&path).map_err(|error| io_error("deleting the folder", error))
        }
        DeleteMode::Permanent => {
            fs::remove_file(&path).map_err(|error| io_error("deleting the file", error))
        }
    }
}

fn metadata_kind(metadata: &fs::Metadata) -> NodeKind {
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

fn io_error(action: &str, error: std::io::Error) -> DeleteError {
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        DeleteError::new(format!(
            "{action}: permission denied. Akimi does not have permission to delete this item"
        ))
    } else {
        DeleteError::new(format!("{action}: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::{delete, DeleteMode, DeleteTarget};
    use akimi_model::NodeKind;
    use std::fs;
    use std::os::unix::fs::{symlink, MetadataExt};
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_directory(test: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "akimi-file-ops-{test}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn permanent_delete_removes_the_scanned_file() {
        let mount = temporary_directory("file");
        let path = mount.join("example.bin");
        fs::write(&path, b"data").unwrap();
        let mount_device = fs::metadata(&mount).unwrap().dev();
        let inode = fs::symlink_metadata(&path).unwrap().ino();
        let target = DeleteTarget::new(
            mount.clone(),
            PathBuf::from("example.bin"),
            mount_device,
            inode,
            NodeKind::File,
        )
        .unwrap();

        delete(&target, DeleteMode::Permanent).unwrap();

        assert!(!path.exists());
        fs::remove_dir(mount).unwrap();
    }

    #[test]
    fn permanent_delete_unlinks_a_symlink_without_touching_its_target() {
        let mount = temporary_directory("symlink");
        let destination = mount.join("destination");
        let link = mount.join("link");
        fs::write(&destination, b"keep me").unwrap();
        symlink(&destination, &link).unwrap();
        let mount_device = fs::metadata(&mount).unwrap().dev();
        let inode = fs::symlink_metadata(&link).unwrap().ino();
        let target = DeleteTarget::new(
            mount.clone(),
            PathBuf::from("link"),
            mount_device,
            inode,
            NodeKind::Symlink,
        )
        .unwrap();

        delete(&target, DeleteMode::Permanent).unwrap();

        assert!(!link.exists());
        assert!(destination.exists());
        fs::remove_file(destination).unwrap();
        fs::remove_dir(mount).unwrap();
    }

    #[test]
    fn rejects_a_stale_inode() {
        let mount = temporary_directory("stale");
        let path = mount.join("example.bin");
        fs::write(&path, b"data").unwrap();
        let mount_device = fs::metadata(&mount).unwrap().dev();
        let inode = fs::symlink_metadata(&path).unwrap().ino();
        let target = DeleteTarget::new(
            mount.clone(),
            PathBuf::from("example.bin"),
            mount_device,
            inode.saturating_add(1),
            NodeKind::File,
        )
        .unwrap();

        let error = delete(&target, DeleteMode::Permanent).unwrap_err();

        assert!(error.to_string().contains("changed after the scan"));
        assert!(path.exists());
        fs::remove_file(path).unwrap();
        fs::remove_dir(mount).unwrap();
    }

    #[test]
    fn rejects_paths_that_can_escape_the_mount() {
        let mount = temporary_directory("escape");
        let result = DeleteTarget::new(
            mount.clone(),
            PathBuf::from("../outside"),
            fs::metadata(&mount).unwrap().dev(),
            1,
            NodeKind::File,
        );

        assert!(result.is_err());
        fs::remove_dir(mount).unwrap();
    }
}
