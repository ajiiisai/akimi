use std::path::PathBuf;

use akimi_filesystem::Filesystem;
use clap::Parser;

#[derive(Parser)]
struct Arguments {
    mount_point: PathBuf,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = Arguments::parse();
    let mut filesystem = Filesystem::open(&arguments.mount_point)?;
    let scan = filesystem.scan_with_threads(1)?;
    serde_json::to_writer(std::io::stdout().lock(), &scan)?;
    Ok(())
}
