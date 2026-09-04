use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub(crate) struct Volume {
    pub(crate) device: PathBuf,
    pub(crate) mount_point: Option<PathBuf>,
}

pub(crate) fn discover() -> Vec<Volume> {
    let mut volumes = Vec::new();
    let mut devices = HashSet::new();

    if let Ok(mounts) = fs::read_to_string("/proc/self/mounts") {
        for line in mounts.lines() {
            let mut fields = line.split_whitespace();
            let (Some(device), Some(mount_point), Some(filesystem)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            if filesystem != "ext4" {
                continue;
            }

            let device = PathBuf::from(decode_mount_field(device));
            if devices.insert(device.clone()) {
                volumes.push(Volume {
                    device,
                    mount_point: Some(PathBuf::from(decode_mount_field(mount_point))),
                });
            }
        }
    }

    if let Some(argument) = env::args_os().nth(1).map(PathBuf::from) {
        if devices.insert(argument.clone()) {
            volumes.insert(
                0,
                Volume {
                    device: argument,
                    mount_point: None,
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
