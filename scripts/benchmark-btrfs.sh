#!/usr/bin/env bash
set -euo pipefail

image=${AKIMI_BTRFS_IMAGE:-/tmp/akimi-btrfs-benchmark.img}
mount_point=${AKIMI_BTRFS_MOUNT:-/tmp/akimi-btrfs-benchmark}
file_count=${AKIMI_BTRFS_FILES:-10000}
binary=${AKIMI_BINARY:-target/release/akimi}

if [[ ! -x "$binary" ]]; then
    echo "building $binary" >&2
    nix develop --command cargo build --release -p akimi
fi

mkdir -p "$mount_point"
mounted_here=false
cleanup() {
    if [[ "$mounted_here" == true ]]; then
        sudo umount "$mount_point"
    fi
}
trap cleanup EXIT

if [[ ! -f "$image" ]]; then
    truncate -s 2G "$image"
    sudo mkfs.btrfs -f "$image"
fi

if ! mountpoint -q "$mount_point"; then
    sudo mount -o loop "$image" "$mount_point"
    mounted_here=true
fi

if [[ ! -e "$mount_point/.akimi-benchmark-ready" ]]; then
    mkdir -p "$mount_point/data"
    for ((index = 0; index < file_count; index++)); do
        printf '%08d\n' "$index" > "$mount_point/data/file-$index.txt"
    done
    ln "$mount_point/data/file-0.txt" "$mount_point/data/hardlink.txt"
    cp --reflink=always "$mount_point/data/file-1.txt" "$mount_point/data/reflink.txt"
    touch "$mount_point/.akimi-benchmark-ready"
fi

sync
echo "Benchmarking $binary against $mount_point"
hyperfine --warmup 2 --runs 8 \
    "$binary $mount_point --files --dirs --json > /dev/null"

echo
echo "btrfs reference accounting"
sudo btrfs filesystem du -s "$mount_point"
