#![cfg_attr(all(windows, feature = "gui"), windows_subsystem = "windows")]

use std::{ffi::OsString, path::PathBuf, process::ExitCode};

use unidirstat::{
    analysis,
    model::EntryKind,
    report,
    scanner::{self, ScanControl, ScanOptions},
    search::{SearchIndex, SearchQuery, SearchSort, SearchTarget},
};

#[cfg(feature = "gui")]
mod app;

#[cfg(feature = "gui")]
mod i18n;

#[cfg(feature = "gui")]
mod icon;

#[cfg(feature = "gui")]
mod volumes;

#[cfg(feature = "gui")]
mod selection;

#[cfg(feature = "gui")]
mod map_job;

#[cfg(feature = "gui")]
mod map_view;

const HELP: &str = r#"UniDirStat — modern disk usage explorer

Usage: unidirstat [FOLDER ...] [OPTIONS]

  --scan FOLDER          Add a folder or mounted drive (repeatable)
  --load FILE            Open a JSON or CSV snapshot (read-only)
  --export FILE          Save JSON/CSV scan and exit
  --duplicates FILE      Save exact duplicate JSON/CSV report and exit
  --permissions FILE     Save permission metadata JSON/CSV and exit
  --hash sha256|sha512    Duplicate hash algorithm (default sha256)
  --threads NUMBER       Worker count, 0 = automatic (maximum 64)
  --exclude REGEX        Exclude matching paths (repeatable)
  --one-file-system      Stay on the root filesystem (Unix)
  --search TEXT          Search scanned paths and print matches; implies headless
  --regex                Interpret --search as a regular expression
  --name-only            Match filenames instead of full paths
  --min-size BYTES       Minimum size (inclusive)
  --max-size BYTES       Maximum size (inclusive)
  --min-age DAYS         Minimum modified age
  --max-age DAYS         Maximum modified age
  --kind KIND            file, directory, symlink, other (repeatable)
  --empty-directories    Find empty-in-scan folders (exclusions may hide contents)
  --extension EXT        File extension filter
  --owner UID            Unix owner ID filter
  --sort FIELD           size, name, modified
  --ascending            Sort search results ascending
  --allocated            Search by allocated bytes
  --settings FILE        Portable JSON preferences, instead of OS settings
  --no-gui               Print a scan summary without opening a window
  --help                 Show this help
  --                     Treat remaining arguments as paths

Examples:
  unidirstat ~/Downloads ~/Documents
  unidirstat --scan /data --threads 8 --export scan.json
  unidirstat --load scan.csv --search '\.rs$' --regex --max-size 1048576
  unidirstat --scan /data --duplicates duplicates.csv --hash sha512
"#;

#[derive(Default)]
struct Arguments {
    roots: Vec<PathBuf>,
    load: Option<PathBuf>,
    export: Option<PathBuf>,
    duplicates: Option<PathBuf>,
    permissions: Option<PathBuf>,
    duplicate_options: analysis::DuplicateOptions,
    query: SearchQuery,
    search_requested: bool,
    settings: Option<PathBuf>,
    options: ScanOptions,
    headless: bool,
    help: bool,
}

fn parse_arguments(arguments: impl IntoIterator<Item = OsString>) -> Result<Arguments, String> {
    let mut parsed = Arguments::default();
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        let value = argument.to_str().unwrap_or_default();
        match value {
            "--" => {
                parsed.roots.extend(arguments.map(PathBuf::from));
                break;
            }
            "--help" | "-h" => parsed.help = true,
            "--no-gui" => parsed.headless = true,
            "--one-file-system" => parsed.options.one_file_system = true,
            "--regex" => {
                parsed.query.regex = true;
                parsed.search_requested = true;
            }
            "--name-only" => {
                parsed.query.target = SearchTarget::Name;
                parsed.search_requested = true;
            }
            "--ascending" => {
                parsed.query.descending = false;
                parsed.search_requested = true;
            }
            "--allocated" => {
                parsed.query.allocated = true;
                parsed.search_requested = true;
            }
            "--empty-directories" => {
                parsed.query.empty_directories = true;
                parsed.search_requested = true;
            }
            "--scan" | "--load" | "--export" | "--duplicates" | "--permissions" | "--exclude"
            | "--threads" | "--search" | "--min-size" | "--max-size" | "--min-age"
            | "--max-age" | "--kind" | "--extension" | "--owner" | "--sort" | "--hash"
            | "--settings" => {
                let next = arguments
                    .next()
                    .ok_or_else(|| format!("{value} requires a value"))?;
                if next.is_empty() || next.to_str().is_some_and(|text| text.starts_with("--")) {
                    return Err(format!("{value} requires a value"));
                }
                match value {
                    "--scan" => parsed.roots.push(next.into()),
                    "--load" => parsed.load = Some(next.into()),
                    "--export" => parsed.export = Some(next.into()),
                    "--duplicates" => parsed.duplicates = Some(next.into()),
                    "--permissions" => parsed.permissions = Some(next.into()),
                    "--settings" => parsed.settings = Some(next.into()),
                    "--exclude" => parsed.options.exclude.push(
                        next.into_string()
                            .map_err(|_| "An exclusion must be valid UTF-8")?,
                    ),
                    "--threads" => {
                        let threads: usize = next
                            .to_str()
                            .ok_or("Thread count must be a number")?
                            .parse()
                            .map_err(|_| "Thread count must be a number")?;
                        if threads > 64 {
                            return Err("Thread count must be between 0 and 64".into());
                        }
                        parsed.options.threads = threads;
                        parsed.duplicate_options.threads = threads;
                    }
                    "--hash" => {
                        parsed.duplicate_options.algorithm = match next.to_str() {
                            Some("sha256") => analysis::HashAlgorithm::Sha256,
                            Some("sha512") => analysis::HashAlgorithm::Sha512,
                            _ => return Err("Hash must be sha256 or sha512".into()),
                        };
                    }
                    "--search" | "--extension" => {
                        let text = next
                            .into_string()
                            .map_err(|_| "Search text must be valid UTF-8")?;
                        if value == "--search" {
                            parsed.query.text = text;
                        } else {
                            parsed.query.extension = Some(text);
                        }
                        parsed.search_requested = true;
                    }
                    "--min-size" | "--max-size" | "--min-age" | "--max-age" | "--owner" => {
                        let number: u64 = next
                            .to_str()
                            .ok_or("Filter value must be a nonnegative integer")?
                            .parse()
                            .map_err(|_| "Filter value must be a nonnegative integer")?;
                        match value {
                            "--min-size" => parsed.query.min_size = Some(number),
                            "--max-size" => parsed.query.max_size = Some(number),
                            "--min-age" => {
                                parsed.query.min_age_seconds = Some(
                                    number.checked_mul(86_400).ok_or("Age value is too large")?,
                                )
                            }
                            "--max-age" => {
                                parsed.query.max_age_seconds = Some(
                                    number.checked_mul(86_400).ok_or("Age value is too large")?,
                                )
                            }
                            "--owner" => {
                                parsed.query.owner =
                                    Some(number.try_into().map_err(|_| "Owner UID is too large")?)
                            }
                            _ => {}
                        }
                        parsed.search_requested = true;
                    }
                    "--kind" => {
                        parsed.query.kinds.push(match next.to_str() {
                            Some("file") => EntryKind::File,
                            Some("directory") => EntryKind::Directory,
                            Some("symlink") => EntryKind::Symlink,
                            Some("other") => EntryKind::Other,
                            _ => {
                                return Err("Kind must be file, directory, symlink or other".into());
                            }
                        });
                        parsed.search_requested = true;
                    }
                    "--sort" => {
                        parsed.query.sort = match next.to_str() {
                            Some("size") => SearchSort::Size,
                            Some("name") => SearchSort::Name,
                            Some("modified") => SearchSort::Modified,
                            _ => return Err("Sort field must be size, name or modified".into()),
                        };
                        parsed.search_requested = true;
                    }
                    _ => return Err(format!("Unknown option: {value}")),
                }
            }
            text if text.starts_with('-') => return Err(format!("Unknown option: {text}")),
            _ => parsed.roots.push(argument.into()),
        }
    }
    if parsed.load.is_some() && !parsed.roots.is_empty() {
        return Err("Use folders or --load, not both".into());
    }
    if parsed.options.one_file_system && !cfg!(unix) {
        return Err(
            "Restricting scans to filesystem boundaries is unsupported on this platform".into(),
        );
    }
    if parsed
        .query
        .min_size
        .zip(parsed.query.max_size)
        .is_some_and(|(min, max)| min > max)
        || parsed
            .query
            .min_age_seconds
            .zip(parsed.query.max_age_seconds)
            .is_some_and(|(min, max)| min > max)
    {
        return Err("Filter minimum cannot exceed maximum".into());
    }
    if parsed.query.regex {
        regex::RegexBuilder::new(&parsed.query.text)
            .case_insensitive(true)
            .build()
            .map_err(|error| format!("Invalid search expression: {error}"))?;
    }
    parsed.headless |= parsed.export.is_some()
        || parsed.duplicates.is_some()
        || parsed.permissions.is_some()
        || parsed.search_requested;
    Ok(parsed)
}

fn run() -> Result<(), String> {
    let arguments = parse_arguments(std::env::args_os().skip(1))?;
    if arguments.help {
        print!("{HELP}");
        return Ok(());
    }
    if arguments.headless {
        let control = ScanControl::default();
        let snapshot = if let Some(path) = &arguments.load {
            report::load_snapshot(path)?
        } else {
            if arguments.roots.is_empty() {
                return Err("Select at least one folder with --scan FOLDER".into());
            }
            scanner::scan(&arguments.roots, &arguments.options, &control)?
        };
        if let Some(path) = &arguments.export {
            match path.extension().and_then(|extension| extension.to_str()) {
                Some(extension) if extension.eq_ignore_ascii_case("csv") => {
                    report::export_csv(path, &snapshot)?
                }
                Some(extension) if extension.eq_ignore_ascii_case("json") => {
                    report::save_snapshot(path, &snapshot)?
                }
                _ => return Err("Export filename must end in .json or .csv".into()),
            }
        }
        if let Some(path) = &arguments.duplicates {
            let duplicates = analysis::find_duplicates_with_options(
                &snapshot,
                &arguments.duplicate_options,
                &ScanControl::default(),
            );
            if path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("csv"))
            {
                report::export_duplicates_csv(path, &duplicates, &snapshot)?;
            } else {
                report::save_duplicates(path, &duplicates)?;
            }
            eprintln!(
                "{} duplicate groups; {} hashing errors",
                duplicates.groups.len(),
                duplicates.errors.len()
            );
        }
        if let Some(path) = &arguments.permissions {
            report::export_permissions(path, &snapshot)?;
        }
        if arguments.search_requested {
            let snapshot = std::sync::Arc::new(snapshot);
            let index = SearchIndex::with_threads(
                std::sync::Arc::clone(&snapshot),
                arguments.options.threads,
            );
            let found = index.search(&arguments.query, &ScanControl::default())?;
            for index in &found {
                println!("{}", snapshot.nodes[*index].path.display());
            }
            eprintln!("{} matches", found.len());
            return Ok(());
        }
        println!(
            "{} files, {} folders, {} logical bytes, {} scan errors",
            snapshot.file_count(),
            snapshot.directory_count(),
            snapshot.total_size(false),
            snapshot.errors.len()
        );
        return Ok(());
    }
    #[cfg(feature = "gui")]
    {
        let options = eframe::NativeOptions {
            viewport: eframe::egui::ViewportBuilder::default()
                .with_title("UniDirStat")
                .with_app_id("unidirstat")
                .with_icon(icon::window_icon()?)
                .with_inner_size([1380.0, 900.0])
                .with_min_inner_size([900.0, 640.0]),
            ..Default::default()
        };
        eframe::run_native(
            "UniDirStat",
            options,
            Box::new(move |context| {
                Ok(Box::new(app::UniDirStatApp::new(
                    context,
                    arguments.roots,
                    arguments.load,
                    arguments.options,
                    arguments.settings,
                )))
            }),
        )
        .map_err(|error| error.to_string())
    }
    #[cfg(not(feature = "gui"))]
    Err("This build has no desktop UI; pass --no-gui, --export or --duplicates".into())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("UniDirStat: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(values: &[&str]) -> Result<Arguments, String> {
        parse_arguments(values.iter().map(OsString::from))
    }

    #[test]
    fn missing_option_value_is_rejected() {
        assert!(arguments(&["--scan", "--export", "result.json"]).is_err());
    }

    #[test]
    fn filesystem_boundary_option_is_available_only_on_unix() {
        assert_eq!(
            arguments(&["--one-file-system", "--no-gui"]).is_ok(),
            cfg!(unix)
        );
    }

    #[test]
    fn snapshot_and_live_roots_cannot_mix() {
        assert!(arguments(&["--load", "scan.json", "/tmp"]).is_err());
    }

    #[test]
    fn export_implies_headless_with_multiple_roots() {
        let parsed =
            arguments(&["one", "--scan", "two", "--export", "scan.json"]).expect("valid args");
        assert!(parsed.headless && parsed.roots.len() == 2);
    }

    #[test]
    fn terminator_preserves_flag_like_paths() {
        assert_eq!(
            arguments(&["--", "--folder"]).expect("valid path").roots,
            vec![PathBuf::from("--folder")]
        );
    }

    #[test]
    fn parallel_workers_apply_to_scanning_and_duplicates() {
        let parsed = arguments(&["--threads", "8", "--hash", "sha512"]).unwrap();
        assert_eq!(parsed.options.threads, 8);
        assert_eq!(parsed.duplicate_options.threads, 8);
        assert_eq!(
            parsed.duplicate_options.algorithm,
            analysis::HashAlgorithm::Sha512
        );
        assert!(arguments(&["--threads", "65"]).is_err());
        assert!(arguments(&["--threads", "-1"]).is_err());
        assert!(arguments(&["--hash", "md5"]).is_err());
    }

    #[test]
    fn compound_search_filters_are_headless_and_validated() {
        let parsed = arguments(&[
            "--load",
            "scan.csv",
            "--search",
            "тест",
            "--name-only",
            "--kind",
            "file",
            "--kind",
            "directory",
            "--min-size",
            "10",
            "--max-size",
            "100",
            "--min-age",
            "2",
            "--max-age",
            "30",
            "--owner",
            "501",
            "--sort",
            "modified",
            "--ascending",
        ])
        .unwrap();
        assert!(parsed.headless && parsed.search_requested);
        assert_eq!(parsed.query.text, "тест");
        assert_eq!(parsed.query.target, SearchTarget::Name);
        assert_eq!(parsed.query.kinds.len(), 2);
        assert_eq!(parsed.query.min_age_seconds, Some(172_800));
        assert_eq!(parsed.query.owner, Some(501));
        assert!(!parsed.query.descending);
        assert!(arguments(&["--min-size", "11", "--max-size", "10"]).is_err());
        assert!(arguments(&["--min-age", "10", "--max-age", "9"]).is_err());
        assert!(arguments(&["--owner", "4294967296"]).is_err());
        assert!(arguments(&["--min-age", "18446744073709551615"]).is_err());
    }

    #[test]
    fn report_flags_and_portable_settings_preserve_paths() {
        let parsed =
            arguments(&["--permissions", "rights.csv", "--settings", "portable.json"]).unwrap();
        assert!(parsed.headless);
        assert_eq!(parsed.permissions, Some(PathBuf::from("rights.csv")));
        assert_eq!(parsed.settings, Some(PathBuf::from("portable.json")));
    }

    #[test]
    fn invalid_regex_is_rejected_before_any_report_can_be_replaced() {
        assert!(
            arguments(&[
                "--load",
                "valid.json",
                "--export",
                "existing.json",
                "--search",
                "[",
                "--regex"
            ])
            .is_err()
        );
    }
}
