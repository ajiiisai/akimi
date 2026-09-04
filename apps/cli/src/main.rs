use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use akimi_ext4::{FilesystemInfo, FilesystemScan};
use akimi_filesystem::{Filesystem, FilesystemError};
use akimi_model::{rank_largest, NodeKind, RankFilter, RankedNode};
use anyhow::Result;
use clap::Parser;
use serde_json::json;

#[derive(Debug, Parser)]
#[command(name = "akimi", version, about = "Scan Linux filesystem disk usage")]
struct Arguments {
    /// A mounted filesystem directory, block device, or filesystem image.
    device: PathBuf,

    /// Number of entries to display.
    #[arg(long, default_value_t = 20)]
    top: usize,

    /// Include regular files in the ranking.
    #[arg(long)]
    files: bool,

    /// Rank directories. Combine with --files to rank both.
    #[arg(long)]
    dirs: bool,

    /// Write machine-readable output.
    #[arg(long)]
    json: bool,

    /// Number of filesystem scan workers. Defaults to available parallelism.
    #[arg(long)]
    threads: Option<NonZeroUsize>,
}

fn main() -> ExitCode {
    match run(Arguments::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            if error
                .downcast_ref::<FilesystemError>()
                .is_some_and(FilesystemError::is_permission_denied)
            {
                eprintln!("\nTry running Akimi with sudo.");
            }
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: Arguments) -> Result<()> {
    let total_started = Instant::now();
    if !arguments.json {
        println!("Akimi {}\n", env!("CARGO_PKG_VERSION"));
        println!("Scanning {}...\n", arguments.device.display());
    }

    let mut filesystem = Filesystem::open(&arguments.device)?;
    let threads = arguments
        .threads
        .map(NonZeroUsize::get)
        .unwrap_or_else(default_thread_count);
    let scan = filesystem.scan_with_threads(threads)?;
    let info = filesystem.info().clone();

    let filter = match (arguments.files, arguments.dirs) {
        (true, true) => RankFilter::DirectoriesAndFiles,
        (true, false) => RankFilter::Files,
        _ => RankFilter::Directories,
    };
    let ranking_started = Instant::now();
    let ranking = rank_largest(
        scan.result.arena.nodes(),
        &scan.result.totals,
        filter,
        arguments.top,
    );
    let ranking_time = ranking_started.elapsed();
    let total_time = total_started.elapsed();

    if arguments.json {
        print_json(&info, &scan, &ranking, ranking_time, total_time)?;
    } else {
        print_text(&info, &scan, &ranking, filter, ranking_time, total_time);
    }
    Ok(())
}

fn print_text(
    info: &FilesystemInfo,
    scan: &FilesystemScan,
    ranking: &[RankedNode],
    filter: RankFilter,
    ranking_time: Duration,
    total_time: Duration,
) {
    println!("Filesystem");
    println!("  type:                 {}", info.filesystem_type);
    println!("  device:               {}", info.device.display());
    println!(
        "  block size:           {} bytes",
        format_count(info.block_size as u64)
    );
    println!("  inode slots:          {}", format_count(info.inode_count));
    println!(
        "  allocated reported:   {}",
        format_count(info.reported_allocated_inodes)
    );
    println!(
        "  features:             compat={:#010x} incompat={:#010x} ro={:#010x}\n",
        info.feature_compat, info.feature_incompat, info.feature_ro_compat
    );
    println!("  size accounting:      {}\n", info.size_accounting);

    println!("Objects");
    println!(
        "  allocated inodes:     {}",
        format_count(scan.stats.allocated_inodes)
    );
    println!("  files:                {}", format_count(scan.stats.files));
    println!(
        "  directories:          {}",
        format_count(scan.stats.directories)
    );
    println!(
        "  symlinks:             {}",
        format_count(scan.stats.symlinks)
    );
    println!("  other:                {}", format_count(scan.stats.other));
    println!("  tree nodes:           {}", format_count(scan.stats.nodes));
    println!(
        "  directory entries:    {}",
        format_count(scan.stats.directory_entries)
    );
    println!(
        "  extra hard links:     {}\n",
        format_count(scan.stats.hard_link_entries)
    );

    println!("Timing");
    println!("  scan workers:         {}", scan.workers);
    println!(
        "  filesystem open:      {:>10}",
        format_duration(scan.timings.open)
    );
    println!(
        "  inode scan:           {:>10}",
        format_duration(scan.timings.inode_scan)
    );
    println!(
        "  directory scan:       {:>10}",
        format_duration(scan.timings.directory_scan)
    );
    println!(
        "  tree build:           {:>10}",
        format_duration(scan.timings.tree_build)
    );
    println!(
        "  aggregation:          {:>10}",
        format_duration(scan.timings.aggregation)
    );
    println!(
        "  ranking:              {:>10}",
        format_duration(ranking_time)
    );
    println!(
        "  total:                {:>10}\n",
        format_duration(total_time)
    );

    if scan.warnings.total() > 0 {
        println!("Warnings");
        println!(
            "  missing inode refs:   {}",
            format_count(scan.warnings.missing_inode_references)
        );
        println!(
            "  nodes without parent: {}",
            format_count(scan.warnings.nodes_without_parent)
        );
        println!(
            "  directory read errs:  {}",
            format_count(scan.warnings.directory_scan_errors)
        );
        println!(
            "  directory aliases:    {}\n",
            format_count(scan.warnings.directory_aliases)
        );
    }

    let heading = match filter {
        RankFilter::Directories => "Largest directories",
        RankFilter::Files => "Largest files",
        RankFilter::DirectoriesAndFiles => "Largest directories and files",
    };
    println!("{heading}\n");
    for ranked in ranking {
        println!(
            "  {:>10}  {}",
            format_size(ranked.allocated_size),
            scan.result.arena.display_path(ranked.id)
        );
    }
}

fn print_json(
    info: &FilesystemInfo,
    scan: &FilesystemScan,
    ranking: &[RankedNode],
    ranking_time: Duration,
    total_time: Duration,
) -> Result<()> {
    let largest = ranking
        .iter()
        .map(|ranked| {
            let node = &scan.result.arena.nodes()[ranked.id.index()];
            json!({
                "path": scan.result.arena.display_path(ranked.id),
                "kind": kind_name(node.kind),
                "allocated_bytes": ranked.allocated_size,
                "logical_bytes": ranked.logical_size,
                "inode": node.inode,
            })
        })
        .collect::<Vec<_>>();
    let value = json!({
        "filesystem": {
            "type": info.filesystem_type,
            "device": info.device.to_string_lossy(),
            "block_size": info.block_size,
            "inode_count": info.inode_count,
            "reported_allocated_inodes": info.reported_allocated_inodes,
            "feature_compat": info.feature_compat,
            "feature_incompat": info.feature_incompat,
            "feature_ro_compat": info.feature_ro_compat,
            "size_accounting": info.size_accounting,
        },
        "stats": {
            "scan_workers": scan.workers,
            "allocated_inodes": scan.stats.allocated_inodes,
            "files": scan.stats.files,
            "directories": scan.stats.directories,
            "symlinks": scan.stats.symlinks,
            "other": scan.stats.other,
            "nodes": scan.stats.nodes,
            "directory_entries": scan.stats.directory_entries,
            "hard_link_entries": scan.stats.hard_link_entries,
        },
        "warnings": {
            "missing_inode_references": scan.warnings.missing_inode_references,
            "nodes_without_parent": scan.warnings.nodes_without_parent,
            "directory_scan_errors": scan.warnings.directory_scan_errors,
            "directory_aliases": scan.warnings.directory_aliases,
        },
        "timings_ms": {
            "open": milliseconds(scan.timings.open),
            "inode_scan": milliseconds(scan.timings.inode_scan),
            "directory_scan": milliseconds(scan.timings.directory_scan),
            "tree_build": milliseconds(scan.timings.tree_build),
            "aggregation": milliseconds(scan.timings.aggregation),
            "ranking": milliseconds(ranking_time),
            "total": milliseconds(total_time),
        },
        "largest": largest,
    });
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn default_thread_count() -> usize {
    std::thread::available_parallelism()
        .map(NonZeroUsize::get)
        .unwrap_or(1)
}

fn format_duration(duration: Duration) -> String {
    format!("{:.1} ms", milliseconds(duration))
}

fn format_count(value: u64) -> String {
    let digits = value.to_string();
    let mut output = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            output.push(',');
        }
        output.push(character);
    }
    output
}

fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.2} {}", UNITS[unit])
}

fn kind_name(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::File => "file",
        NodeKind::Directory => "directory",
        NodeKind::Symlink => "symlink",
        NodeKind::Other => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_binary_sizes() {
        assert_eq!(format_size(999), "999 B");
        assert_eq!(format_size(1536), "1.50 KiB");
        assert_eq!(format_size(1024 * 1024 * 3), "3.00 MiB");
    }

    #[test]
    fn formats_counts_with_separators() {
        assert_eq!(format_count(12), "12");
        assert_eq!(format_count(1_234_567), "1,234,567");
    }
}
