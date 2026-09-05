use std::collections::HashMap;
use std::fs::File;
use std::os::fd::OwnedFd as StdOwnedFd;
use std::os::unix::fs::FileTypeExt;
use std::path::Path;
use std::process::Command;

use akimi_filesystem::Filesystem;
use akimi_model::FilesystemScan;
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedFd, OwnedObjectPath, Value};

const UDISKS_DESTINATION: &str = "org.freedesktop.UDisks2";
const UDISKS_MANAGER_PATH: &str = "/org/freedesktop/UDisks2/Manager";
const UDISKS_MANAGER_INTERFACE: &str = "org.freedesktop.UDisks2.Manager";
const UDISKS_BLOCK_INTERFACE: &str = "org.freedesktop.UDisks2.Block";

/// Opens a volume without broadening the user's permissions. A normal open is
/// attempted first. If the kernel denies it, UDisks asks polkit for one
/// read-only descriptor whose lifetime is limited to this scan.
pub(crate) fn open_for_scan(device: &Path) -> Result<Filesystem, String> {
    match Filesystem::open(device) {
        Ok(filesystem) => Ok(filesystem),
        Err(error) if error.is_permission_denied() && is_block_device(device) => {
            let descriptor = request_read_descriptor(device)?;
            Filesystem::open_descriptor(device, descriptor).map_err(|error| error.to_string())
        }
        Err(error) => Err(error.to_string()),
    }
}

pub(crate) fn scan_btrfs_with_helper(device: &Path) -> Result<FilesystemScan, String> {
    let helper = std::env::var_os("AKIMI_BTRFS_HELPER")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::current_exe()
                .ok()?
                .parent()
                .map(|path| path.join("akimi-btrfs-helper"))
        })
        .ok_or_else(|| "could not locate the btrfs privilege helper".to_string())?;
    let output = Command::new("pkexec")
        .arg(helper)
        .arg(device)
        .output()
        .map_err(|error| format!("could not start the btrfs privilege helper: {error}"))?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if message.is_empty() {
            format!("btrfs authorization failed ({})", output.status)
        } else {
            message
        });
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("btrfs helper returned invalid scan data: {error}"))
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
            return "Read access was not authorized. Polkit needs a graphical authentication agent, which niri does not start by itself. Start polkit-kde-authentication-agent-1, polkit-gnome-authentication-agent-1, lxqt-policykit-agent, or another polkit agent, then retry.".into();
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
