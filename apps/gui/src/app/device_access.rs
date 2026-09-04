use std::collections::HashMap;
use std::fs::File;
use std::os::fd::OwnedFd as StdOwnedFd;
use std::os::unix::fs::FileTypeExt;
use std::path::Path;

use akimi_ext4::Ext4Filesystem;
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedFd, OwnedObjectPath, Value};

const UDISKS_DESTINATION: &str = "org.freedesktop.UDisks2";
const UDISKS_MANAGER_PATH: &str = "/org/freedesktop/UDisks2/Manager";
const UDISKS_MANAGER_INTERFACE: &str = "org.freedesktop.UDisks2.Manager";
const UDISKS_BLOCK_INTERFACE: &str = "org.freedesktop.UDisks2.Block";

/// Opens a volume without broadening the user's permissions. A normal open is
/// attempted first. If the kernel denies it, UDisks asks polkit for one
/// read-only descriptor whose lifetime is limited to this scan.
pub(crate) fn open_for_scan(device: &Path) -> Result<Ext4Filesystem, String> {
    match Ext4Filesystem::open(device) {
        Ok(filesystem) => Ok(filesystem),
        Err(error) if error.is_permission_denied() && is_block_device(device) => {
            let descriptor = request_read_descriptor(device)?;
            Ext4Filesystem::open_descriptor(device, descriptor).map_err(|error| error.to_string())
        }
        Err(error) => Err(error.to_string()),
    }
}

fn is_block_device(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|metadata| metadata.file_type().is_block_device())
}

fn request_read_descriptor(device: &Path) -> Result<File, String> {
    let device_path = device.to_str().ok_or_else(|| {
        format!(
            "{} cannot be authorized because its path is not valid UTF-8",
            device.display()
        )
    })?;
    let connection = Connection::system().map_err(|error| {
        format!("Could not connect to the system authorization service: {error}")
    })?;

    let manager = Proxy::new(
        &connection,
        UDISKS_DESTINATION,
        UDISKS_MANAGER_PATH,
        UDISKS_MANAGER_INTERFACE,
    )
    .map_err(|error| format!("Could not contact UDisks2: {error}"))?;

    let mut device_spec = HashMap::<&str, Value<'_>>::new();
    device_spec.insert("path", Value::from(device_path));
    let options = HashMap::<&str, Value<'_>>::new();
    let objects: Vec<OwnedObjectPath> = manager
        .call("ResolveDevice", &(device_spec, options))
        .map_err(|error| describe_udisks_error("Could not look up the block device", error))?;
    let object = objects.first().ok_or_else(|| {
        format!(
            "UDisks2 does not know the block device {}",
            device.display()
        )
    })?;

    let block = Proxy::new(
        &connection,
        UDISKS_DESTINATION,
        object.as_str(),
        UDISKS_BLOCK_INTERFACE,
    )
    .map_err(|error| format!("Could not prepare the read-access request: {error}"))?;
    let options = HashMap::<&str, Value<'_>>::new();
    let descriptor: OwnedFd = block
        .call("OpenDevice", &("r", options))
        .map_err(describe_authorization_error)?;
    let descriptor: StdOwnedFd = descriptor.into();
    Ok(File::from(descriptor))
}

fn describe_authorization_error(error: zbus::Error) -> String {
    if let zbus::Error::MethodError(name, _, _) = &error {
        let name = name.as_str();
        if name.ends_with(".NotAuthorizedDismissed") {
            return "Read access was cancelled. The volume was not scanned.".into();
        }
        if name.contains(".NotAuthorized") || name.ends_with(".AccessDenied") {
            return "Read access was not authorized. If no password dialog appeared, start your desktop's polkit authentication agent.".into();
        }
    }
    describe_udisks_error("Could not obtain read-only access", error)
}

fn describe_udisks_error(action: &str, error: zbus::Error) -> String {
    if let zbus::Error::MethodError(name, _, _) = &error {
        let name = name.as_str();
        if name.ends_with(".ServiceUnknown") || name.ends_with(".NameHasNoOwner") {
            return "UDisks2 is not running. Enable the UDisks2 service, then try again.".into();
        }
    }
    format!("{action} through UDisks2: {error}")
}
