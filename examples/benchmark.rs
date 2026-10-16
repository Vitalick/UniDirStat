use std::error::Error;
use std::fs;
use std::sync::Arc;
use std::time::Instant;

use unidirstat::analysis::{DuplicateOptions, find_duplicates_with_options};
use unidirstat::model::EntryKind;
use unidirstat::scanner::{ScanControl, ScanOptions, scan};
use unidirstat::search::{SearchIndex, SearchQuery, SearchSort};

fn number(value: Option<String>, default: usize) -> Result<usize, std::num::ParseIntError> {
    value.map_or(Ok(default), |value| value.parse())
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args().skip(1);
    let files = number(arguments.next(), 10_000)?;
    let runs = number(arguments.next(), 3)?;
    let threads = number(arguments.next(), 0)?;
    let bytes_per_file = number(arguments.next(), 4096)?;
    let content_groups = number(arguments.next(), 31)?;
    if arguments.next().is_some()
        || files < 2
        || runs == 0
        || threads > 64
        || bytes_per_file == 0
        || !(1..=256).contains(&content_groups)
    {
        return Err(
            "Usage: benchmark [files>=2] [runs>=1] [threads=0..64] [bytes_per_file>=1] [content_groups=1..256]".into(),
        );
    }
    let expected_bytes = (files as u64)
        .checked_mul(bytes_per_file as u64)
        .ok_or("Fixture size is too large")?;
    let fixture = tempfile::tempdir()?;
    let mut payload = vec![0; bytes_per_file];
    for index in 0..files {
        let directory = fixture.path().join(format!("group-{:05}", index / 128));
        if index % 128 == 0 {
            fs::create_dir(&directory)?;
        }
        payload.fill((index % content_groups) as u8);
        fs::write(directory.join(format!("file-{index:08}.rs")), &payload)?;
    }
    const QUERIES: usize = 20;
    println!(
        "files={files} bytes_per_file={bytes_per_file} runs={runs} threads={threads} content_groups={content_groups} queries_per_run={QUERIES} cache=filesystem-managed hash=sha256"
    );
    for run in 1..=runs {
        let started = Instant::now();
        let snapshot = Arc::new(scan(
            &[fixture.path().into()],
            &ScanOptions {
                threads,
                ..ScanOptions::default()
            },
            &ScanControl::default(),
        )?);
        let scan_ms = started.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(snapshot.file_count(), files as u64);
        assert_eq!(snapshot.total_size(false), expected_bytes);
        assert!(snapshot.errors.is_empty(), "{:?}", snapshot.errors);
        let started = Instant::now();
        let index = SearchIndex::with_threads(Arc::clone(&snapshot), threads);
        let index_ms = started.elapsed().as_secs_f64() * 1000.0;
        let query = SearchQuery {
            text: "FILE-".into(),
            kinds: vec![EntryKind::File],
            sort: SearchSort::Name,
            descending: false,
            ..SearchQuery::default()
        };
        let started = Instant::now();
        for _ in 0..QUERIES {
            let found = index.search(&query, &ScanControl::default())?;
            assert_eq!(found.len(), files);
        }
        let search_ms = started.elapsed().as_secs_f64() * 1000.0 / QUERIES as f64;
        let started = Instant::now();
        let duplicate_control = ScanControl::default();
        let report = find_duplicates_with_options(
            &snapshot,
            &DuplicateOptions {
                threads,
                ..DuplicateOptions::default()
            },
            &duplicate_control,
        );
        let hash_ms = started.elapsed().as_secs_f64() * 1000.0;
        assert!(!report.cancelled);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(
            report
                .groups
                .iter()
                .map(|group| group.nodes.len() - 1)
                .sum::<usize>(),
            files.saturating_sub(content_groups)
        );
        println!(
            "run={run} scan_ms={scan_ms:.3} index_ms={index_ms:.3} search_mean_ms={search_ms:.3} hash_ms={hash_ms:.3} groups={} read_bytes={}",
            report.groups.len(),
            duplicate_control
                .bytes
                .load(std::sync::atomic::Ordering::Relaxed)
        );
    }
    Ok(())
}
