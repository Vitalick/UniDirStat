use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::analysis::DuplicateReport;
use crate::model::{EntryKind, Node, NodePath, Snapshot};

const MAX_SNAPSHOT_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct ScanMetadata {
    version: u32,
    started: u64,
    elapsed_ms: u64,
    cancelled: bool,
    errors: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct CsvNode {
    path: String,
    kind: String,
    logical_bytes: u64,
    allocated_bytes: Option<u64>,
    files: u64,
    directories: u64,
    modified_unix: Option<u64>,
    readonly: bool,
    unix_mode: Option<String>,
    unix_owner: Option<u32>,
    error: Option<String>,
    #[serde(default)]
    file_identity: Option<String>,
    #[serde(default)]
    scan_metadata: Option<String>,
    #[serde(default)]
    scan_duration_ns: u64,
    #[serde(default)]
    depth_limited: bool,
    #[serde(default)]
    scan_failed: bool,
    #[serde(default)]
    scan_warning: bool,
}

fn read_snapshot_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    if file.metadata().map_err(|error| error.to_string())?.len() > MAX_SNAPSHOT_BYTES {
        return Err("Snapshot exceeds the 256 MiB import limit".into());
    }
    let mut bytes = Vec::new();
    BufReader::new(file)
        .take(MAX_SNAPSHOT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_SNAPSHOT_BYTES {
        return Err("Snapshot exceeds the 256 MiB import limit".into());
    }
    Ok(bytes)
}

pub fn save_snapshot(path: &Path, snapshot: &Snapshot) -> Result<(), String> {
    snapshot.validate()?;
    write_json(path, snapshot)
}

pub fn load_snapshot(path: &Path) -> Result<Snapshot, String> {
    if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("csv"))
    {
        return load_csv(path);
    }
    let bytes = read_snapshot_bytes(path)?;
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
    let mut snapshot: Snapshot =
        serde_json::from_slice(bytes).map_err(|error| format!("Invalid snapshot: {error}"))?;
    snapshot.validate()?;
    snapshot.compact_paths();
    Ok(snapshot)
}

pub fn load_csv(path: &Path) -> Result<Snapshot, String> {
    let bytes = read_snapshot_bytes(path)?;
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
    let mut reader = csv::Reader::from_reader(bytes);
    let mut snapshot = Snapshot {
        version: 1,
        roots: Vec::new(),
        nodes: Vec::new(),
        errors: Vec::new(),
        started: 0,
        elapsed_ms: 0,
        cancelled: false,
    };
    let mut paths = HashMap::new();
    let mut metadata_seen = false;
    let mut empty_metadata = false;
    for row in reader.deserialize::<CsvNode>() {
        if empty_metadata {
            return Err("Empty CSV snapshot metadata must be the only row".into());
        }
        let row = row.map_err(|error| format!("Invalid CSV snapshot: {error}"))?;
        if let Some(metadata) = row.scan_metadata {
            if metadata_seen {
                return Err("CSV snapshot contains repeated scan metadata".into());
            }
            let metadata: ScanMetadata = serde_json::from_str(&metadata)
                .map_err(|error| format!("Invalid CSV scan metadata: {error}"))?;
            snapshot.version = metadata.version;
            snapshot.started = metadata.started;
            snapshot.elapsed_ms = metadata.elapsed_ms;
            snapshot.cancelled = metadata.cancelled;
            snapshot.errors = metadata.errors;
            metadata_seen = true;
        }
        if row.kind == "scan_metadata" {
            if !row.path.is_empty()
                || !metadata_seen
                || !snapshot.nodes.is_empty()
                || row.logical_bytes != 0
                || row.files != 0
                || row.directories != 0
            {
                return Err("Invalid empty CSV snapshot metadata".into());
            }
            empty_metadata = true;
            continue;
        }
        let kind = match row.kind.as_str() {
            "file" => EntryKind::File,
            "directory" => EntryKind::Directory,
            "symlink" => EntryKind::Symlink,
            "other" => EntryKind::Other,
            _ => return Err(format!("Invalid CSV entry kind: {}", row.kind)),
        };
        let path = std::path::PathBuf::from(row.path);
        if paths.insert(path.clone(), snapshot.nodes.len()).is_some() {
            return Err("CSV snapshot contains a duplicate path".into());
        }
        let permissions = row
            .unix_mode
            .map(|mode| u32::from_str_radix(&mode, 8))
            .transpose()
            .map_err(|error| format!("Invalid Unix mode: {error}"))?;
        snapshot.nodes.push(Node {
            path: path.into(),
            parent: None,
            children: Vec::new(),
            kind,
            logical_size: row.logical_bytes,
            allocated_size: row.allocated_bytes,
            files: row.files,
            directories: row.directories,
            modified: row.modified_unix,
            readonly: row.readonly,
            permissions,
            owner: row.unix_owner,
            file_id: row.file_identity,
            error: row.error,
            scan_failed: row.scan_failed,
            scan_warning: row.scan_warning,
            depth_limited: row.depth_limited,
            scan_duration_ns: row.scan_duration_ns,
        });
    }
    for index in 0..snapshot.nodes.len() {
        if let Some(parent) = snapshot.nodes[index]
            .path
            .parent()
            .and_then(|parent| paths.get(parent))
            .copied()
        {
            snapshot.nodes[index].parent = Some(parent);
            snapshot.nodes[parent].children.push(index);
        } else {
            snapshot.roots.push(index);
        }
    }
    for index in 0..snapshot.nodes.len() {
        let mut children = std::mem::take(&mut snapshot.nodes[index].children);
        children.sort_unstable_by(|&first, &second| {
            snapshot.nodes[second]
                .logical_size
                .cmp(&snapshot.nodes[first].logical_size)
                .then_with(|| snapshot.nodes[first].path.cmp(&snapshot.nodes[second].path))
        });
        snapshot.nodes[index].children = children;
    }
    if !metadata_seen {
        snapshot
            .errors
            .extend(snapshot.nodes.iter().filter_map(|node| node.error.clone()));
    }
    snapshot.validate()?;
    snapshot.compact_paths();
    Ok(snapshot)
}

pub fn export_csv(path: &Path, snapshot: &Snapshot) -> Result<(), String> {
    snapshot.validate()?;
    let metadata = serde_json::to_string(&ScanMetadata {
        version: snapshot.version,
        started: snapshot.started,
        elapsed_ms: snapshot.elapsed_ms,
        cancelled: snapshot.cancelled,
        errors: snapshot.errors.clone(),
    })
    .map_err(|error| error.to_string())?;
    atomic_write(path, |file| {
        let mut writer = csv::Writer::from_writer(file);
        if snapshot.nodes.is_empty() {
            writer
                .serialize(CsvNode {
                    path: String::new(),
                    kind: "scan_metadata".into(),
                    logical_bytes: 0,
                    allocated_bytes: None,
                    files: 0,
                    directories: 0,
                    modified_unix: None,
                    readonly: false,
                    unix_mode: None,
                    unix_owner: None,
                    error: None,
                    file_identity: None,
                    scan_metadata: Some(metadata.clone()),
                    scan_failed: false,
                    scan_warning: false,
                    depth_limited: false,
                    scan_duration_ns: 0,
                })
                .map_err(|error| error.to_string())?;
        }
        for (index, node) in snapshot.nodes.iter().enumerate() {
            let kind = match node.kind {
                EntryKind::File => "file",
                EntryKind::Directory => "directory",
                EntryKind::Symlink => "symlink",
                EntryKind::Other => "other",
            };
            writer
                .serialize(CsvNode {
                    path: node
                        .path
                        .to_path_buf()
                        .to_str()
                        .ok_or("CSV snapshots require UTF-8 paths")?
                        .to_owned(),
                    kind: kind.into(),
                    logical_bytes: node.logical_size,
                    allocated_bytes: node.allocated_size,
                    files: node.files,
                    directories: node.directories,
                    modified_unix: node.modified,
                    readonly: node.readonly,
                    unix_mode: node.permissions.map(|mode| format!("{mode:o}")),
                    unix_owner: node.owner,
                    error: node.error.clone(),
                    file_identity: node.file_id.clone(),
                    scan_metadata: (index == 0).then(|| metadata.clone()),
                    scan_failed: node.scan_failed,
                    scan_warning: node.scan_warning,
                    depth_limited: node.depth_limited,
                    scan_duration_ns: node.scan_duration_ns,
                })
                .map_err(|error| error.to_string())?;
        }
        writer.flush().map_err(|error| error.to_string())
    })
}

pub fn save_duplicates(path: &Path, report: &DuplicateReport) -> Result<(), String> {
    write_json(path, report)
}

pub fn export_duplicates_csv(
    path: &Path,
    report: &DuplicateReport,
    snapshot: &Snapshot,
) -> Result<(), String> {
    atomic_write(path, |file| {
        let mut writer = csv::Writer::from_writer(file);
        writer
            .write_record([
                "group",
                "algorithm",
                "hash",
                "path",
                "logical_bytes",
                "allocated_bytes",
                "sampled",
            ])
            .map_err(|error| error.to_string())?;
        for (group_index, group) in report.groups.iter().enumerate() {
            for &index in &group.nodes {
                let node = snapshot
                    .nodes
                    .get(index)
                    .ok_or("Duplicate report refers to a missing node")?;
                writer
                    .write_record([
                        (group_index + 1).to_string(),
                        report.algorithm.as_str().to_owned(),
                        group.hash.clone(),
                        node.path
                            .to_path_buf()
                            .to_str()
                            .ok_or("CSV reports require UTF-8 paths")?
                            .to_owned(),
                        node.logical_size.to_string(),
                        node.allocated_size
                            .map(|size| size.to_string())
                            .unwrap_or_default(),
                        "false".into(),
                    ])
                    .map_err(|error| error.to_string())?;
            }
        }
        writer.flush().map_err(|error| error.to_string())
    })
}

#[derive(Serialize)]
struct PermissionRecord<'entry> {
    path: &'entry NodePath,
    readonly: bool,
    unix_mode: Option<String>,
    unix_owner: Option<u32>,
    error: Option<&'entry str>,
}

pub fn export_permissions(path: &Path, snapshot: &Snapshot) -> Result<(), String> {
    let entries: Vec<_> = snapshot
        .nodes
        .iter()
        .map(|node| PermissionRecord {
            path: &node.path,
            readonly: node.readonly,
            unix_mode: node.permissions.map(|mode| format!("{mode:o}")),
            unix_owner: node.owner,
            error: node.error.as_deref(),
        })
        .collect();
    if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("csv"))
    {
        atomic_write(path, |file| {
            let mut writer = csv::Writer::from_writer(file);
            if entries.is_empty() {
                writer
                    .write_record(["path", "readonly", "unix_mode", "unix_owner", "error"])
                    .map_err(|error| error.to_string())?;
            }
            for entry in &entries {
                writer.serialize(entry).map_err(|error| error.to_string())?;
            }
            writer.flush().map_err(|error| error.to_string())
        })
    } else {
        write_json(path, &entries)
    }
}

pub fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    atomic_write(path, |file| {
        serde_json::to_writer_pretty(file, value).map_err(|error| error.to_string())
    })
}

fn atomic_write(
    path: &Path,
    write: impl FnOnce(&mut File) -> Result<(), String>,
) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    write(temporary.as_file_mut())?;
    temporary
        .as_file_mut()
        .flush()
        .map_err(|error| error.to_string())?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    temporary.persist(path).map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::scanner::{ScanControl, ScanOptions, scan};

    #[cfg(unix)]
    #[test]
    fn csv_refuses_non_utf8_paths_without_replacing_an_existing_report() {
        use std::os::unix::ffi::OsStringExt;
        let fixture = tempfile::tempdir().unwrap();
        let name = std::ffi::OsString::from_vec(vec![b'f', 0xff]);
        fs::write(fixture.path().join("file"), b"data").unwrap();
        let mut snapshot = scan(
            &[fixture.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        let node = snapshot
            .nodes
            .iter_mut()
            .find(|node| node.kind == EntryKind::File)
            .unwrap();
        let mut path = node.path.to_path_buf();
        path.set_file_name(name);
        node.path = path.into();
        let output = fixture.path().join("report.csv");
        fs::write(&output, b"original report").unwrap();
        assert!(export_csv(&output, &snapshot).is_err());
        assert_eq!(fs::read(output).unwrap(), b"original report");
    }

    #[test]
    fn scan_timings_roundtrip_through_json_and_csv() {
        let fixture = tempfile::tempdir().unwrap();
        fs::write(fixture.path().join("data"), b"contents").unwrap();
        let mut snapshot = scan(
            &[fixture.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        snapshot.nodes[0].scan_duration_ns = 1_250_000_000;
        snapshot.nodes[1].scan_duration_ns = 250_000;
        snapshot.nodes[0].scan_warning = true;
        for extension in ["json", "csv"] {
            let output = fixture.path().join(format!("scan.{extension}"));
            if extension == "csv" {
                export_csv(&output, &snapshot).unwrap();
            } else {
                save_snapshot(&output, &snapshot).unwrap();
            }
            let restored = load_snapshot(&output).unwrap();
            assert_eq!(restored.nodes[0].scan_duration_ns, 1_250_000_000);
            assert_eq!(restored.nodes[1].scan_duration_ns, 250_000);
            assert!(restored.nodes[0].scan_warning);
        }
    }

    #[test]
    fn depth_cutoffs_roundtrip_and_old_snapshots_default_to_unlimited() {
        let fixture = tempfile::tempdir().unwrap();
        let snapshot = scan(
            &[fixture.path().into()],
            &ScanOptions {
                max_depth: Some(0),
                ..ScanOptions::default()
            },
            &ScanControl::default(),
        )
        .unwrap();
        for extension in ["json", "csv"] {
            let output = fixture.path().join(format!("limited.{extension}"));
            if extension == "csv" {
                export_csv(&output, &snapshot).unwrap();
            } else {
                save_snapshot(&output, &snapshot).unwrap();
            }
            assert!(load_snapshot(&output).unwrap().nodes[0].depth_limited);
        }
        let mut legacy = serde_json::to_value(snapshot).unwrap();
        for key in ["depth_limited", "scan_failed", "scan_warning"] {
            legacy["nodes"][0].as_object_mut().unwrap().remove(key);
        }
        let restored: Snapshot = serde_json::from_value(legacy).unwrap();
        assert!(
            !restored.nodes[0].depth_limited
                && !restored.nodes[0].scan_warning
                && !restored.nodes[0].scan_failed
        );
    }

    #[test]
    fn legacy_json_without_scan_timings_imports_as_unavailable() {
        let fixture = tempfile::tempdir().unwrap();
        let snapshot = scan(
            &[fixture.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        let mut value = serde_json::to_value(snapshot).unwrap();
        for node in value["nodes"].as_array_mut().unwrap() {
            node.as_object_mut().unwrap().remove("scan_duration_ns");
        }
        let output = fixture.path().join("legacy.json");
        fs::write(&output, serde_json::to_vec(&value).unwrap()).unwrap();
        let restored = load_snapshot(&output).unwrap();
        assert_eq!(restored.nodes.len(), 1);
        assert_eq!(restored.nodes[0].scan_duration_ns, 0);
    }

    #[test]
    fn legacy_csv_without_scan_timings_preserves_identity_and_scan_metadata() {
        let fixture = tempfile::tempdir().unwrap();
        let mut snapshot = scan(
            &[fixture.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        snapshot.started = 17;
        snapshot.nodes[0].scan_duration_ns = 1_250_000_000;
        let current = fixture.path().join("current.csv");
        export_csv(&current, &snapshot).unwrap();
        let legacy = fixture.path().join("legacy.csv");
        let mut reader = csv::Reader::from_path(current).unwrap();
        let mut writer = csv::Writer::from_path(&legacy).unwrap();
        writer
            .write_record(reader.headers().unwrap().iter().take(13))
            .unwrap();
        for record in reader.records() {
            writer
                .write_record(record.unwrap().iter().take(13))
                .unwrap();
        }
        writer.flush().unwrap();
        let restored = load_snapshot(&legacy).unwrap();
        assert_eq!(restored.started, 17);
        assert_eq!(restored.nodes[0].file_id, snapshot.nodes[0].file_id);
        assert_eq!(restored.nodes[0].scan_duration_ns, 0);
    }

    #[test]
    fn cancelled_empty_snapshot_roundtrips_through_csv() {
        let fixture = tempfile::tempdir().unwrap();
        let snapshot = Snapshot {
            version: 1,
            nodes: Vec::new(),
            roots: Vec::new(),
            errors: vec!["Cancelled before discovery".into()],
            started: 17,
            elapsed_ms: 4,
            cancelled: true,
        };
        let output = fixture.path().join("empty.csv");
        export_csv(&output, &snapshot).unwrap();
        assert_eq!(
            serde_json::to_value(load_csv(&output).unwrap()).unwrap(),
            serde_json::to_value(snapshot).unwrap()
        );
    }

    #[test]
    fn empty_csv_metadata_must_be_the_only_row() {
        let fixture = tempfile::tempdir().unwrap();
        let output = fixture.path().join("empty.csv");
        let snapshot = Snapshot {
            version: 1,
            nodes: Vec::new(),
            roots: Vec::new(),
            errors: Vec::new(),
            started: 0,
            elapsed_ms: 0,
            cancelled: true,
        };
        export_csv(&output, &snapshot).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&output)
            .unwrap();
        writeln!(file, ",scan_metadata,0,,0,0,,false,,,,,").unwrap();
        assert!(load_csv(&output).is_err());
    }

    #[test]
    fn empty_duplicate_and_permission_csv_reports_have_headers() {
        let fixture = tempfile::tempdir().unwrap();
        let snapshot = scan(
            &[fixture.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        let duplicates = fixture.path().join("duplicates.csv");
        export_duplicates_csv(&duplicates, &DuplicateReport::default(), &snapshot).unwrap();
        let content = fs::read_to_string(duplicates).unwrap();
        assert_eq!(
            content.lines().next().unwrap(),
            "group,algorithm,hash,path,logical_bytes,allocated_bytes,sampled"
        );
        let permissions = fixture.path().join("permissions.csv");
        export_permissions(&permissions, &snapshot).unwrap();
        let mut reader = csv::Reader::from_path(permissions).unwrap();
        assert_eq!(reader.records().count(), 1);
    }

    #[test]
    fn csv_snapshot_roundtrip_restores_the_tree_and_identity() {
        let fixture = tempfile::tempdir().unwrap();
        fs::create_dir(fixture.path().join("nested")).unwrap();
        fs::write(fixture.path().join("nested/data.txt"), b"contents").unwrap();
        let snapshot = scan(
            &[fixture.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        let output = fixture.path().join("scan.csv");
        export_csv(&output, &snapshot).unwrap();
        let restored = load_snapshot(&output).unwrap();
        assert_eq!(
            serde_json::to_value(restored).unwrap(),
            serde_json::to_value(snapshot).unwrap()
        );
    }

    #[test]
    fn legacy_eleven_column_csv_keeps_totals_without_claiming_file_identity() {
        let fixture = tempfile::tempdir().unwrap();
        fs::write(fixture.path().join("data.txt"), b"contents").unwrap();
        let snapshot = scan(
            &[fixture.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        let current = fixture.path().join("current.csv");
        export_csv(&current, &snapshot).unwrap();
        let legacy = fixture.path().join("legacy.csv");
        let mut reader = csv::Reader::from_path(current).unwrap();
        let mut writer = csv::Writer::from_path(&legacy).unwrap();
        writer
            .write_record(reader.headers().unwrap().iter().take(11))
            .unwrap();
        for record in reader.records() {
            writer
                .write_record(record.unwrap().iter().take(11))
                .unwrap();
        }
        writer.flush().unwrap();
        let restored = load_csv(&legacy).unwrap();
        assert_eq!(restored.total_size(false), snapshot.total_size(false));
        assert_eq!(restored.file_count(), snapshot.file_count());
        assert!(restored.nodes.iter().all(|node| node.file_id.is_none()));
        assert_eq!(restored.started, 0);
    }

    #[test]
    fn csv_import_refuses_duplicate_paths_and_inconsistent_totals() {
        let fixture = tempfile::tempdir().unwrap();
        let snapshot = scan(
            &[fixture.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        let output = fixture.path().join("bad.csv");
        export_csv(&output, &snapshot).unwrap();
        let content = fs::read_to_string(&output).unwrap();
        let record = content.lines().last().unwrap();
        fs::write(&output, format!("{content}{record}\n")).unwrap();
        assert!(load_snapshot(&output).is_err());
    }

    #[test]
    fn csv_import_refuses_a_non_file_node_with_descendants() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("invalid.csv");
        let root = fixture.path().to_string_lossy();
        fs::write(&path, format!("path,kind,logical_bytes,allocated_bytes,files,directories,modified_unix,readonly,unix_mode,unix_owner,error\n{root},file,1,,1,0,,false,,,\n{root}/child,file,1,,1,0,,false,,,\n")).unwrap();
        assert!(load_snapshot(&path).is_err());
    }

    #[test]
    fn snapshot_json_roundtrip_preserves_scan() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("file.txt"), b"content").unwrap();
        let snapshot = scan(
            &[directory.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        let output = directory.path().join("snapshot.json");

        save_snapshot(&output, &snapshot).unwrap();
        let loaded = load_snapshot(&output).unwrap();

        assert_eq!(loaded.total_size(false), snapshot.total_size(false));
        assert_eq!(loaded.nodes.len(), snapshot.nodes.len());
        assert_eq!(loaded.nodes[0].path, snapshot.nodes[0].path);
    }

    #[test]
    fn snapshot_import_rejects_invalid_indices() {
        let directory = tempfile::tempdir().unwrap();
        let snapshot = scan(
            &[directory.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        let mut value = serde_json::to_value(snapshot).unwrap();
        value["roots"] = serde_json::json!([100]);
        let output = directory.path().join("invalid.json");
        fs::write(&output, serde_json::to_vec(&value).unwrap()).unwrap();

        assert!(load_snapshot(&output).is_err());
    }

    #[test]
    fn snapshot_import_rejects_deeply_nested_json() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("nested.json");
        fs::write(&output, format!("{}0{}", "[".repeat(256), "]".repeat(256))).unwrap();

        assert!(load_snapshot(&output).is_err());
    }

    #[test]
    fn snapshot_import_rejects_oversized_files_before_reading() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("oversized.json");
        File::create(&output)
            .unwrap()
            .set_len(256 * 1024 * 1024 + 1)
            .unwrap();

        assert!(load_snapshot(&output).unwrap_err().contains("256 MiB"));
    }

    #[test]
    fn duplicate_report_atomically_replaces_existing_json() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("duplicates.json");
        fs::write(&output, "old contents").unwrap();
        let report = DuplicateReport {
            errors: vec!["could not read file".into()],
            ..DuplicateReport::default()
        };

        save_duplicates(&output, &report).unwrap();
        let loaded: DuplicateReport = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();

        assert_eq!(loaded.errors, report.errors);
    }

    #[test]
    fn csv_quotes_paths_with_commas_quotes_and_newlines() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("file.txt"), b"content").unwrap();
        let mut snapshot = scan(
            &[directory.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        let mut path = snapshot.nodes[1].path.to_path_buf();
        path.set_file_name("comma,quote\"line\n.txt");
        snapshot.nodes[1].path = path.into();
        let output = directory.path().join("report.csv");

        export_csv(&output, &snapshot).unwrap();
        let mut reader = csv::Reader::from_path(&output).unwrap();
        let rows: Vec<_> = reader.records().map(Result::unwrap).collect();

        assert_eq!(
            rows[1].get(0),
            snapshot.nodes[1].path.to_path_buf().to_str()
        );
    }

    #[test]
    fn failed_atomic_write_preserves_existing_report() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("report.json");
        fs::write(&output, b"old contents").unwrap();

        let result = atomic_write(&output, |file| {
            file.write_all(b"partial new contents")
                .map_err(|error| error.to_string())?;
            Err("write interrupted".into())
        });

        assert!(result.is_err());
        assert_eq!(fs::read(&output).unwrap(), b"old contents");
    }
}
