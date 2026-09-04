use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub(crate) struct Volume {
    pub(crate) device: PathBuf,
    pub(crate) mount_point: Option<PathBuf>,
    pub(crate) scan_path: Option<PathBuf>,
    pub(crate) filesystem: String,
}

pub(crate) fn discover() -> Vec<Volume> {
    let mut volumes = Vec::new();
    let mut mounts = HashSet::new();

    if let Ok(mount_table) = fs::read_to_string("/proc/self/mounts") {
        for line in mount_table.lines() {
            let mut fields = line.split_whitespace();
            let (Some(device), Some(mount_point), Some(filesystem)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            if !matches!(filesystem, "ext4" | "btrfs") {
                continue;
            }

            let device = PathBuf::from(decode_mount_field(device));
            let mount_point = PathBuf::from(decode_mount_field(mount_point));
            if mounts.insert((device.clone(), mount_point.clone())) {
                let scan_path = (filesystem == "btrfs").then(|| mount_point.clone());
                volumes.push(Volume {
                    device,
                    mount_point: Some(mount_point),
                    scan_path,
                    filesystem: filesystem.to_owned(),
                });
            }
        }
    }

    if let Some(argument) = env::args_os().nth(1).map(PathBuf::from) {
        if !volumes.iter().any(|volume| volume.device == argument) {
            volumes.insert(
                0,
                Volume {
                    device: argument,
                    mount_point: None,
                    scan_path: None,
                    filesystem: "unknown".to_owned(),
                },
            );
        }
    }

    volumes.sort_by(|left, right| {
        let left_is_argument = left.mount_point.is_none();
        let right_is_argument = right.mount_point.is_none();
        right_is_argument
            .cmp(&left_is_argument)
            .then_with(|| left.mount_point.cmp(&right.mount_point))
    });
    volumes
}

fn decode_mount_field(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' && index + 3 < bytes.len() {
            let digits = &bytes[index + 1..index + 4];
            if digits.iter().all(|byte| (b'0'..=b'7').contains(byte)) {
                decoded.push((digits[0] - b'0') * 64 + (digits[1] - b'0') * 8 + (digits[2] - b'0'));
                index += 4;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

#[cfg(test)]
mod tests {
    use super::decode_mount_field;

    #[test]
    fn decodes_proc_mount_escapes() {
        assert_eq!(decode_mount_field("/media/My\\040Drive"), "/media/My Drive");
    }
}
