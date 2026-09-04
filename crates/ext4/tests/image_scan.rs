use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use akimi_ext4::Ext4Filesystem;
use akimi_model::{NodeId, NodeKind};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn create() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("akimi-image-test-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn scans_an_ext4_image_without_mounting_it() {
    if Command::new("mke2fs").arg("-V").output().is_err() {
        eprintln!("skipping image test because mke2fs is unavailable");
        return;
    }

    let temp = TestDirectory::create();
    let source = temp.0.join("source");
    fs::create_dir_all(source.join("foo/bar")).unwrap();
    fs::create_dir_all(source.join("links")).unwrap();

    let payload = source.join("foo/bar/payload.bin");
    let mut file = File::create(&payload).unwrap();
    file.write_all(&vec![0x5a; 128 * 1024]).unwrap();
    fs::hard_link(&payload, source.join("links/payload-hardlink.bin")).unwrap();

    let sparse = File::create(source.join("foo/sparse.bin")).unwrap();
    sparse.set_len(8 * 1024 * 1024).unwrap();

    let image = temp.0.join("filesystem.img");
    let status = Command::new("mke2fs")
        .args(["-q", "-t", "ext4", "-d"])
        .arg(&source)
        .arg(&image)
        .arg("32768")
        .status()
        .unwrap();
    assert!(status.success());

    let mut filesystem = Ext4Filesystem::open(Path::new(&image)).unwrap();
    let scan = filesystem.scan().unwrap();
    let find = |path: &str| {
        scan.result
            .arena
            .nodes()
            .iter()
            .enumerate()
            .find_map(|(index, _)| {
                let id = NodeId(index as u32);
                (scan.result.arena.display_path(id) == path).then_some(id)
            })
            .unwrap_or_else(|| panic!("missing path {path}"))
    };

    let payload_id = find("/foo/bar/payload.bin");
    let hardlink_id = find("/links/payload-hardlink.bin");
    let sparse_id = find("/foo/sparse.bin");
    assert_eq!(
        scan.result.arena.nodes()[payload_id.index()].inode,
        scan.result.arena.nodes()[hardlink_id.index()].inode
    );
    assert_eq!(
        scan.result.arena.nodes()[payload_id.index()].allocated_size
            + scan.result.arena.nodes()[hardlink_id.index()].allocated_size,
        128 * 1024
    );
    let sparse_node = &scan.result.arena.nodes()[sparse_id.index()];
    assert_eq!(sparse_node.kind, NodeKind::File);
    assert_eq!(sparse_node.logical_size, 8 * 1024 * 1024);
    assert!(sparse_node.allocated_size < sparse_node.logical_size);
    assert_eq!(scan.stats.hard_link_entries, 1);
    assert_eq!(scan.warnings.total(), 0, "{:?}", scan.warnings);

    let mut parallel_filesystem = Ext4Filesystem::open(Path::new(&image)).unwrap();
    let parallel = parallel_filesystem.scan_with_threads(2).unwrap();
    assert_eq!(parallel.workers, 2);
    assert_eq!(parallel.stats, scan.stats);
    assert_eq!(parallel.warnings, scan.warnings);
    assert_eq!(parallel.result.arena.nodes(), scan.result.arena.nodes());
    assert_eq!(parallel.result.totals, scan.result.totals);
}
