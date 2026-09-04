# Akimi

Akimi is a disk usage analyzer for Linux filesystems. It reads ext4 metadata directly through `libext2fs` and has a path-based btrfs proof of concept.

The name Akimi (空き見) plays on 空き (aki), meaning free or unused space, and 見 (mi), meaning seeing or looking. Put together, it’s roughly “looking at free space.”

Akimi is experimental. Scans of mounted filesystems are best-effort views rather than atomic snapshots.

## Features

- Interactive treemap and size-sorted directory tree
- Fast, parallel metadata scanning
- CLI output in human-readable or JSON form
- Allocated and logical size accounting
- Correct physical accounting for sparse files and hard links
- Trash and permanent deletion from the GUI
- Read-only access to filesystem metadata

## Installation

Akimi is not packaged yet. Build it from source with Rust, a C compiler, `pkg-config`, and e2fsprogs development files that provide `ext2fs.pc` and `com_err.pc`. Akimi requires `libext2fs` 1.47 or newer.

On NixOS:

```bash
nix develop
cargo build --release
```

On other Linux distributions, install the equivalent development dependencies, then run:

```bash
cargo build --release
```

The resulting executables are `target/release/akimi-gui` and `target/release/akimi`.

## Usage

### Graphical interface

Start the application and select a mounted Linux volume:

```bash
./target/release/akimi-gui
```

You can also open an ext4 device or filesystem image directly:

```bash
./target/release/akimi-gui /dev/nvme0n1p2
```

When a block device is not readable by the current user, the GUI requests temporary read-only access through UDisks2 and polkit. Deleting files still uses the current user's normal permissions and is available only for mounted filesystems.

### Command line

Scan an ext4 block device or a mounted btrfs directory:

```bash
sudo ./target/release/akimi /dev/nvme0n1p2
```

Regular filesystem images do not normally require elevated permissions:

```bash
./target/release/akimi disk.img

# btrfs proof of concept
./target/release/akimi /mnt/data
```

Useful options include:

```bash
akimi DEVICE --top 50
akimi DEVICE --files
akimi DEVICE --files --dirs
akimi DEVICE --json
akimi DEVICE --threads 4
```

Run `akimi --help` for the complete command reference.

## Size accounting

Rankings use allocated size because it represents physical disk usage. Sparse files can therefore be smaller than their logical size. Hard-linked data is assigned to one path so its allocation is counted once, while every link remains visible. Akimi does not follow symbolic links.

## Limitations

- Linux only. The btrfs proof of concept accepts mounted btrfs directories; raw btrfs devices and images are not supported yet.
- Ext4 devices and images still use the direct metadata scanner.
- Mounted filesystems can change during a scan, causing small inconsistencies.
- Filesystem metadata, journals, and reserved blocks are not attributed to file-tree entries.
- Btrfs currently uses a serial directory walker and approximate allocated-size accounting.

## Development

Run the repository checks with:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets
cargo test --workspace
```

The workspace contains the GUI in `apps/gui`, the CLI in `apps/cli`, the ext4 scanner in `crates/ext4`, and the shared data model in `crates/model`.

## License

Akimi is available under the MIT License.
