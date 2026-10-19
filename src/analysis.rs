use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::{self, Metadata, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::Ordering;
use std::time::UNIX_EPOCH;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha512};

use crate::model::{EntryKind, Node, Snapshot};
use crate::scanner::{ScanControl, ScanPhase, file_identity, path_identity, thread_pool};

#[derive(Clone, Debug)]
pub struct ExtensionStats {
    pub extension: String,
    pub bytes: u64,
    pub files: u64,
}

pub fn extensions(snapshot: &Snapshot, allocated: bool) -> Vec<ExtensionStats> {
    extensions_with_control(snapshot, allocated, &ScanControl::default()).unwrap_or_default()
}

pub fn largest_files(snapshot: &Snapshot, limit: usize) -> Vec<usize> {
    largest_files_with_control(snapshot, limit, &ScanControl::default()).unwrap_or_default()
}

pub fn extensions_with_control(
    snapshot: &Snapshot,
    allocated: bool,
    control: &ScanControl,
) -> Result<Vec<ExtensionStats>, String> {
    if !control.checkpoint() {
        return Err("Analysis cancelled".into());
    }
    control.start_work(snapshot.nodes.len() as u64);
    control.set_phase(ScanPhase::Summarizing);
    let mut summary = BTreeMap::<String, (u64, u64)>::new();
    for (position, node) in snapshot.nodes.iter().enumerate() {
        if !control.checkpoint() {
            return Err("Analysis cancelled".into());
        }
        if position % 256 == 0 && position > 0 {
            control.advance_work(256);
        }
        if node.kind != EntryKind::File {
            continue;
        }
        let item = summary.entry(node.extension()).or_default();
        item.0 = item.0.saturating_add(node.size(allocated));
        item.1 = item.1.saturating_add(1);
    }
    control.advance_work((snapshot.nodes.len() % 256) as u64);
    if !snapshot.nodes.is_empty() && snapshot.nodes.len().is_multiple_of(256) {
        control.advance_work(256);
    }
    control.start_work((summary.len() as u64).saturating_mul(2));
    control.set_phase(ScanPhase::Sorting);
    let mut ordered = BTreeMap::new();
    for (extension, (bytes, files)) in summary {
        if !control.checkpoint() {
            return Err("Analysis cancelled".into());
        }
        ordered.insert((Reverse(bytes), extension), files);
        control.advance_work(1);
    }
    let mut result = Vec::with_capacity(ordered.len());
    for ((Reverse(bytes), extension), files) in ordered {
        if !control.checkpoint() {
            return Err("Analysis cancelled".into());
        }
        result.push(ExtensionStats {
            extension,
            bytes,
            files,
        });
        control.advance_work(1);
    }
    if !control.checkpoint() {
        return Err("Analysis cancelled".into());
    }
    Ok(result)
}

pub fn largest_files_with_control(
    snapshot: &Snapshot,
    limit: usize,
    control: &ScanControl,
) -> Result<Vec<usize>, String> {
    if !control.checkpoint() {
        return Err("Analysis cancelled".into());
    }
    control.start_work(if limit == 0 {
        0
    } else {
        snapshot.nodes.len() as u64
    });
    control.set_phase(ScanPhase::Summarizing);
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut largest = BTreeSet::new();
    for (index, node) in snapshot.nodes.iter().enumerate() {
        if !control.checkpoint() {
            return Err("Analysis cancelled".into());
        }
        if index % 256 == 0 && index > 0 {
            control.advance_work(256);
        }
        if node.kind != EntryKind::File {
            continue;
        }
        let candidate = (Reverse(node.logical_size), index);
        if largest.len() == limit && largest.last().is_some_and(|worst| *worst <= candidate) {
            continue;
        }
        largest.insert(candidate);
        if largest.len() > limit {
            largest.pop_last();
        }
    }
    control.advance_work((snapshot.nodes.len() % 256) as u64);
    if !snapshot.nodes.is_empty() && snapshot.nodes.len().is_multiple_of(256) {
        control.advance_work(256);
    }
    let mut result = Vec::with_capacity(largest.len());
    for (_, index) in largest {
        if !control.checkpoint() {
            return Err("Analysis cancelled".into());
        }
        result.push(index);
    }
    if !control.checkpoint() {
        return Err("Analysis cancelled".into());
    }
    Ok(result)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DuplicateGroup {
    pub size: u64,
    pub hash: String,
    pub nodes: Vec<usize>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HashAlgorithm {
    #[default]
    Sha256,
    Sha512,
}

impl HashAlgorithm {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sha256 => "sha256",
            Self::Sha512 => "sha512",
        }
    }
}

impl std::str::FromStr for HashAlgorithm {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.eq_ignore_ascii_case("sha256") {
            Ok(Self::Sha256)
        } else if value.eq_ignore_ascii_case("sha512") {
            Ok(Self::Sha512)
        } else {
            Err(format!(
                "Unknown hash algorithm: {value}; use sha256 or sha512"
            ))
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DuplicateOptions {
    pub algorithm: HashAlgorithm,
    pub threads: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DuplicateReport {
    #[serde(default)]
    pub algorithm: HashAlgorithm,
    pub groups: Vec<DuplicateGroup>,
    pub errors: Vec<String>,
    pub cancelled: bool,
}

pub fn find_duplicates(snapshot: &Snapshot, control: &ScanControl) -> DuplicateReport {
    find_duplicates_with_options(snapshot, &DuplicateOptions::default(), control)
}

pub fn find_duplicates_with_options(
    snapshot: &Snapshot,
    options: &DuplicateOptions,
    control: &ScanControl,
) -> DuplicateReport {
    let mut report = DuplicateReport {
        algorithm: options.algorithm,
        errors: snapshot.errors.clone(),
        ..DuplicateReport::default()
    };
    if !control.checkpoint() {
        report.cancelled = true;
        return report;
    }
    let pool = match thread_pool(options.threads) {
        Ok(pool) => pool,
        Err(error) => {
            report.errors.push(error);
            return report;
        }
    };
    let mut sizes = BTreeMap::<u64, Vec<usize>>::new();
    let mut identities = HashSet::new();
    let mut hashes = BTreeMap::<(u64, String), Vec<usize>>::new();
    control.start_work(snapshot.nodes.len() as u64);
    control.set_phase(ScanPhase::Filtering);
    for (index, node) in snapshot.nodes.iter().enumerate() {
        if !control.checkpoint() {
            report.cancelled = true;
            return report;
        }
        control.advance_work(1);
        if node.kind != EntryKind::File || node.logical_size == 0 {
            continue;
        }
        if node
            .file_id
            .as_ref()
            .is_some_and(|identity| !identities.insert(identity))
        {
            continue;
        }
        sizes.entry(node.logical_size).or_default().push(index);
    }
    let (large, small): (Vec<_>, Vec<_>) = sizes
        .into_values()
        .filter(|indices| indices.len() > 1)
        .flatten()
        .partition(|index| snapshot.nodes[*index].logical_size > 256 * 1024);
    let mut candidates: Vec<_> = small.into_iter().map(|index| (index, None)).collect();
    control.start_work(large.len() as u64);
    control.set_phase(ScanPhase::Sampling);
    let sampled: Vec<_> = pool.install(|| {
        large
            .into_par_iter()
            .map(|index| match sample_file(&snapshot.nodes[index], control) {
                Ok(None) => None,
                result => {
                    control.visited.fetch_add(1, Ordering::Relaxed);
                    control.advance_work(1);
                    Some((index, result))
                }
            })
            .while_some()
            .collect()
    });
    let mut samples = BTreeMap::<(u64, String), Vec<(usize, Metadata)>>::new();
    let mut errors = Vec::new();
    for (index, result) in sampled {
        if !control.checkpoint() {
            report.cancelled = true;
            return report;
        }
        match result {
            Ok(Some((hash, metadata))) => samples
                .entry((snapshot.nodes[index].logical_size, hash))
                .or_default()
                .push((index, metadata)),
            Err(error) => errors.push((index, format!("{}: {error}", snapshot.nodes[index].path))),
            Ok(None) => {}
        }
    }
    errors.sort_unstable_by_key(|(index, _)| *index);
    report
        .errors
        .extend(errors.into_iter().map(|(_, error)| error));
    for nodes in samples.into_values().filter(|nodes| nodes.len() > 1) {
        if !control.checkpoint() {
            report.cancelled = true;
            return report;
        }
        candidates.extend(
            nodes
                .into_iter()
                .map(|(index, metadata)| (index, Some(metadata))),
        );
    }
    if !control.checkpoint() {
        report.cancelled = true;
        return report;
    }
    control.start_work(candidates.len() as u64);
    control.set_phase(ScanPhase::Hashing);
    let hashed: Vec<_> = pool.install(|| {
        candidates
            .into_par_iter()
            .map(|(index, sampled_metadata)| {
                let result = match options.algorithm {
                    HashAlgorithm::Sha256 => hash_file(&snapshot.nodes[index], control),
                    HashAlgorithm::Sha512 => {
                        hash_file_digest::<Sha512>(&snapshot.nodes[index], control, None)
                    }
                };
                let result = result.and_then(|result| match result {
                    Some((_, ref metadata))
                        if sampled_metadata
                            .as_ref()
                            .is_some_and(|sampled| !metadata_unchanged(sampled, metadata)) =>
                    {
                        Err("file changed since sampling".into())
                    }
                    result => Ok(result),
                });
                match result {
                    Ok(Some(result)) => {
                        if sampled_metadata.is_none() {
                            control.visited.fetch_add(1, Ordering::Relaxed);
                        }
                        control.advance_work(1);
                        Some((index, Ok(result)))
                    }
                    Ok(None) => None,
                    Err(error) => {
                        if sampled_metadata.is_none() {
                            control.visited.fetch_add(1, Ordering::Relaxed);
                        }
                        control.advance_work(1);
                        Some((index, Err(error)))
                    }
                }
            })
            .while_some()
            .collect()
    });
    let mut finished = Vec::with_capacity(hashed.len());
    let mut errors = Vec::new();
    for (position, (index, result)) in hashed.into_iter().enumerate() {
        if position % 256 == 0 && !control.checkpoint() {
            report
                .errors
                .extend(errors.into_iter().map(|(_, error)| error));
            report.cancelled = true;
            return report;
        }
        match result {
            Ok((hash, metadata)) => finished.push((index, hash, metadata)),
            Err(error) => errors.push((index, format!("{}: {error}", snapshot.nodes[index].path))),
        }
    }
    errors.sort_unstable_by_key(|(index, _)| *index);
    report
        .errors
        .extend(errors.into_iter().map(|(_, error)| error));
    if !control.checkpoint() {
        report.cancelled = true;
        return report;
    }
    control.start_work(finished.len() as u64);
    control.set_phase(ScanPhase::Validating);
    let validated: Vec<_> = pool.install(|| {
        finished
            .into_par_iter()
            .map(|(index, hash, metadata)| {
                if !control.checkpoint() {
                    return None;
                }
                let node = &snapshot.nodes[index];
                let path = node.path.to_path_buf();
                let result = match fs::symlink_metadata(&path) {
                    Ok(current)
                        if metadata_unchanged(&metadata, &current)
                            && node.file_id.is_some()
                            && path_identity(&path, &current) == node.file_id =>
                    {
                        Ok(hash)
                    }
                    Ok(_) => Err(format!("{}: changed after hashing", path.display())),
                    Err(error) => Err(format!("{}: {error}", path.display())),
                };
                control.advance_work(1);
                Some((index, result))
            })
            .while_some()
            .collect()
    });
    let mut errors = Vec::new();
    for (position, (index, result)) in validated.into_iter().enumerate() {
        if position % 256 == 0 && !control.checkpoint() {
            report
                .errors
                .extend(errors.into_iter().map(|(_, error)| error));
            report.cancelled = true;
            return report;
        }
        match result {
            Ok(hash) => hashes
                .entry((snapshot.nodes[index].logical_size, hash))
                .or_default()
                .push(index),
            Err(error) => errors.push((index, error)),
        }
    }
    errors.sort_unstable_by_key(|(index, _)| *index);
    report
        .errors
        .extend(errors.into_iter().map(|(_, error)| error));
    if !control.checkpoint() {
        report.cancelled = true;
        return report;
    }
    control.start_work(hashes.len() as u64);
    control.set_phase(ScanPhase::Sorting);
    for ((size, hash), mut nodes) in hashes {
        if !control.checkpoint() {
            report.cancelled = true;
            report.groups.clear();
            return report;
        }
        control.advance_work(1);
        if nodes.len() > 1 {
            nodes.sort_unstable();
            report.groups.push(DuplicateGroup { size, hash, nodes });
        }
    }
    report.groups.sort_by_key(|group| {
        (
            Reverse(
                group
                    .size
                    .saturating_mul(group.nodes.len().saturating_sub(1) as u64),
            ),
            Reverse(group.size),
        )
    });
    if !control.checkpoint() {
        report.cancelled = true;
        report.groups.clear();
    }
    report
}

fn hash_file(node: &Node, control: &ScanControl) -> Result<Option<(String, Metadata)>, String> {
    hash_file_digest::<Sha256>(node, control, None)
}

fn sample_file(node: &Node, control: &ScanControl) -> Result<Option<(String, Metadata)>, String> {
    hash_file_digest::<Sha256>(
        node,
        control,
        Some([0, node.logical_size / 2 - 2048, node.logical_size - 4096]),
    )
}

fn hash_file_digest<Hash: Digest>(
    node: &Node,
    control: &ScanControl,
    sample_offsets: Option<[u64; 3]>,
) -> Result<Option<(String, Metadata)>, String>
where
    sha2::digest::Output<Hash>: std::fmt::LowerHex,
{
    if !control.checkpoint() {
        return Ok(None);
    }
    if let Some(error) = &node.error {
        return Err(error.clone());
    }
    let identity = node
        .file_id
        .as_ref()
        .ok_or("File identity is unavailable; refresh before hashing")?;
    let path = node.path.to_path_buf();
    for parent in path
        .ancestors()
        .skip(1)
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        let metadata = fs::symlink_metadata(parent).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() {
            return Err("a parent directory is a symbolic link".into());
        }
    }
    let before = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
    let modified = before
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_secs());
    if !before.is_file()
        || before.len() != node.logical_size
        || modified != node.modified
        || path_identity(&path, &before).as_ref() != Some(identity)
    {
        return Err("changed since scanning or is no longer a regular file".into());
    }
    if unavailable_content(&before) {
        return Err("offline or placeholder file was skipped".into());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let mut file = options.open(&path).map_err(|error| error.to_string())?;
    let opened = file.metadata().map_err(|error| error.to_string())?;
    let current = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
    if !metadata_unchanged(&before, &opened)
        || !metadata_unchanged(&opened, &current)
        || file_identity(&file, &opened).as_ref() != Some(identity)
        || path_identity(&path, &current).as_ref() != Some(identity)
        || unavailable_content(&opened)
    {
        return Err("file was replaced before hashing".into());
    }
    let mut buffer = vec![0; 64 * 1024];
    let mut hash = Hash::new();
    let mut length = 0_u64;
    if let Some(offsets) = sample_offsets {
        for offset in offsets {
            if !control.checkpoint() {
                return Ok(None);
            }
            file.seek(SeekFrom::Start(offset))
                .map_err(|error| error.to_string())?;
            file.read_exact(&mut buffer[..4096])
                .map_err(|error| error.to_string())?;
            length += 4096;
            hash.update(&buffer[..4096]);
            control.bytes.fetch_add(4096, Ordering::Relaxed);
        }
    } else {
        loop {
            if !control.checkpoint() {
                return Ok(None);
            }
            let count = file.read(&mut buffer).map_err(|error| error.to_string())?;
            if count == 0 {
                break;
            }
            length = length.saturating_add(count as u64);
            if length > before.len() {
                return Err("file grew while hashing".into());
            }
            hash.update(&buffer[..count]);
            control.bytes.fetch_add(count as u64, Ordering::Relaxed);
        }
    }
    let after = file.metadata().map_err(|error| error.to_string())?;
    let current = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
    if length != sample_offsets.map_or(before.len(), |_| 3 * 4096)
        || !metadata_unchanged(&opened, &after)
        || !metadata_unchanged(&after, &current)
        || file_identity(&file, &after).as_ref() != Some(identity)
        || path_identity(&path, &current).as_ref() != Some(identity)
    {
        return Err("file changed while hashing".into());
    }
    Ok(Some((format!("{:x}", hash.finalize()), after)))
}

fn metadata_unchanged(before: &Metadata, after: &Metadata) -> bool {
    if !after.is_file()
        || after.file_type().is_symlink()
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
        || unavailable_content(after)
    {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if (before.ctime(), before.ctime_nsec()) != (after.ctime(), after.ctime_nsec()) {
            return false;
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if before.creation_time() != after.creation_time() {
            return false;
        }
    }
    true
}

#[cfg(windows)]
fn unavailable_content(metadata: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const FILE_ATTRIBUTE_OFFLINE: u32 = 0x0000_1000;
    const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x0004_0000;
    const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
    metadata.file_attributes()
        & (FILE_ATTRIBUTE_REPARSE_POINT
            | FILE_ATTRIBUTE_OFFLINE
            | FILE_ATTRIBUTE_RECALL_ON_OPEN
            | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
        != 0
}

#[cfg(not(windows))]
fn unavailable_content(_metadata: &Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;
    use std::sync::atomic::Ordering;

    use super::*;
    use crate::scanner::{ScanOptions, scan};

    #[test]
    fn duplicates_require_equal_full_contents() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("first.txt"), b"same contents").unwrap();
        fs::write(directory.path().join("second.bin"), b"same contents").unwrap();
        fs::write(directory.path().join("different.txt"), b"other content").unwrap();
        fs::write(directory.path().join("empty.txt"), b"").unwrap();
        let control = ScanControl::default();
        let snapshot = scan(
            &[directory.path().into()],
            &ScanOptions::default(),
            &control,
        )
        .unwrap();

        let report = find_duplicates(&snapshot, &control);

        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].nodes.len(), 2);
        assert_eq!(report.groups[0].hash.len(), 64);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
    }

    #[test]
    fn sampling_rejects_different_large_files_without_reading_all_bytes() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("first"), vec![1; 2 * 1024 * 1024]).unwrap();
        fs::write(directory.path().join("second"), vec![2; 2 * 1024 * 1024]).unwrap();
        let snapshot = scan(
            &[directory.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        let control = ScanControl::default();

        let report = find_duplicates(&snapshot, &control);

        assert!(report.groups.is_empty());
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(control.bytes.load(Ordering::Relaxed) < 1024 * 1024);
    }

    #[test]
    fn matching_samples_still_require_equal_full_contents() {
        let directory = tempfile::tempdir().unwrap();
        let payload = vec![7; 2 * 1024 * 1024];
        fs::write(directory.path().join("first"), &payload).unwrap();
        fs::write(directory.path().join("copy"), &payload).unwrap();
        let mut different = payload;
        different[512 * 1024] = 8;
        fs::write(directory.path().join("different"), &different).unwrap();
        let snapshot = scan(
            &[directory.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        for algorithm in [HashAlgorithm::Sha256, HashAlgorithm::Sha512] {
            let control = ScanControl::default();
            let report = find_duplicates_with_options(
                &snapshot,
                &DuplicateOptions {
                    algorithm,
                    threads: 2,
                },
                &control,
            );

            assert_eq!(report.groups.len(), 1);
            let mut names: Vec<_> = report.groups[0]
                .nodes
                .iter()
                .map(|index| snapshot.nodes[*index].name())
                .collect();
            names.sort();
            assert_eq!(names, vec!["copy", "first"]);
            assert!(report.errors.is_empty(), "{:?}", report.errors);
            assert!(control.bytes.load(Ordering::Relaxed) >= 6 * 1024 * 1024);
        }
    }

    #[test]
    fn duplicate_stages_report_work_and_honor_cancellation() {
        let directory = tempfile::tempdir().unwrap();
        for name in ["first", "second"] {
            fs::write(directory.path().join(name), vec![7; 2 * 1024 * 1024]).unwrap();
        }
        let snapshot = scan(
            &[directory.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        for phase in [
            ScanPhase::Sampling,
            ScanPhase::Hashing,
            ScanPhase::Validating,
        ] {
            let control = ScanControl::default();
            let gate = crate::scanner::tests::register_gate(&control, phase);
            std::thread::scope(|scope| {
                let worker = scope.spawn(|| {
                    find_duplicates_with_options(
                        &snapshot,
                        &DuplicateOptions {
                            threads: 1,
                            ..DuplicateOptions::default()
                        },
                        &control,
                    )
                });
                gate.arrived.wait();
                assert_eq!(control.phase(), phase);
                assert_eq!(control.work_done.load(Ordering::Relaxed), 1);
                assert_eq!(control.work_total.load(Ordering::Relaxed), 2);
                control.cancel();
                gate.release.wait();
                let report = worker.join().unwrap();
                assert!(report.cancelled);
                assert!(report.groups.is_empty());
            });
        }
    }

    #[test]
    fn files_changed_after_sampling_are_not_reported_as_duplicates() {
        let directory = tempfile::tempdir().unwrap();
        let modified = UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let paths = [
            directory.path().join("first"),
            directory.path().join("second"),
        ];
        for path in &paths {
            let mut file = fs::File::create(path).unwrap();
            file.write_all(&vec![7; 2 * 1024 * 1024]).unwrap();
            file.set_modified(modified).unwrap();
        }
        let snapshot = scan(
            &[directory.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        let control = ScanControl::default();
        let gate = crate::scanner::tests::register_gate(&control, ScanPhase::Sampling);
        let report = std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                find_duplicates_with_options(
                    &snapshot,
                    &DuplicateOptions {
                        threads: 1,
                        ..DuplicateOptions::default()
                    },
                    &control,
                )
            });
            gate.arrived.wait();
            for path in &paths {
                let mut file = OpenOptions::new().write(true).open(path).unwrap();
                file.seek(SeekFrom::Start(512 * 1024)).unwrap();
                file.write_all(&[8]).unwrap();
                file.set_modified(modified + std::time::Duration::from_millis(500))
                    .unwrap();
            }
            gate.release.wait();
            worker.join().unwrap()
        });

        assert!(report.groups.is_empty());
        assert!(
            report
                .errors
                .iter()
                .any(|error| error.ends_with("file changed since sampling"))
        );
    }

    #[test]
    fn duplicate_hash_algorithm_is_explicit_and_matches_the_sha512_vector() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("first"), b"abc").unwrap();
        fs::write(directory.path().join("second"), b"abc").unwrap();
        let snapshot = scan(
            &[directory.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        let options = DuplicateOptions {
            algorithm: HashAlgorithm::Sha512,
            threads: 2,
        };

        let report = find_duplicates_with_options(&snapshot, &options, &ScanControl::default());

        assert_eq!(report.algorithm, HashAlgorithm::Sha512);
        assert_eq!(
            report.groups[0].hash,
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
    }

    #[test]
    fn parallel_duplicates_match_serial_results() {
        let directory = tempfile::tempdir().unwrap();
        for number in 1..=24_u8 {
            let length = if number < 20 {
                number as usize
            } else {
                512 * 1024 + number as usize
            };
            fs::write(
                directory.path().join(format!("{number}-a")),
                vec![number; length],
            )
            .unwrap();
            fs::write(
                directory.path().join(format!("{number}-b")),
                vec![number; length],
            )
            .unwrap();
        }
        let snapshot = scan(
            &[directory.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        let serial = find_duplicates_with_options(
            &snapshot,
            &DuplicateOptions {
                threads: 1,
                ..DuplicateOptions::default()
            },
            &ScanControl::default(),
        );
        let parallel = find_duplicates_with_options(
            &snapshot,
            &DuplicateOptions {
                threads: 4,
                ..DuplicateOptions::default()
            },
            &ScanControl::default(),
        );

        assert_eq!(
            serde_json::to_value(serial).unwrap(),
            serde_json::to_value(parallel).unwrap()
        );
    }

    #[test]
    fn parallel_hashing_pauses_then_cancels_from_the_paused_state() {
        let directory = tempfile::tempdir().unwrap();
        for name in ["first", "second"] {
            fs::File::create(directory.path().join(name))
                .unwrap()
                .set_len(32 * 1024 * 1024)
                .unwrap();
        }
        let snapshot = scan(
            &[directory.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        let control = ScanControl::default();
        let completed = std::sync::atomic::AtomicBool::new(false);
        let ready = std::sync::Barrier::new(2);
        let report = std::thread::scope(|scope| {
            scope.spawn(|| {
                ready.wait();
                while control.bytes.load(Ordering::Relaxed) == 0
                    && !completed.load(Ordering::Relaxed)
                {
                    std::thread::yield_now();
                }
                control.pause();
                std::thread::sleep(std::time::Duration::from_millis(20));
                let paused = control.bytes.load(Ordering::Relaxed);
                std::thread::sleep(std::time::Duration::from_millis(20));
                let stopped = control.bytes.load(Ordering::Relaxed);
                control.cancel();
                assert_eq!(paused, stopped);
            });
            ready.wait();
            let report = find_duplicates_with_options(
                &snapshot,
                &DuplicateOptions {
                    threads: 2,
                    ..DuplicateOptions::default()
                },
                &control,
            );
            completed.store(true, Ordering::Relaxed);
            report
        });

        assert!(report.cancelled);
        assert!(report.groups.is_empty());
        assert!(report.errors.is_empty(), "{:?}", report.errors);
    }

    #[test]
    fn legacy_duplicate_json_defaults_to_sha256() {
        let report: DuplicateReport =
            serde_json::from_str(r#"{"groups":[],"errors":[],"cancelled":false}"#).unwrap();
        assert_eq!(report.algorithm.as_str(), "sha256");
    }

    #[test]
    fn extensions_and_largest_files_use_regular_files_only() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("first.txt"), b"123").unwrap();
        fs::write(directory.path().join("second.txt"), b"12345").unwrap();
        fs::write(directory.path().join("third.bin"), b"12").unwrap();
        let snapshot = scan(
            &[directory.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();

        let summary = extensions(&snapshot, false);
        let largest = largest_files(&snapshot, 1);

        assert_eq!(summary[0].bytes, 8);
        assert_eq!(summary[0].files, 2);
        assert_eq!(snapshot.nodes[largest[0]].name(), "second.txt");
        assert!(largest_files(&snapshot, 0).is_empty());
    }

    #[test]
    fn controlled_summaries_honor_cancellation() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("data.txt"), b"content").unwrap();
        let snapshot = scan(
            &[directory.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        let control = ScanControl::default();
        control.cancel();

        assert!(extensions_with_control(&snapshot, false, &control).is_err());
        assert!(largest_files_with_control(&snapshot, 5, &control).is_err());
    }

    #[test]
    fn summaries_report_completed_work_and_preserve_scan_totals() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("data.txt"), b"content").unwrap();
        let snapshot = scan(
            &[directory.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        let control = ScanControl::default();
        control.visited.store(42, Ordering::Relaxed);
        extensions_with_control(&snapshot, false, &control).unwrap();
        assert_eq!(
            control.work_done.load(Ordering::Relaxed),
            control.work_total.load(Ordering::Relaxed)
        );
        largest_files_with_control(&snapshot, 5, &control).unwrap();
        assert_eq!(
            control.work_done.load(Ordering::Relaxed),
            snapshot.nodes.len() as u64
        );
        assert_eq!(
            control.work_total.load(Ordering::Relaxed),
            snapshot.nodes.len() as u64
        );
        assert_eq!(control.visited.load(Ordering::Relaxed), 42);
    }

    #[test]
    fn summaries_cancel_after_reporting_progress() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("data.txt"), b"content").unwrap();
        let snapshot = scan(
            &[directory.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        for extensions in [true, false] {
            let control = ScanControl::default();
            let gate = crate::scanner::tests::register_gate(&control, ScanPhase::Summarizing);
            std::thread::scope(|scope| {
                let worker = scope.spawn(|| {
                    if extensions {
                        extensions_with_control(&snapshot, false, &control).map(|_| ())
                    } else {
                        largest_files_with_control(&snapshot, 5, &control).map(|_| ())
                    }
                });
                gate.arrived.wait();
                assert!(control.work_done.load(Ordering::Relaxed) > 0);
                control.cancel();
                gate.release.wait();
                assert!(worker.join().unwrap().is_err());
            });
        }
    }

    #[test]
    fn largest_files_keep_the_requested_limit_and_break_equal_sizes_by_node_id() {
        let directory = tempfile::tempdir().unwrap();
        for name in ["first", "second", "third"] {
            fs::write(directory.path().join(name), b"same").unwrap();
        }
        let snapshot = scan(
            &[directory.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        let expected: Vec<_> = snapshot
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| (node.kind == EntryKind::File).then_some(index))
            .take(2)
            .collect();

        assert_eq!(
            largest_files_with_control(&snapshot, 2, &ScanControl::default()).unwrap(),
            expected
        );
    }

    #[test]
    fn duplicates_reject_changed_or_missing_files() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first");
        let second = directory.path().join("second");
        fs::write(&first, b"same").unwrap();
        fs::write(&second, b"same").unwrap();
        let control = ScanControl::default();
        let snapshot = scan(
            &[directory.path().into()],
            &ScanOptions::default(),
            &control,
        )
        .unwrap();
        fs::write(&first, b"changed size").unwrap();
        fs::remove_file(&second).unwrap();

        let report = find_duplicates(&snapshot, &control);

        assert!(report.groups.is_empty());
        assert_eq!(report.errors.len(), 2);
    }

    #[test]
    fn duplicates_honor_cancellation_before_reading() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("first"), b"same").unwrap();
        fs::write(directory.path().join("second"), b"same").unwrap();
        let control = ScanControl::default();
        let snapshot = scan(
            &[directory.path().into()],
            &ScanOptions::default(),
            &control,
        )
        .unwrap();
        control.cancelled.store(true, Ordering::Relaxed);

        let report = find_duplicates(&snapshot, &control);

        assert!(report.cancelled);
        assert!(report.groups.is_empty());
    }

    #[test]
    fn hashing_can_be_cancelled_during_a_large_file_read() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large");
        fs::File::create(&path)
            .unwrap()
            .set_len(16 * 1024 * 1024)
            .unwrap();
        let snapshot = scan(
            &[directory.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        let canonical = path.canonicalize().unwrap();
        let node = snapshot
            .nodes
            .iter()
            .find(|node| node.path.to_path_buf() == canonical)
            .unwrap();
        let control = ScanControl::default();
        let completed = std::sync::atomic::AtomicBool::new(false);
        let ready = std::sync::Barrier::new(2);

        let result = std::thread::scope(|scope| {
            scope.spawn(|| {
                ready.wait();
                while control.bytes.load(Ordering::Relaxed) == 0
                    && !completed.load(Ordering::Relaxed)
                {
                    std::thread::yield_now();
                }
                control.cancelled.store(true, Ordering::Relaxed);
            });
            ready.wait();
            let result = hash_file(node, &control);
            completed.store(true, Ordering::Relaxed);
            result
        });

        assert!(result.unwrap().is_none());
    }

    #[test]
    fn hashing_refuses_files_without_a_snapshot_identity() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("data"), b"content").unwrap();
        let mut snapshot = scan(
            &[directory.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        let node = snapshot
            .nodes
            .iter_mut()
            .find(|node| node.kind == EntryKind::File)
            .unwrap();
        node.file_id = None;

        assert!(hash_file(node, &ScanControl::default()).is_err());
    }

    #[test]
    fn hashing_rejects_a_file_that_changes_during_reading() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large");
        let file = fs::File::create(&path).unwrap();
        file.set_len(16 * 1024 * 1024).unwrap();
        let snapshot = scan(
            &[directory.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        let canonical = path.canonicalize().unwrap();
        let node = snapshot
            .nodes
            .iter()
            .find(|node| node.path.to_path_buf() == canonical)
            .unwrap();
        let control = ScanControl::default();
        let completed = std::sync::atomic::AtomicBool::new(false);
        let ready = std::sync::Barrier::new(2);

        let result = std::thread::scope(|scope| {
            scope.spawn(|| {
                ready.wait();
                while control.bytes.load(Ordering::Relaxed) == 0
                    && !completed.load(Ordering::Relaxed)
                {
                    std::thread::yield_now();
                }
                file.set_len(1).unwrap();
            });
            ready.wait();
            let result = hash_file(node, &control);
            completed.store(true, Ordering::Relaxed);
            result
        });

        assert!(result.is_err());
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn hard_links_are_not_reclaimable_duplicates() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first");
        fs::write(&first, b"same").unwrap();
        fs::hard_link(&first, directory.path().join("linked")).unwrap();
        let control = ScanControl::default();
        let snapshot = scan(
            &[directory.path().into()],
            &ScanOptions::default(),
            &control,
        )
        .unwrap();

        let report = find_duplicates(&snapshot, &control);

        assert!(report.groups.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn duplicates_refuse_files_replaced_by_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first");
        let second = directory.path().join("second");
        fs::write(&first, b"same").unwrap();
        fs::write(&second, b"same").unwrap();
        let control = ScanControl::default();
        let snapshot = scan(
            &[directory.path().into()],
            &ScanOptions::default(),
            &control,
        )
        .unwrap();
        fs::remove_file(&second).unwrap();
        std::os::unix::fs::symlink(&first, &second).unwrap();

        let report = find_duplicates(&snapshot, &control);

        assert!(report.groups.is_empty());
        assert_eq!(report.errors.len(), 1);
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn duplicates_refuse_same_size_replacements() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first");
        let second = directory.path().join("second");
        let payload = vec![7; 512 * 1024];
        fs::write(&first, &payload).unwrap();
        fs::write(&second, &payload).unwrap();
        let control = ScanControl::default();
        let snapshot = scan(
            &[directory.path().into()],
            &ScanOptions::default(),
            &control,
        )
        .unwrap();
        fs::rename(&second, directory.path().join("old")).unwrap();
        fs::write(&second, &payload).unwrap();

        let report = find_duplicates(&snapshot, &control);

        assert!(report.groups.is_empty());
        assert_eq!(report.errors.len(), 1);
    }
}
