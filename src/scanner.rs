use std::collections::{HashMap, HashSet};
use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rayon::prelude::*;
use regex::RegexSet;

use crate::model::{EntryKind, Node, Snapshot};

mod progress;
use progress::{DirectoryProgress, LiveScan, LiveTotals, duration_ns};
pub use progress::{DirectoryScanStatus, ScanView};

pub const DEFAULT_MAX_DEPTH: usize = 256;

#[derive(Clone, Debug)]
pub struct ScanOptions {
    pub exclude: Vec<String>,
    pub one_file_system: bool,
    pub threads: usize,
    /// Maximum entry depth relative to each root (zero); None scans every level.
    pub max_depth: Option<usize>,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            exclude: Vec::new(),
            one_file_system: false,
            threads: 0,
            max_depth: Some(DEFAULT_MAX_DEPTH),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum ScanPhase {
    #[default]
    Idle,
    Discovering,
    Finalizing,
    Complete,
    Indexing,
    Summarizing,
    Sorting,
    Mapping,
    Filtering,
    Sampling,
    Hashing,
    Validating,
}

#[derive(Default, Debug)]
pub struct ScanControl {
    pub cancelled: AtomicBool,
    pub paused: AtomicBool,
    pub visited: AtomicU64,
    pub bytes: AtomicU64,
    pub allocated_bytes: AtomicU64,
    pub unknown_allocations: AtomicU64,
    /// Directory listings intentionally skipped at the configured depth limit.
    pub depth_limited: AtomicU64,
    pub phase: AtomicU8,
    pub work_done: AtomicU64,
    pub work_total: AtomicU64,
    current_path: Mutex<Option<PathBuf>>,
    waiting: AtomicU64,
    live_enabled: AtomicBool,
    live: Mutex<Option<LiveScan>>,
}

impl ScanControl {
    pub fn pause(&self) {
        self.paused.store(true, Ordering::Release);
    }

    pub fn resume(&self) {
        self.paused.store(false, Ordering::Release);
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.resume();
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Acquire)
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub fn phase(&self) -> ScanPhase {
        match self.phase.load(Ordering::Acquire) {
            1 => ScanPhase::Discovering,
            2 => ScanPhase::Finalizing,
            3 => ScanPhase::Complete,
            4 => ScanPhase::Indexing,
            5 => ScanPhase::Summarizing,
            6 => ScanPhase::Sorting,
            7 => ScanPhase::Mapping,
            8 => ScanPhase::Filtering,
            9 => ScanPhase::Sampling,
            10 => ScanPhase::Hashing,
            11 => ScanPhase::Validating,
            _ => ScanPhase::Idle,
        }
    }

    pub fn set_phase(&self, phase: ScanPhase) {
        self.phase.store(phase as u8, Ordering::Release);
    }

    pub fn start_work(&self, total: u64) {
        self.work_done.store(0, Ordering::Relaxed);
        self.work_total.store(total, Ordering::Release);
    }

    pub fn advance_work(&self, count: u64) {
        self.work_done.fetch_add(count, Ordering::Relaxed);
        #[cfg(test)]
        tests::gate(self, self.phase());
    }

    pub fn current_path(&self) -> Option<PathBuf> {
        self.current_path
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn checkpoint(&self) -> bool {
        if self.is_cancelled() {
            return false;
        }
        if self.is_paused() {
            self.waiting.fetch_add(1, Ordering::AcqRel);
            while self.is_paused() && !self.is_cancelled() {
                std::thread::sleep(Duration::from_millis(5));
            }
            self.waiting.fetch_sub(1, Ordering::AcqRel);
        }
        !self.is_cancelled()
    }
}

pub(crate) fn thread_pool(threads: usize) -> Result<rayon::ThreadPool, String> {
    let threads = if threads == 0 {
        std::thread::available_parallelism()
            .map_or(1, usize::from)
            .min(16)
    } else {
        threads.clamp(1, 64)
    };
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(|error| error.to_string())
}

#[derive(Debug)]
struct ScanState {
    snapshot: Snapshot,
    identities: HashMap<String, usize>,
    directories: HashSet<String>,
    progress: Option<DirectoryProgress>,
}

struct Traversal<'control> {
    state: Arc<Mutex<ScanState>>,
    excluded: &'control RegexSet,
    options: &'control ScanOptions,
    control: &'control ScanControl,
}

pub fn scan(
    roots: &[PathBuf],
    options: &ScanOptions,
    control: &ScanControl,
) -> Result<Snapshot, String> {
    scan_with_directories(roots, options, control, HashSet::new())
}

fn scan_with_directories(
    roots: &[PathBuf],
    options: &ScanOptions,
    control: &ScanControl,
    mut directories: HashSet<String>,
) -> Result<Snapshot, String> {
    if options.one_file_system && !cfg!(unix) {
        return Err(
            "Restricting scans to filesystem boundaries is unsupported on this platform".to_owned(),
        );
    }
    let excluded = RegexSet::new(&options.exclude)
        .map_err(|error| format!("Invalid exclusion expression: {error}"))?;
    let pool = thread_pool(options.threads)?;
    let started = Instant::now();
    *control
        .live
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    control.start_work(0);
    *control
        .current_path
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    control
        .phase
        .store(ScanPhase::Discovering as u8, Ordering::Release);
    let mut snapshot = Snapshot {
        version: 1,
        roots: Vec::new(),
        nodes: Vec::new(),
        errors: Vec::new(),
        started: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        elapsed_ms: 0,
        cancelled: false,
    };
    control.visited.store(0, Ordering::Relaxed);
    control.bytes.store(0, Ordering::Relaxed);
    control.allocated_bytes.store(0, Ordering::Relaxed);
    control.unknown_allocations.store(0, Ordering::Relaxed);
    control.depth_limited.store(0, Ordering::Relaxed);
    let current = std::env::current_dir().map_err(|error| error.to_string())?;
    let mut candidates = Vec::new();
    for root in roots {
        if !control.checkpoint() {
            break;
        }
        let metadata_started = Instant::now();
        let absolute: PathBuf = current.join(root).components().collect();
        let normalized = match (absolute.parent(), absolute.file_name()) {
            (Some(parent), Some(name)) => fs::canonicalize(parent).map(|parent| parent.join(name)),
            _ => fs::canonicalize(&absolute),
        };
        let path = match normalized {
            Ok(path) => path,
            Err(error) => {
                snapshot.errors.push(format!("{}: {error}", root.display()));
                continue;
            }
        };
        if excluded.is_match(&path.to_string_lossy()) {
            continue;
        }
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                let canonical = if is_link(&metadata) {
                    Ok(path.clone())
                } else {
                    fs::canonicalize(&path)
                };
                match canonical {
                    Ok(canonical) => {
                        candidates.push((canonical, metadata, metadata_started.elapsed()))
                    }
                    Err(error) => snapshot.errors.push(format!("{}: {error}", path.display())),
                }
            }
            Err(error) => snapshot.errors.push(format!("{}: {error}", path.display())),
        }
    }
    candidates.sort_by(|(left, _, _), (right, _, _)| {
        left.components()
            .count()
            .cmp(&right.components().count())
            .then_with(|| left.cmp(right))
    });
    let mut accepted: Vec<(PathBuf, bool)> = Vec::new();
    let mut identities = HashMap::new();
    let mut pending = Vec::new();
    for (path, metadata, metadata_duration) in candidates {
        if !control.checkpoint() {
            break;
        }
        if accepted.iter().any(|(previous, directory)| {
            path == *previous || (*directory && path.starts_with(previous))
        }) {
            continue;
        }
        let directory = metadata.is_dir() && !is_link(&metadata);
        accepted.push((path.clone(), directory));
        let device = metadata_device(&metadata);
        let metadata_started = Instant::now();
        let mut node = node_from_metadata(path, None, &metadata);
        if node.kind != EntryKind::Directory {
            node.scan_duration_ns =
                duration_ns(metadata_duration.saturating_add(metadata_started.elapsed()));
        }
        let Some(index) = insert_node(
            &mut snapshot,
            &mut identities,
            &mut directories,
            node,
            control,
        ) else {
            continue;
        };
        snapshot.roots.push(index);
        if directory {
            pending.push((index, device));
        }
    }
    if snapshot.roots.is_empty() && !control.cancelled.load(Ordering::Relaxed) {
        control
            .phase
            .store(ScanPhase::Complete as u8, Ordering::Release);
        return Err(if snapshot.errors.is_empty() {
            "No paths to scan after exclusions".to_owned()
        } else {
            snapshot.errors.join("\n")
        });
    }
    let progress = Some(DirectoryProgress::new(
        &snapshot,
        control.live_enabled.load(Ordering::Acquire),
    ));
    let state = Arc::new(Mutex::new(ScanState {
        snapshot,
        identities,
        directories,
        progress,
    }));
    if control.live_enabled.load(Ordering::Acquire) {
        *control
            .live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(LiveScan::Active(Arc::clone(&state)));
    }
    let traversal = Traversal {
        state,
        excluded: &excluded,
        options,
        control,
    };
    pool.install(|| {
        rayon::scope(|scope| {
            for (index, device) in pending {
                let traversal = &traversal;
                scope.spawn(move |scope| traversal.directory(scope, index, device, 0));
            }
        })
    });
    control.set_phase(ScanPhase::Finalizing);
    #[cfg(test)]
    tests::gate(control, ScanPhase::Finalizing);
    let mut state = traversal
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut snapshot = std::mem::replace(
        &mut state.snapshot,
        Snapshot {
            version: 1,
            roots: Vec::new(),
            nodes: Vec::new(),
            errors: Vec::new(),
            started: 0,
            elapsed_ms: 0,
            cancelled: false,
        },
    );
    let progress = state.progress.take();
    drop(state);
    if let Some(mut progress) = progress.filter(|_| control.live_enabled.load(Ordering::Acquire)) {
        progress.stop_timers();
        *control
            .live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(LiveScan::Detached(progress));
    }
    drop(traversal);
    control
        .phase
        .store(ScanPhase::Finalizing as u8, Ordering::Release);
    snapshot.nodes.shrink_to_fit();
    finalize(&mut snapshot, control, false);
    snapshot.cancelled = control.is_cancelled();
    snapshot.elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    control
        .phase
        .store(ScanPhase::Complete as u8, Ordering::Release);
    Ok(snapshot)
}

pub fn refresh_subtree(
    snapshot: &Snapshot,
    index: usize,
    options: &ScanOptions,
    control: &ScanControl,
) -> Result<Snapshot, String> {
    let started = Instant::now();
    refresh_checkpoint(control)?;
    let selected = snapshot.nodes.get(index).ok_or("Invalid subtree index")?;
    if selected.kind != EntryKind::Directory {
        return Err("Only directories can be refreshed as subtrees".to_owned());
    }
    snapshot.validate()?;
    let metadata = fs::symlink_metadata(&selected.path).map_err(|error| error.to_string())?;
    if !metadata.is_dir()
        || is_link(&metadata)
        || fs::canonicalize(&selected.path).ok().as_ref() != Some(&selected.path)
    {
        return Err("The selected directory changed or became a link; run a new scan".to_owned());
    }
    let mut mapping = vec![usize::MAX; snapshot.nodes.len()];
    let mut retained = Vec::new();
    let mut directories = HashSet::new();
    let mut pending: Vec<_> = snapshot.roots.iter().rev().copied().collect();
    while let Some(previous) = pending.pop() {
        if retained.len() % 256 == 0 {
            refresh_checkpoint(control)?;
        }
        if previous == index {
            continue;
        }
        let node = &snapshot.nodes[previous];
        if node.kind == EntryKind::Directory
            && let Some(identity) = &node.file_id
        {
            directories.insert(identity.clone());
        }
        mapping[previous] = retained.len();
        retained.push(previous);
        pending.extend(node.children.iter().rev().copied());
    }
    let mut subtree_options = options.clone();
    if let Some(limit) = &mut subtree_options.max_depth {
        let mut ancestor = selected.parent;
        while let Some(index) = ancestor {
            *limit = limit.saturating_sub(1);
            ancestor = snapshot.nodes[index].parent;
        }
    }
    let fresh = scan_with_directories(
        std::slice::from_ref(&selected.path),
        &subtree_options,
        control,
        directories,
    )?;
    refresh_checkpoint(control)?;
    let fresh_root = *fresh.roots.first().ok_or("The subtree scan has no root")?;
    if fresh.cancelled || fresh.nodes[fresh_root].kind != EntryKind::Directory {
        return Err("Subtree refresh was cancelled or the directory changed; previous results are preserved".to_owned());
    }
    if let Some(error) = &fresh.nodes[fresh_root].error {
        return Err(format!("{error}; previous subtree results are preserved"));
    }
    if snapshot.roots.len() == 1 && snapshot.roots[0] == index {
        return Ok(fresh);
    }
    control
        .phase
        .store(ScanPhase::Finalizing as u8, Ordering::Release);
    let mut hints = allocation_hints(snapshot, control)?;
    hints.extend(allocation_hints(&fresh, control)?);
    let offset = retained.len();
    let replacement_root = offset + fresh_root;
    let parent = selected.parent.map(|parent| mapping[parent]);
    let mut nodes = Vec::with_capacity(offset + fresh.nodes.len());
    for previous in retained {
        if nodes.len() % 256 == 0 {
            refresh_checkpoint(control)?;
        }
        let mut node = snapshot.nodes[previous].clone();
        node.parent = node.parent.map(|parent| mapping[parent]);
        node.children = node
            .children
            .iter()
            .filter_map(|&child| (mapping[child] != usize::MAX).then_some(mapping[child]))
            .collect();
        nodes.push(node);
    }
    for mut node in fresh.nodes {
        if nodes.len() % 256 == 0 {
            refresh_checkpoint(control)?;
        }
        node.parent = node.parent.map(|parent| offset + parent).or(parent);
        for child in &mut node.children {
            *child += offset;
        }
        nodes.push(node);
    }
    if let Some(parent) = parent {
        nodes[parent].children.push(replacement_root);
    }
    let path_error = format!("{}:", selected.path.display());
    let descendant_error = format!("{}{}", selected.path.display(), std::path::MAIN_SEPARATOR);
    let mut merged = Snapshot {
        version: snapshot.version,
        roots: snapshot
            .roots
            .iter()
            .map(|&root| {
                if root == index {
                    replacement_root
                } else {
                    mapping[root]
                }
            })
            .collect(),
        nodes,
        errors: snapshot
            .errors
            .iter()
            .filter(|error| {
                !error.starts_with(&path_error) && !error.starts_with(&descendant_error)
            })
            .cloned()
            .chain(fresh.errors)
            .collect(),
        started: snapshot.started,
        elapsed_ms: 0,
        cancelled: snapshot.cancelled,
    };
    for index in 0..merged.nodes.len() {
        if index % 256 == 0 {
            refresh_checkpoint(control)?;
        }
        let node = &merged.nodes[index];
        if node.kind != EntryKind::File {
            continue;
        }
        if let Some(identity) = &node.file_id {
            let Some((_, owner)) = hints.get_mut(identity) else {
                continue;
            };
            if owner.is_none_or(|previous| node.path < merged.nodes[previous].path) {
                *owner = Some(index);
            }
            merged.nodes[index].allocated_size = Some(0);
        }
    }
    for (_, (allocated, owner)) in hints {
        if let Some(owner) = owner {
            merged.nodes[owner].allocated_size = allocated;
        }
    }
    finalize(&mut merged, control, true);
    refresh_checkpoint(control)?;
    merged.elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    control
        .phase
        .store(ScanPhase::Complete as u8, Ordering::Release);
    Ok(merged)
}

fn refresh_checkpoint(control: &ScanControl) -> Result<(), String> {
    if control.checkpoint() {
        Ok(())
    } else {
        control
            .phase
            .store(ScanPhase::Complete as u8, Ordering::Release);
        Err("Subtree refresh cancelled; previous results are preserved".to_owned())
    }
}

type AllocationHints = HashMap<String, (Option<u64>, Option<usize>)>;

fn allocation_hints(snapshot: &Snapshot, control: &ScanControl) -> Result<AllocationHints, String> {
    let mut hints: AllocationHints = HashMap::new();
    for (index, node) in snapshot.nodes.iter().enumerate() {
        if index % 256 == 0 {
            refresh_checkpoint(control)?;
        }
        if node.kind == EntryKind::File
            && let Some(identity) = &node.file_id
        {
            hints
                .entry(identity.clone())
                .and_modify(|(allocated, _)| {
                    *allocated = allocated
                        .zip(node.allocated_size)
                        .map(|(left, right)| left.max(right));
                })
                .or_insert((node.allocated_size, None));
        }
    }
    Ok(hints)
}

impl Traversal<'_> {
    fn directory<'scope>(
        &'scope self,
        scope: &rayon::Scope<'scope>,
        index: usize,
        device: Option<u64>,
        depth: usize,
    ) {
        const BATCH: usize = 256;
        #[cfg(test)]
        tests::directory_gate(self.control, index);
        if !self.control.checkpoint() {
            return;
        }
        if self.options.max_depth.is_some_and(|limit| depth >= limit) {
            self.control.depth_limited.fetch_add(1, Ordering::Relaxed);
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .snapshot
                .nodes[index]
                .depth_limited = true;
            self.finish_directory(index);
            return;
        }
        let (path, identity) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(progress) = &mut state.progress {
                progress.start(index);
            }
            (
                state.snapshot.nodes[index].path.clone(),
                state.snapshot.nodes[index].file_id.clone(),
            )
        };
        let unchanged = directory_matches(&path, identity.as_deref());
        if !unchanged.as_ref().is_ok_and(|unchanged| *unchanged) {
            let message = unchanged.err().map_or_else(
                || format!("{}: directory changed during scan", path.display()),
                |error| format!("{}: {error}", path.display()),
            );
            self.record_error(index, message);
            self.finish_directory(index);
            return;
        }
        *self
            .control
            .current_path
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(path.clone());
        let entries = match fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(error) => {
                self.record_error(index, format!("{}: {error}", path.display()));
                self.finish_directory(index);
                return;
            }
        };
        let mut paths = Vec::with_capacity(BATCH);
        for result in entries {
            if !self.control.checkpoint() {
                break;
            }
            match result {
                Ok(entry) => {
                    let path = entry.path();
                    if !self.excluded.is_match(&path.to_string_lossy()) {
                        paths.push((path, entry));
                    }
                }
                Err(error) => self.record_error(index, format!("{}: {error}", path.display())),
            }
            if paths.len() == BATCH {
                self.batch(
                    scope,
                    index,
                    device,
                    depth,
                    std::mem::replace(&mut paths, Vec::with_capacity(BATCH)),
                );
            }
        }
        if !paths.is_empty() {
            self.batch(scope, index, device, depth, paths);
        }
        self.finish_directory(index);
    }

    fn batch<'scope>(
        &'scope self,
        scope: &rayon::Scope<'scope>,
        parent: usize,
        device: Option<u64>,
        depth: usize,
        paths: Vec<(PathBuf, fs::DirEntry)>,
    ) {
        let nodes: Vec<_> = paths
            .into_par_iter()
            .filter_map(|(path, entry)| {
                if !self.control.checkpoint() {
                    return None;
                }
                let metadata_started = Instant::now();
                let metadata = entry.metadata().and_then(|metadata| {
                    // Windows enumeration omits directory lengths needed by live handle validation.
                    if cfg!(windows) && metadata.is_dir() {
                        fs::symlink_metadata(&path)
                    } else {
                        Ok(metadata)
                    }
                });
                #[cfg(test)]
                tests::gate(self.control, ScanPhase::Discovering);
                if !self.control.checkpoint() {
                    return None;
                }
                match metadata {
                    Ok(metadata) => {
                        if self.options.one_file_system && metadata_device(&metadata) != device {
                            None
                        } else {
                            let mut node = node_from_metadata(path, Some(parent), &metadata);
                            if node.kind != EntryKind::Directory {
                                node.scan_duration_ns = duration_ns(metadata_started.elapsed());
                            }
                            Some(node)
                        }
                    }
                    Err(error) => {
                        let error = Some(format!("{}: {error}", path.display()));
                        Some(Node {
                            path,
                            parent: Some(parent),
                            children: Vec::new(),
                            kind: EntryKind::Other,
                            logical_size: 0,
                            allocated_size: None,
                            files: 0,
                            directories: 0,
                            modified: None,
                            readonly: false,
                            permissions: None,
                            owner: None,
                            file_id: None,
                            error,
                            scan_duration_ns: 0,
                            depth_limited: false,
                            scan_failed: false,
                            scan_warning: false,
                        })
                    }
                }
            })
            .collect();
        self.control.checkpoint();
        let mut directories = Vec::new();
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let ScanState {
                snapshot,
                identities,
                directories: visited_directories,
                progress,
            } = &mut *state;
            let first = snapshot.nodes.len();
            for node in nodes {
                let directory = node.kind == EntryKind::Directory;
                let previous_owner = (self.control.live_enabled.load(Ordering::Acquire)
                    && node.kind == EntryKind::File)
                    .then_some(())
                    .and_then(|()| {
                        let previous = *identities.get(node.file_id.as_ref()?)?;
                        (previous < first && node.path < snapshot.nodes[previous].path).then_some((
                            snapshot.nodes[previous].parent,
                            snapshot.nodes[previous].allocated_size,
                        ))
                    });
                let Some(index) = insert_node(
                    snapshot,
                    identities,
                    visited_directories,
                    node,
                    self.control,
                ) else {
                    continue;
                };
                snapshot.nodes[parent].children.push(index);
                if let Some(progress) = progress.as_mut() {
                    progress.insert(&snapshot.nodes[index], parent);
                    if let Some((mut ancestor, Some(allocated))) = previous_owner {
                        while let Some(index) = ancestor {
                            let node = &mut snapshot.nodes[index];
                            node.allocated_size = node
                                .allocated_size
                                .map(|size| size.saturating_sub(allocated));
                            ancestor = node.parent;
                        }
                    }
                }
                if directory {
                    directories.push(index);
                }
            }
            if self.control.live_enabled.load(Ordering::Acquire)
                && let Some(progress) = progress.as_mut()
            {
                let mut totals = LiveTotals::default();
                for node in &snapshot.nodes[first..] {
                    totals.add(node);
                }
                totals.apply(snapshot, parent);
                progress.revision += 1;
            }
        }
        for index in directories {
            scope.spawn(move |scope| self.directory(scope, index, device, depth + 1));
        }
    }

    fn record_error(&self, index: usize, message: String) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.snapshot.nodes[index].error = Some(message.clone());
        state.snapshot.errors.push(message);
        if let Some(progress) = &mut state.progress {
            progress.fail(index);
        }
    }

    fn finish_directory(&self, index: usize) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let ScanState {
            snapshot, progress, ..
        } = &mut *state;
        if let Some(progress) = progress {
            progress.finish_listing(index);
            if !self.control.is_cancelled() {
                progress.finish(snapshot, index);
            }
        }
        #[cfg(test)]
        let completed = progress
            .as_ref()
            .is_some_and(|progress| progress.finished(index));
        #[cfg(test)]
        drop(state);
        #[cfg(test)]
        if completed {
            tests::gate(self.control, ScanPhase::Mapping);
        }
    }
}

fn insert_node(
    snapshot: &mut Snapshot,
    identities: &mut HashMap<String, usize>,
    directories: &mut HashSet<String>,
    mut node: Node,
    control: &ScanControl,
) -> Option<usize> {
    if node.kind == EntryKind::Directory
        && let Some(identity) = &node.file_id
        && !directories.insert(identity.clone())
    {
        return None;
    }
    let index = snapshot.nodes.len();
    if node.kind == EntryKind::File
        && let Some(identity) = &node.file_id
    {
        if let Some(&previous) = identities.get(identity) {
            if node.path < snapshot.nodes[previous].path {
                if let Some(allocated) = snapshot.nodes[previous].allocated_size {
                    control
                        .allocated_bytes
                        .fetch_sub(allocated, Ordering::Relaxed);
                }
                snapshot.nodes[previous].allocated_size =
                    snapshot.nodes[previous].allocated_size.map(|_| 0);
                identities.insert(identity.clone(), index);
            } else {
                node.allocated_size = node.allocated_size.map(|_| 0);
            }
        } else {
            identities.insert(identity.clone(), index);
        }
    }
    if let Some(error) = &node.error {
        snapshot.errors.push(error.clone());
    }
    update_progress(control, &node);
    snapshot.nodes.push(node);
    Some(index)
}

fn finalize(snapshot: &mut Snapshot, control: &ScanControl, refresh_statuses: bool) {
    let nodes = snapshot.nodes.len();
    let mut successes = if refresh_statuses {
        vec![false; nodes]
    } else {
        Vec::new()
    };
    control.start_work(
        nodes as u64
            + snapshot
                .nodes
                .iter()
                .map(|node| node.children.len() as u64)
                .sum::<u64>(),
    );
    control.set_phase(ScanPhase::Finalizing);
    for (position, index) in (0..nodes).rev().enumerate() {
        if position % 256 == 0 {
            control.checkpoint();
        }
        if snapshot.nodes[index].kind != EntryKind::Directory {
            if refresh_statuses {
                successes[index] = snapshot.nodes[index].error.is_none();
            }
            if (position + 1) % 256 == 0 || position + 1 == nodes {
                control.advance_work(((position % 256) + 1) as u64);
            }
            continue;
        }
        let mut logical = 0_u64;
        let mut allocated = empty_allocation();
        let mut files = 0_u64;
        let mut directories = 0_u64;
        let mut failed = snapshot.nodes[index].error.is_some();
        let mut succeeded = !failed
            && snapshot.nodes[index].children.is_empty()
            && !snapshot.nodes[index].depth_limited;
        for chunk in snapshot.nodes[index].children.chunks(256) {
            control.checkpoint();
            for &child in chunk {
                let node = &snapshot.nodes[child];
                logical = logical.saturating_add(node.logical_size);
                allocated = allocated
                    .zip(node.allocated_size)
                    .map(|(total, size)| total.saturating_add(size));
                files = files.saturating_add(node.files);
                directories = directories
                    .saturating_add(node.directories)
                    .saturating_add(u64::from(node.kind == EntryKind::Directory));
                if refresh_statuses {
                    failed |= node.error.is_some() || node.scan_failed || node.scan_warning;
                    succeeded |= successes[child];
                }
            }
            control.advance_work(chunk.len() as u64);
        }
        let node = &mut snapshot.nodes[index];
        node.logical_size = logical;
        node.allocated_size = allocated;
        node.files = files;
        node.directories = directories;
        if refresh_statuses {
            successes[index] = succeeded;
            node.scan_failed = failed && !succeeded;
            node.scan_warning = failed && succeeded;
        }
        if (position + 1) % 256 == 0 || position + 1 == nodes {
            control.advance_work(((position % 256) + 1) as u64);
        }
    }
    if control.is_cancelled() {
        return;
    }
    control.start_work(
        snapshot
            .nodes
            .iter()
            .map(|node| sort_work_total(node.children.len(), 1024))
            .sum(),
    );
    control.set_phase(ScanPhase::Sorting);
    for index in 0..nodes {
        if index % 256 == 0 && !control.checkpoint() {
            return;
        }
        let mut children = std::mem::take(&mut snapshot.nodes[index].children);
        sort_children(&mut children, &snapshot.nodes, control);
        snapshot.nodes[index].children = children;
    }
}

pub(crate) fn sort_work_total(length: usize, mut run: usize) -> u64 {
    let mut passes = 1;
    while run < length {
        passes += 1;
        run = run.saturating_mul(2);
    }
    (length as u64).saturating_mul(passes)
}

fn sort_children(children: &mut [usize], nodes: &[Node], control: &ScanControl) {
    const RUN: usize = 1024;
    let compare = |left: &usize, right: &usize| {
        nodes[*right]
            .logical_size
            .cmp(&nodes[*left].logical_size)
            .then_with(|| nodes[*left].path.cmp(&nodes[*right].path))
    };
    for run in children.chunks_mut(RUN) {
        if !control.checkpoint() {
            return;
        }
        run.sort_unstable_by(compare);
        control.advance_work(run.len() as u64);
    }
    if children.len() <= RUN {
        return;
    }
    let mut scratch = vec![0; children.len()];
    let mut width = RUN;
    while width < children.len() {
        for (run_index, run) in children.chunks(width * 2).enumerate() {
            if !control.checkpoint() {
                return;
            }
            let middle = width.min(run.len());
            let (mut left, mut right, mut output) = (0, middle, run_index * width * 2);
            while left < middle && right < run.len() {
                if output % 256 == 0 && !control.checkpoint() {
                    return;
                }
                if compare(&run[left], &run[right]).is_le() {
                    scratch[output] = run[left];
                    left += 1;
                } else {
                    scratch[output] = run[right];
                    right += 1;
                }
                output += 1;
                if output % 256 == 0 {
                    control.advance_work(256);
                }
            }
            let remaining = if left < middle {
                &run[left..middle]
            } else {
                &run[right..]
            };
            scratch[output..output + remaining.len()].copy_from_slice(remaining);
            control.advance_work((output % 256 + remaining.len()) as u64);
        }
        children.copy_from_slice(&scratch);
        width *= 2;
    }
}

fn update_progress(control: &ScanControl, node: &Node) {
    control.visited.fetch_add(1, Ordering::Relaxed);
    let _ = control
        .bytes
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |total| {
            Some(total.saturating_add(node.logical_size))
        });
    if let Some(allocated) = node.allocated_size {
        let _ = control
            .allocated_bytes
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |total| {
                Some(total.saturating_add(allocated))
            });
    } else {
        control.unknown_allocations.fetch_add(1, Ordering::Relaxed);
    }
}

fn node_from_metadata(path: PathBuf, parent: Option<usize>, metadata: &Metadata) -> Node {
    let kind = if is_link(metadata) {
        EntryKind::Symlink
    } else if metadata.is_dir() {
        EntryKind::Directory
    } else if metadata.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    };
    let (file_id, mut allocated_size) = if matches!(kind, EntryKind::File | EntryKind::Directory) {
        entry_storage(&path, metadata)
    } else {
        (metadata_identity(metadata), empty_allocation())
    };
    if kind != EntryKind::File {
        allocated_size = empty_allocation();
    }
    let (permissions, owner) = platform_permissions(metadata);
    Node {
        path,
        parent,
        children: Vec::new(),
        kind,
        logical_size: if kind == EntryKind::File {
            metadata.len()
        } else {
            0
        },
        allocated_size,
        files: u64::from(kind == EntryKind::File),
        directories: 0,
        modified: metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs()),
        readonly: metadata.permissions().readonly(),
        permissions,
        owner,
        file_id,
        error: None,
        scan_duration_ns: 0,
        depth_limited: false,
        scan_failed: false,
        scan_warning: false,
    }
}

pub(crate) fn metadata_identity(metadata: &Metadata) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(format!("{}:{}", metadata.dev(), metadata.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

fn directory_matches(path: &Path, identity: Option<&str>) -> std::io::Result<bool> {
    #[cfg(windows)]
    {
        let (file, metadata) = open_metadata_file(path)?;
        Ok(metadata.is_dir() && file_identity(&file, &metadata).as_deref() == identity)
    }
    #[cfg(not(windows))]
    {
        let metadata = fs::symlink_metadata(path)?;
        Ok(metadata.is_dir()
            && !is_link(&metadata)
            && path_identity(path, &metadata).as_deref() == identity)
    }
}

pub(crate) fn path_identity(path: &Path, metadata: &Metadata) -> Option<String> {
    #[cfg(windows)]
    {
        let (file, opened) = open_metadata_file(path).ok()?;
        if opened.is_dir() != metadata.is_dir()
            || opened.len() != metadata.len()
            || opened.modified().ok() != metadata.modified().ok()
        {
            return None;
        }
        file_identity(&file, &opened)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        metadata_identity(metadata)
    }
}

pub(crate) fn file_identity(file: &fs::File, metadata: &Metadata) -> Option<String> {
    #[cfg(windows)]
    {
        if is_link(metadata) {
            return None;
        }
        let information = winapi_util::file::information(file).ok()?;
        if information.file_attributes() & 0x400 != 0 || information.file_index() == 0 {
            return None;
        }
        Some(format!(
            "win:{}:{}",
            information.volume_serial_number(),
            information.file_index()
        ))
    }
    #[cfg(not(windows))]
    {
        let _ = file;
        metadata_identity(metadata)
    }
}

fn entry_storage(path: &Path, metadata: &Metadata) -> (Option<String>, Option<u64>) {
    #[cfg(windows)]
    {
        let Ok((file, opened)) = open_metadata_file(path) else {
            return (None, None);
        };
        if opened.is_dir() != metadata.is_dir()
            || opened.len() != metadata.len()
            || opened.modified().ok() != metadata.modified().ok()
        {
            return (None, None);
        }
        let identity = file_identity(&file, &opened);
        let allocated = if opened.is_file() {
            identity
                .as_ref()
                .and_then(|_| fs2::FileExt::allocated_size(&file).ok())
        } else {
            Some(0)
        };
        (identity, allocated)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        (metadata_identity(metadata), allocated_bytes(metadata))
    }
}

#[cfg(windows)]
fn open_metadata_file(path: &Path) -> std::io::Result<(fs::File, Metadata)> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_FLAG_OPEN_NO_RECALL: u32 = 0x0010_0000;
    let file = fs::OpenOptions::new()
        .access_mode(0)
        .custom_flags(
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_OPEN_NO_RECALL,
        )
        .open(path)?;
    let metadata = file.metadata()?;
    if is_link(&metadata) {
        return Err(std::io::Error::other("Reparse points are not followed"));
    }
    Ok((file, metadata))
}

fn metadata_device(metadata: &Metadata) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(metadata.dev())
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

fn platform_permissions(metadata: &Metadata) -> (Option<u32>, Option<u32>) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (Some(metadata.mode()), Some(metadata.uid()))
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        (None, None)
    }
}

#[cfg(not(windows))]
fn allocated_bytes(metadata: &Metadata) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(metadata.blocks().saturating_mul(512))
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

fn empty_allocation() -> Option<u64> {
    if cfg!(any(unix, windows)) {
        Some(0)
    } else {
        None
    }
}

fn is_link(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashMap;
    use std::fs;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Barrier, Mutex, OnceLock, mpsc};

    use super::*;

    #[test]
    fn scan_depth_limit_stops_before_listing_boundary_directories_without_errors() {
        let temporary = tempfile::tempdir().unwrap();
        fs::create_dir_all(temporary.path().join("child/grandchild")).unwrap();
        fs::write(temporary.path().join("root-file"), [7; 3]).unwrap();
        fs::write(temporary.path().join("child/child-file"), [7; 5]).unwrap();
        fs::write(temporary.path().join("child/grandchild/deep-file"), [7; 11]).unwrap();
        let control = ScanControl::default();
        control.enable_live_view();
        for (max_depth, bytes, entries, limited) in [
            (Some(0), 0, 1, 1),
            (Some(1), 3, 3, 1),
            (Some(2), 8, 5, 1),
            (None, 19, 6, 0),
        ] {
            let snapshot = Arc::new(
                scan(
                    &[temporary.path().into()],
                    &ScanOptions {
                        max_depth,
                        ..ScanOptions::default()
                    },
                    &control,
                )
                .unwrap(),
            );
            assert_eq!(snapshot.total_size(false), bytes);
            assert_eq!(snapshot.nodes.len(), entries);
            assert_eq!(control.depth_limited.load(Ordering::Relaxed), limited);
            assert_eq!(
                snapshot
                    .nodes
                    .iter()
                    .filter(|node| node.depth_limited)
                    .count() as u64,
                limited
            );
            assert!(snapshot.errors.is_empty());
            assert!(!snapshot.cancelled);
            assert!(snapshot.validate().is_ok());
            control.set_completed_live_snapshot(snapshot);
            assert_eq!(
                control.with_live_view(|view| view.directory_status(0)),
                Some(Some(DirectoryScanStatus::Complete))
            );
        }
    }

    #[test]
    fn scan_depth_limit_is_relative_to_each_root_and_keeps_file_roots() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("directory");
        let file = temporary.path().join("file");
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("hidden-file"), [7; 9]).unwrap();
        fs::write(&file, [7; 3]).unwrap();
        let snapshot = scan(
            &[directory, file],
            &ScanOptions {
                max_depth: Some(0),
                ..ScanOptions::default()
            },
            &ScanControl::default(),
        )
        .unwrap();
        assert_eq!(snapshot.roots.len(), 2);
        assert_eq!(snapshot.total_size(false), 3);
        assert_eq!(snapshot.nodes.len(), 2);
        assert!(snapshot.validate().is_ok());
    }

    #[test]
    fn subtree_refresh_keeps_depth_limit_relative_to_the_original_root() {
        let temporary = tempfile::tempdir().unwrap();
        let selected = temporary.path().join("child");
        fs::create_dir_all(selected.join("grandchild")).unwrap();
        fs::write(selected.join("data"), [7; 5]).unwrap();
        fs::write(selected.join("grandchild/deep-data"), [7; 11]).unwrap();
        let options = ScanOptions {
            max_depth: Some(2),
            ..ScanOptions::default()
        };
        let original = scan(
            &[temporary.path().into()],
            &options,
            &ScanControl::default(),
        )
        .unwrap();
        let selected = original.nodes[original.roots[0]].children[0];
        let refreshed =
            refresh_subtree(&original, selected, &options, &ScanControl::default()).unwrap();
        let entries = |snapshot: &Snapshot| {
            snapshot
                .nodes
                .iter()
                .map(|node| (node.path.clone(), node.logical_size, node.depth_limited))
                .collect::<std::collections::BTreeSet<_>>()
        };
        assert_eq!(entries(&refreshed), entries(&original));
        assert_eq!(refreshed.total_size(false), 5);
        assert!(refreshed.validate().is_ok());
    }

    #[test]
    fn subtree_refresh_clears_ancestor_warning_after_the_failed_subtree_recovers() {
        let temporary = tempfile::tempdir().unwrap();
        fs::create_dir(temporary.path().join("child")).unwrap();
        fs::write(temporary.path().join("child/data"), b"data").unwrap();
        let options = ScanOptions::default();
        let mut original = scan(
            &[temporary.path().into()],
            &options,
            &ScanControl::default(),
        )
        .unwrap();
        let child = original.nodes[0].children[0];
        let error = format!("{}: listing failed", original.nodes[child].path.display());
        original.nodes[child].error = Some(error.clone());
        original.nodes[child].scan_warning = true;
        original.nodes[0].scan_warning = true;
        original.errors.push(error);
        let refreshed =
            refresh_subtree(&original, child, &options, &ScanControl::default()).unwrap();
        assert!(refreshed.errors.is_empty());
        assert!(
            refreshed
                .nodes
                .iter()
                .all(|node| !node.scan_failed && !node.scan_warning)
        );
    }

    #[test]
    fn completed_scan_releases_unused_node_capacity() {
        let temporary = tempfile::tempdir().unwrap();
        for index in 0..64 {
            fs::write(temporary.path().join(format!("file-{index}")), [7; 8]).unwrap();
        }
        let snapshot = scan(
            &[temporary.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        assert_eq!(snapshot.nodes.len(), 65);
        assert_eq!(snapshot.nodes.capacity(), snapshot.nodes.len());
        assert_eq!(
            (snapshot.file_count(), snapshot.total_size(false)),
            (64, 512)
        );
        assert!(snapshot.validate().is_ok());
    }

    #[test]
    fn live_view_exposes_root_before_discovery_and_retains_final_snapshot_by_arc() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join("data"), [7; 10]).unwrap();
        let control = ScanControl::default();
        control.enable_live_view();
        let gate = register_gate(&control, ScanPhase::Discovering);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                scan(
                    &[temporary.path().into()],
                    &ScanOptions {
                        threads: 1,
                        ..ScanOptions::default()
                    },
                    &control,
                )
                .unwrap()
            });
            gate.arrived.wait();
            let visible = control.with_live_view(|view| {
                (
                    view.snapshot.nodes.len(),
                    view.directory_finished(view.snapshot.roots[0]),
                    view.scan_duration(view.snapshot.roots[0]).is_some(),
                    view.directory_status(view.snapshot.roots[0]),
                )
            });
            gate.release.wait();
            assert_eq!(
                visible,
                Some((1, false, true, Some(DirectoryScanStatus::Scanning)))
            );
            let snapshot = Arc::new(worker.join().unwrap());
            control.set_completed_live_snapshot(Arc::clone(&snapshot));
            control
                .with_live_view(|view| {
                    assert!(std::ptr::eq(view.snapshot, snapshot.as_ref()));
                    assert!(view.directory_finished(view.snapshot.roots[0]));
                    assert_eq!(view.snapshot.total_size(false), 10);
                })
                .unwrap();
        });
    }

    #[test]
    fn live_view_cancelled_directory_remains_incomplete_after_handoff() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join("data"), [7; 10]).unwrap();
        let control = ScanControl::default();
        control.enable_live_view();
        let gate = register_gate(&control, ScanPhase::Discovering);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                scan(
                    &[temporary.path().into()],
                    &ScanOptions {
                        threads: 1,
                        ..ScanOptions::default()
                    },
                    &control,
                )
                .unwrap()
            });
            gate.arrived.wait();
            control.cancel();
            gate.release.wait();
            let snapshot = Arc::new(worker.join().unwrap());
            control.set_completed_live_snapshot(snapshot);
            assert_eq!(
                control.with_live_view(|view| view.directory_finished(view.snapshot.roots[0])),
                Some(false)
            );
            assert_eq!(
                control.with_live_view(|view| view.directory_status(view.snapshot.roots[0])),
                Some(Some(DirectoryScanStatus::Cancelled))
            );
        });
    }

    #[test]
    fn live_view_keeps_parent_pending_until_every_descendant_finishes() {
        let temporary = tempfile::tempdir().unwrap();
        fs::create_dir(temporary.path().join("child")).unwrap();
        fs::write(temporary.path().join("child/data"), [7; 10]).unwrap();
        let control = ScanControl::default();
        control.enable_live_view();
        let gate = register_directory_gate(&control, 1);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                scan(
                    &[temporary.path().into()],
                    &ScanOptions {
                        threads: 1,
                        ..ScanOptions::default()
                    },
                    &control,
                )
                .unwrap()
            });
            gate.arrived.wait();
            let pending = control.with_live_view(|view| {
                (
                    view.snapshot.nodes.len(),
                    view.directory_finished(0),
                    view.directory_finished(1),
                    view.snapshot.nodes[0].directories,
                    view.directory_status(0),
                    view.directory_status(1),
                )
            });
            let active = register_gate(&control, ScanPhase::Discovering);
            gate.release.wait();
            active.arrived.wait();
            let scanning =
                control.with_live_view(|view| (view.directory_status(0), view.directory_status(1)));
            active.release.wait();
            assert_eq!(
                pending,
                Some((
                    2,
                    false,
                    false,
                    1,
                    Some(DirectoryScanStatus::Queued),
                    Some(DirectoryScanStatus::Queued)
                ))
            );
            assert_eq!(
                scanning,
                Some((
                    Some(DirectoryScanStatus::Scanning),
                    Some(DirectoryScanStatus::Scanning)
                ))
            );
            let snapshot = Arc::new(worker.join().unwrap());
            control.set_completed_live_snapshot(snapshot);
            assert_eq!(control.with_live_view(|view| view.directory_finished(0) && view.directory_finished(1)), Some(true));
            assert_eq!(
                control.with_live_view(|view| view.directory_status(0)),
                Some(Some(DirectoryScanStatus::Complete))
            );
        });
    }

    #[test]
    fn scan_records_file_metadata_and_completed_subtree_durations_without_live_view() {
        let temporary = tempfile::tempdir().unwrap();
        fs::create_dir(temporary.path().join("child")).unwrap();
        fs::write(temporary.path().join("child/data"), [7; 10]).unwrap();
        let snapshot = scan(
            &[temporary.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        assert!(snapshot.nodes.iter().all(|node| node.scan_duration_ns > 0));
        assert!(
            snapshot
                .nodes
                .iter()
                .all(|node| node.scan_duration_ns <= snapshot.nodes[0].scan_duration_ns)
        );
    }

    #[test]
    fn live_view_marks_completed_child_while_parent_and_sibling_are_pending() {
        let temporary = tempfile::tempdir().unwrap();
        for name in ["first", "second"] {
            fs::create_dir(temporary.path().join(name)).unwrap();
            fs::write(temporary.path().join(name).join("data"), [7; 10]).unwrap();
        }
        let control = ScanControl::default();
        control.enable_live_view();
        let gate = register_gate(&control, ScanPhase::Mapping);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                scan(
                    &[temporary.path().into()],
                    &ScanOptions {
                        threads: 1,
                        ..ScanOptions::default()
                    },
                    &control,
                )
                .unwrap()
            });
            gate.arrived.wait();
            let visible = control.with_live_view(|view| {
                (
                    view.directory_finished(0),
                    view.snapshot.nodes[0]
                        .children
                        .iter()
                        .filter(|&&child| view.directory_finished(child))
                        .count(),
                    view.snapshot.nodes[0].logical_size,
                    view.directory_status(0),
                )
            });
            gate.release.wait();
            assert_eq!(
                visible,
                Some((false, 1, 10, Some(DirectoryScanStatus::Queued)))
            );
            worker.join().unwrap();
        });
    }

    #[test]
    fn live_view_does_not_complete_ancestors_of_a_failed_directory() {
        let temporary = tempfile::tempdir().unwrap();
        let child = temporary.path().join("child");
        fs::create_dir(&child).unwrap();
        let control = ScanControl::default();
        control.enable_live_view();
        let gate = register_directory_gate(&control, 1);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                scan(
                    &[temporary.path().into()],
                    &ScanOptions {
                        threads: 1,
                        ..ScanOptions::default()
                    },
                    &control,
                )
                .unwrap()
            });
            gate.arrived.wait();
            let removed = fs::remove_dir(&child);
            gate.release.wait();
            removed.unwrap();
            let snapshot = Arc::new(worker.join().unwrap());
            assert!(!snapshot.errors.is_empty());
            control.set_completed_live_snapshot(snapshot);
            assert_eq!(control.with_live_view(|view| view.directory_finished(0) || view.directory_finished(1)), Some(false));
            assert_eq!(
                control.with_live_view(|view| view.directory_failed(0) && view.directory_failed(1)),
                Some(true)
            );
            assert_eq!(
                control.with_live_view(|view| (view.directory_status(0), view.directory_status(1))),
                Some((
                    Some(DirectoryScanStatus::Failed),
                    Some(DirectoryScanStatus::Failed)
                ))
            );
        });
    }

    #[test]
    fn live_view_totals_match_final_totals_before_finalization_with_hardlinks() {
        let temporary = tempfile::tempdir().unwrap();
        for name in ["a", "b", "c"] {
            fs::create_dir(temporary.path().join(name)).unwrap();
        }
        fs::write(temporary.path().join("a/data"), [7; 100]).unwrap();
        fs::hard_link(
            temporary.path().join("a/data"),
            temporary.path().join("c/alias"),
        )
        .unwrap();
        fs::write(temporary.path().join("b/data"), [7; 50]).unwrap();
        let control = ScanControl::default();
        control.enable_live_view();
        let gate = register_gate(&control, ScanPhase::Finalizing);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                scan(
                    &[temporary.path().into()],
                    &ScanOptions {
                        threads: 1,
                        ..ScanOptions::default()
                    },
                    &control,
                )
                .unwrap()
            });
            gate.arrived.wait();
            let completed = control.with_live_view(|view| view.directory_finished(0));
            let live = control.with_live_view(|view| {
                view.snapshot
                    .nodes
                    .iter()
                    .map(|node| {
                        (
                            node.logical_size,
                            node.allocated_size,
                            node.files,
                            node.directories,
                        )
                    })
                    .collect::<Vec<_>>()
            });
            gate.release.wait();
            assert_eq!(completed, Some(true));
            let snapshot = worker.join().unwrap();
            let finalized = snapshot
                .nodes
                .iter()
                .map(|node| {
                    (
                        node.logical_size,
                        node.allocated_size,
                        node.files,
                        node.directories,
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(live, Some(finalized));
        });
    }

    #[test]
    fn work_progress_advances_monotonically_without_changing_scan_counters() {
        let control = ScanControl::default();
        control.visited.store(17, Ordering::Relaxed);
        control.bytes.store(1234, Ordering::Relaxed);
        control.start_work(10);
        control.set_phase(ScanPhase::Indexing);
        control.advance_work(3);
        assert_eq!(control.work_done.load(Ordering::Relaxed), 3);
        control.advance_work(4);
        assert_eq!(control.work_done.load(Ordering::Relaxed), 7);
        assert_eq!(control.work_total.load(Ordering::Relaxed), 10);
        assert_eq!(control.visited.load(Ordering::Relaxed), 17);
        assert_eq!(control.bytes.load(Ordering::Relaxed), 1234);
        control.start_work(5);
        assert_eq!(control.work_done.load(Ordering::Relaxed), 0);
        assert_eq!(control.phase(), ScanPhase::Indexing);
    }

    #[test]
    fn scan_sorting_reports_all_completed_work() {
        let directory = tempfile::tempdir().unwrap();
        for index in 0..8 {
            fs::write(directory.path().join(format!("file-{index}")), [7; 10]).unwrap();
        }
        let control = ScanControl::default();
        scan(
            &[directory.path().into()],
            &ScanOptions::default(),
            &control,
        )
        .unwrap();
        assert_eq!(control.work_total.load(Ordering::Relaxed), 8);
        assert_eq!(control.work_done.load(Ordering::Relaxed), 8);
    }

    #[test]
    fn scan_reports_the_last_directory_and_resets_it_for_each_scan() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let file = first.path().join("data");
        fs::write(&file, b"content").unwrap();
        let control = ScanControl::default();
        assert_eq!(control.current_path(), None);
        for directory in [first.path(), second.path()] {
            let snapshot = scan(&[directory.into()], &ScanOptions::default(), &control).unwrap();
            assert_eq!(
                control.current_path().as_ref(),
                Some(&snapshot.nodes[snapshot.roots[0]].path)
            );
        }
        scan(&[file], &ScanOptions::default(), &control).unwrap();
        assert_eq!(control.current_path(), None);
    }

    #[derive(Debug)]
    pub(crate) struct Gate {
        pub(crate) arrived: Barrier,
        pub(crate) release: Barrier,
    }

    type GateMap = HashMap<usize, (ScanPhase, Arc<Gate>)>;
    type DirectoryGateMap = HashMap<usize, (usize, Arc<Gate>)>;

    fn directory_gates() -> &'static Mutex<DirectoryGateMap> {
        static GATES: OnceLock<Mutex<DirectoryGateMap>> = OnceLock::new();
        GATES.get_or_init(Mutex::default)
    }

    fn register_directory_gate(control: &ScanControl, index: usize) -> Arc<Gate> {
        let gate = Arc::new(Gate {
            arrived: Barrier::new(2),
            release: Barrier::new(2),
        });
        directory_gates().lock().unwrap().insert(
            control as *const ScanControl as usize,
            (index, Arc::clone(&gate)),
        );
        gate
    }

    pub(super) fn directory_gate(control: &ScanControl, index: usize) {
        let gate = {
            let mut gates = directory_gates().lock().unwrap();
            let key = control as *const ScanControl as usize;
            if gates
                .get(&key)
                .is_some_and(|(expected, _)| *expected == index)
            {
                gates.remove(&key).map(|(_, gate)| gate)
            } else {
                None
            }
        };
        if let Some(gate) = gate {
            gate.arrived.wait();
            gate.release.wait();
        }
    }

    fn gates() -> &'static Mutex<GateMap> {
        static GATES: OnceLock<Mutex<GateMap>> = OnceLock::new();
        GATES.get_or_init(Mutex::default)
    }

    pub(crate) fn register_gate(control: &ScanControl, phase: ScanPhase) -> Arc<Gate> {
        let gate = Arc::new(Gate {
            arrived: Barrier::new(2),
            release: Barrier::new(2),
        });
        gates().lock().unwrap().insert(
            control as *const ScanControl as usize,
            (phase, Arc::clone(&gate)),
        );
        gate
    }

    pub(super) fn gate(control: &ScanControl, phase: ScanPhase) {
        let gate = {
            let mut gates = gates().lock().unwrap();
            let key = control as *const ScanControl as usize;
            if gates
                .get(&key)
                .is_some_and(|(expected, _)| *expected == phase)
            {
                gates.remove(&key).map(|(_, gate)| gate)
            } else {
                None
            }
        };
        if let Some(gate) = gate {
            gate.arrived.wait();
            gate.release.wait();
        }
    }

    fn wait_for_paused_worker(control: &ScanControl) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while control.waiting.load(Ordering::Acquire) == 0 {
            assert!(
                Instant::now() < deadline,
                "worker did not acknowledge pause"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn running_scan_pauses_and_resumes_during_discovery_and_finalization() {
        for phase in [ScanPhase::Discovering, ScanPhase::Finalizing] {
            let temporary = tempfile::tempdir().unwrap();
            for index in 0..8 {
                fs::write(temporary.path().join(format!("file-{index}")), [7; 10]).unwrap();
            }
            let control = ScanControl::default();
            let gate = register_gate(&control, phase);
            std::thread::scope(|scope| {
                let (sender, receiver) = mpsc::channel();
                let path = temporary.path().to_owned();
                let control_ref = &control;
                scope.spawn(move || {
                    let result = scan(
                        &[path],
                        &ScanOptions {
                            threads: 1,
                            ..ScanOptions::default()
                        },
                        control_ref,
                    );
                    sender.send(result).unwrap();
                });
                gate.arrived.wait();
                control.pause();
                gate.release.wait();
                wait_for_paused_worker(&control);
                assert_eq!(control.phase(), phase);
                assert!(receiver.try_recv().is_err());
                control.resume();
                let snapshot = receiver
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .unwrap();
                assert_eq!(snapshot.file_count(), 8);
                assert!(snapshot.validate().is_ok());
                assert_eq!(control.phase(), ScanPhase::Complete);
            });
        }
    }

    #[test]
    fn running_scan_cancels_while_paused_and_returns_valid_partial_results() {
        for phase in [ScanPhase::Discovering, ScanPhase::Finalizing] {
            let temporary = tempfile::tempdir().unwrap();
            for index in 0..8 {
                fs::write(temporary.path().join(format!("file-{index}")), [7; 10]).unwrap();
            }
            let control = ScanControl::default();
            let gate = register_gate(&control, phase);
            std::thread::scope(|scope| {
                let (sender, receiver) = mpsc::channel();
                let path = temporary.path().to_owned();
                let control_ref = &control;
                scope.spawn(move || {
                    sender
                        .send(scan(
                            &[path],
                            &ScanOptions {
                                threads: 1,
                                ..ScanOptions::default()
                            },
                            control_ref,
                        ))
                        .unwrap();
                });
                gate.arrived.wait();
                control.pause();
                gate.release.wait();
                wait_for_paused_worker(&control);
                control.cancel();
                let snapshot = receiver
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .unwrap();
                assert!(snapshot.cancelled);
                assert!(snapshot.validate().is_ok());
                if phase == ScanPhase::Discovering {
                    assert!(snapshot.file_count() < 8);
                }
            });
        }
    }

    #[test]
    fn one_and_many_threads_produce_equivalent_totals_and_hardlink_allocation() {
        let temporary = tempfile::tempdir().unwrap();
        for directory in 0..4 {
            let path = temporary.path().join(format!("directory-{directory}"));
            fs::create_dir(&path).unwrap();
            for index in 0..40 {
                fs::write(path.join(format!("file-{index}")), vec![7; index + 1]).unwrap();
            }
        }
        fs::hard_link(
            temporary.path().join("directory-0/file-1"),
            temporary.path().join("directory-1/alias"),
        )
        .unwrap();
        let run = |threads| {
            scan(
                &[temporary.path().to_owned()],
                &ScanOptions {
                    threads,
                    ..ScanOptions::default()
                },
                &ScanControl::default(),
            )
            .unwrap()
        };
        let single = run(1);
        let parallel = run(4);
        assert_eq!(
            (
                single.total_size(false),
                single.total_size(true),
                single.file_count(),
                single.directory_count()
            ),
            (
                parallel.total_size(false),
                parallel.total_size(true),
                parallel.file_count(),
                parallel.directory_count()
            )
        );
        let paths = |snapshot: &Snapshot| {
            snapshot
                .nodes
                .iter()
                .map(|node| {
                    (
                        node.path.clone(),
                        node.logical_size,
                        node.allocated_size,
                        node.files,
                        node.directories,
                    )
                })
                .collect::<std::collections::BTreeSet<_>>()
        };
        assert_eq!(paths(&single), paths(&parallel));
        assert!(single.validate().is_ok());
        assert!(parallel.validate().is_ok());
    }

    #[test]
    fn cancellable_sort_merges_wide_directories_without_losing_entries() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("template");
        fs::write(&path, b"template").unwrap();
        let template = scan(
            &[path],
            &ScanOptions {
                threads: 1,
                ..ScanOptions::default()
            },
            &ScanControl::default(),
        )
        .unwrap()
        .nodes
        .remove(0);
        let nodes: Vec<_> = (0..4097)
            .map(|index| {
                let mut node = template.clone();
                node.path = temporary.path().join(format!("file-{index:04}"));
                node.logical_size = ((4096 - index) % 97) as u64;
                node
            })
            .collect();
        let mut children: Vec<_> = (0..nodes.len()).rev().collect();
        let control = ScanControl::default();
        control.start_work(sort_work_total(children.len(), 1024));
        sort_children(&mut children, &nodes, &control);
        assert_eq!(control.work_done.load(Ordering::Relaxed), 16388);
        assert_eq!(control.work_total.load(Ordering::Relaxed), 16388);
        assert_eq!(
            children
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            4097
        );
        assert!(children.windows(2).all(|pair| {
            let left = &nodes[pair[0]];
            let right = &nodes[pair[1]];
            left.logical_size > right.logical_size
                || (left.logical_size == right.logical_size && left.path <= right.path)
        }));
    }

    #[test]
    fn subtree_refresh_preserves_other_roots_and_reassigns_hardlink_allocation() {
        let temporary = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let selected = temporary.path().join("a-selected");
        let retained = temporary.path().join("z-retained");
        fs::create_dir(&selected).unwrap();
        fs::create_dir(&retained).unwrap();
        fs::write(selected.join("original"), [7; 8192]).unwrap();
        fs::hard_link(selected.join("original"), retained.join("alias")).unwrap();
        fs::write(other.path().join("unrelated"), [4; 7]).unwrap();
        let roots = vec![temporary.path().to_owned(), other.path().to_owned()];
        let options = ScanOptions {
            threads: 2,
            ..ScanOptions::default()
        };
        let original = scan(&roots, &options, &ScanControl::default()).unwrap();
        let selected = fs::canonicalize(selected).unwrap();
        let index = original
            .nodes
            .iter()
            .position(|node| node.path == selected)
            .unwrap();
        fs::remove_file(selected.join("original")).unwrap();
        fs::write(selected.join("new"), [2; 5]).unwrap();
        let refreshed =
            refresh_subtree(&original, index, &options, &ScanControl::default()).unwrap();
        let complete = scan(&roots, &options, &ScanControl::default()).unwrap();
        let values = |snapshot: &Snapshot| {
            snapshot
                .nodes
                .iter()
                .map(|node| {
                    (
                        node.path.clone(),
                        node.logical_size,
                        node.allocated_size,
                        node.files,
                        node.directories,
                    )
                })
                .collect::<std::collections::BTreeSet<_>>()
        };
        assert_eq!(values(&refreshed), values(&complete));
        assert_eq!(refreshed.roots.len(), 2);
        assert!(refreshed.validate().is_ok());
        assert_eq!(original.file_count(), 3);
    }

    #[test]
    fn subtree_refresh_accepts_directory_roots_and_rejects_invalid_indices_and_files() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join("data"), [2; 5]).unwrap();
        let options = ScanOptions {
            threads: 1,
            ..ScanOptions::default()
        };
        let original = scan(
            &[temporary.path().to_owned()],
            &options,
            &ScanControl::default(),
        )
        .unwrap();
        assert!(refresh_subtree(&original, usize::MAX, &options, &ScanControl::default()).is_err());
        let file = original
            .nodes
            .iter()
            .position(|node| node.kind == EntryKind::File)
            .unwrap();
        assert!(refresh_subtree(&original, file, &options, &ScanControl::default()).is_err());
        fs::write(temporary.path().join("new"), [2; 6]).unwrap();
        let refreshed = refresh_subtree(
            &original,
            original.roots[0],
            &options,
            &ScanControl::default(),
        )
        .unwrap();
        assert_eq!(
            (refreshed.total_size(false), refreshed.file_count()),
            (11, 2)
        );
        assert!(refreshed.validate().is_ok());
    }

    #[test]
    fn cancelling_subtree_refresh_keeps_the_original_snapshot() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join("data"), [2; 5]).unwrap();
        let options = ScanOptions {
            threads: 1,
            ..ScanOptions::default()
        };
        let original = scan(
            &[temporary.path().to_owned()],
            &options,
            &ScanControl::default(),
        )
        .unwrap();
        let control = ScanControl::default();
        let gate = register_gate(&control, ScanPhase::Discovering);
        std::thread::scope(|scope| {
            let (sender, receiver) = mpsc::channel();
            let original_ref = &original;
            let control_ref = &control;
            scope.spawn(move || {
                sender
                    .send(refresh_subtree(
                        original_ref,
                        original_ref.roots[0],
                        &options,
                        control_ref,
                    ))
                    .unwrap();
            });
            gate.arrived.wait();
            control.cancel();
            gate.release.wait();
            assert!(
                receiver
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .is_err()
            );
        });
        assert_eq!((original.total_size(false), original.file_count()), (5, 1));
        assert!(original.validate().is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn subtree_refresh_refuses_a_directory_replaced_by_a_symlink() {
        use std::os::unix::fs::symlink;
        let temporary = tempfile::tempdir().unwrap();
        let selected = temporary.path().join("selected");
        fs::create_dir(&selected).unwrap();
        let options = ScanOptions {
            threads: 1,
            ..ScanOptions::default()
        };
        let original = scan(
            &[temporary.path().to_owned()],
            &options,
            &ScanControl::default(),
        )
        .unwrap();
        let index = original
            .nodes
            .iter()
            .position(|node| node.name() == "selected")
            .unwrap();
        fs::remove_dir(&selected).unwrap();
        symlink(temporary.path(), selected).unwrap();
        assert!(refresh_subtree(&original, index, &options, &ScanControl::default()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn subtree_refresh_keeps_previous_results_when_the_selected_directory_is_unreadable() {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempfile::tempdir().unwrap();
        let selected = temporary.path().join("selected");
        fs::create_dir(&selected).unwrap();
        fs::write(selected.join("data"), [2; 5]).unwrap();
        let options = ScanOptions {
            threads: 1,
            ..ScanOptions::default()
        };
        let original = scan(
            &[temporary.path().to_owned()],
            &options,
            &ScanControl::default(),
        )
        .unwrap();
        let index = original
            .nodes
            .iter()
            .position(|node| node.name() == "selected")
            .unwrap();
        fs::set_permissions(&selected, fs::Permissions::from_mode(0o0)).unwrap();
        let inaccessible = fs::read_dir(&selected).is_err();
        let refreshed = refresh_subtree(&original, index, &options, &ScanControl::default());
        fs::set_permissions(selected, fs::Permissions::from_mode(0o700)).unwrap();
        if inaccessible {
            assert!(refreshed.is_err());
        }
        assert_eq!(original.file_count(), 1);
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn file_identity_matches_paths_and_hardlinks() {
        let temporary = tempfile::tempdir().unwrap();
        let original = temporary.path().join("original");
        let alias = temporary.path().join("alias");
        fs::write(&original, b"contents").unwrap();
        fs::hard_link(&original, &alias).unwrap();
        let file = fs::File::open(&original).unwrap();
        let metadata = file.metadata().unwrap();
        let original_id = path_identity(&original, &metadata).expect("identity is available");
        assert_eq!(file_identity(&file, &metadata), Some(original_id.clone()));
        assert_eq!(
            path_identity(&alias, &fs::symlink_metadata(&alias).unwrap()),
            Some(original_id)
        );
    }
    #[cfg(windows)]
    #[test]
    fn descent_rejects_a_directory_replaced_with_another_directory() {
        let temporary = tempfile::tempdir().unwrap();
        let selected = temporary.path().join("selected");
        fs::create_dir(&selected).unwrap();
        let control = ScanControl::default();
        let gate = register_directory_gate(&control, 1);
        let snapshot = std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                scan(
                    &[temporary.path().into()],
                    &ScanOptions {
                        threads: 1,
                        ..ScanOptions::default()
                    },
                    &control,
                )
                .unwrap()
            });
            gate.arrived.wait();
            let replaced = (|| -> std::io::Result<()> {
                fs::rename(&selected, temporary.path().join("original"))?;
                fs::create_dir(&selected)?;
                fs::write(selected.join("replacement-data"), [7; 8192])
            })();
            gate.release.wait();
            let snapshot = worker.join().unwrap();
            replaced.unwrap();
            snapshot
        });
        assert_eq!((snapshot.file_count(), snapshot.total_size(false)), (0, 0));
        assert!(
            snapshot
                .errors
                .iter()
                .any(|error| error.contains("directory changed during scan"))
        );
        assert!(snapshot.validate().is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn scan_reads_windows_directories_with_nonresident_directory_data() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("wide");
        fs::create_dir(&directory).unwrap();
        for index in 0..128 {
            fs::write(directory.join(format!("file-{index:04}")), [7; 8]).unwrap();
        }
        let snapshot = scan(
            &[temporary.path().to_owned()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        assert_eq!(
            (snapshot.file_count(), snapshot.total_size(false)),
            (128, 1024)
        );
        assert!(snapshot.errors.is_empty(), "{:?}", snapshot.errors);
        assert!(snapshot.validate().is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn scan_rejects_a_directory_replaced_by_a_file_after_enumeration() {
        let temporary = tempfile::tempdir().unwrap();
        let selected = temporary.path().join("selected");
        fs::create_dir(&selected).unwrap();
        let control = ScanControl::default();
        let gate = register_gate(&control, ScanPhase::Discovering);
        let snapshot = std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                scan(
                    &[temporary.path().to_owned()],
                    &ScanOptions {
                        threads: 1,
                        ..ScanOptions::default()
                    },
                    &control,
                )
            });
            gate.arrived.wait();
            fs::remove_dir(&selected).unwrap();
            fs::write(&selected, [7; 8192]).unwrap();
            gate.release.wait();
            worker.join().unwrap().unwrap()
        });
        assert_eq!((snapshot.file_count(), snapshot.total_size(false)), (0, 0));
        assert!(!snapshot.errors.is_empty());
        assert!(snapshot.validate().is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn scan_uses_windows_allocation_and_deduplicates_hardlinks() {
        let temporary = tempfile::tempdir().unwrap();
        let original = temporary.path().join("original");
        fs::write(&original, [7; 8192]).unwrap();
        fs::hard_link(&original, temporary.path().join("alias")).unwrap();
        let file = fs::File::open(&original).unwrap();
        let allocated = fs2::FileExt::allocated_size(&file).unwrap();
        let snapshot = scan(
            &[temporary.path().to_owned()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        assert_eq!(
            snapshot.nodes[snapshot.roots[0]].allocated_size,
            Some(allocated)
        );
        assert_eq!(snapshot.total_size(false), 16384);
        assert!(snapshot.validate().is_ok());
    }

    #[cfg(not(unix))]
    #[test]
    fn scan_reports_unsupported_filesystem_boundaries() {
        let temporary = tempfile::tempdir().unwrap();
        let options = ScanOptions {
            one_file_system: true,
            ..ScanOptions::default()
        };
        assert!(
            scan(
                &[temporary.path().to_owned()],
                &options,
                &ScanControl::default()
            )
            .unwrap_err()
            .contains("filesystem boundaries")
        );
    }

    #[test]
    fn scan_aggregates_files_and_deduplicates_overlapping_roots() {
        let temporary = tempfile::tempdir().unwrap();
        let child = temporary.path().join("nested");
        fs::create_dir(&child).unwrap();
        fs::write(temporary.path().join("large.bin"), [0; 9]).unwrap();
        fs::write(child.join("small.bin"), [0; 3]).unwrap();
        let snapshot = scan(
            &[
                child,
                temporary.path().to_owned(),
                temporary.path().to_owned(),
            ],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        assert_eq!(
            (
                snapshot.total_size(false),
                snapshot.file_count(),
                snapshot.directory_count(),
                snapshot.roots.len()
            ),
            (12, 2, 2, 1)
        );
        let root = &snapshot.nodes[snapshot.roots[0]];
        assert_eq!(snapshot.nodes[root.children[0]].logical_size, 9);
        assert!(snapshot.validate().is_ok());
    }

    #[test]
    fn repeated_directory_identity_is_pruned_before_insertion() {
        let temporary = tempfile::tempdir().unwrap();
        let metadata = fs::symlink_metadata(temporary.path()).unwrap();
        let mut snapshot = scan(
            &[temporary.path().to_owned()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        snapshot.nodes.clear();
        snapshot.roots.clear();
        let mut identities = HashMap::new();
        let mut directories = HashSet::new();
        let control = ScanControl::default();
        for name in ["original", "alias"] {
            let mut node = node_from_metadata(temporary.path().to_owned(), None, &metadata);
            node.path = temporary.path().join(name);
            insert_node(
                &mut snapshot,
                &mut identities,
                &mut directories,
                node,
                &control,
            );
        }
        assert_eq!(snapshot.nodes.len(), 1);
        assert_eq!(control.visited.load(Ordering::Relaxed), 1);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn scan_counts_macos_firmlink_roots_once() {
        let temporary = tempfile::tempdir_in("/private/tmp").unwrap();
        fs::write(temporary.path().join("data"), [1; 11]).unwrap();
        let original = fs::canonicalize(temporary.path()).unwrap();
        let alias = Path::new("/System/Volumes/Data").join(original.strip_prefix("/").unwrap());
        let original_metadata = fs::symlink_metadata(&original).unwrap();
        let alias_metadata = fs::symlink_metadata(&alias).unwrap();
        assert!(!is_link(&original_metadata));
        assert!(!is_link(&alias_metadata));
        assert_eq!(
            metadata_identity(&original_metadata),
            metadata_identity(&alias_metadata)
        );
        let snapshot = scan(
            &[original, alias],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        assert_eq!(
            (
                snapshot.total_size(false),
                snapshot.file_count(),
                snapshot.directory_count(),
                snapshot.roots.len()
            ),
            (11, 1, 1, 1)
        );
        assert!(snapshot.validate().is_ok());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn subtree_refresh_does_not_reintroduce_a_retained_directory_alias() {
        let temporary = tempfile::tempdir_in("/private/tmp").unwrap();
        let original_path = fs::canonicalize(temporary.path()).unwrap();
        let nested = original_path.join("nested");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("data"), [1; 11]).unwrap();
        let alias = Path::new("/System/Volumes/Data").join(nested.strip_prefix("/").unwrap());
        let original = scan(
            &[original_path.clone(), alias],
            &ScanOptions {
                exclude: vec![format!("^{}$", regex::escape(&nested.to_string_lossy()))],
                ..ScanOptions::default()
            },
            &ScanControl::default(),
        )
        .unwrap();
        let selected = original
            .nodes
            .iter()
            .position(|node| node.path == original_path)
            .unwrap();
        let refreshed = refresh_subtree(
            &original,
            selected,
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        assert_eq!(
            (
                refreshed.total_size(false),
                refreshed.file_count(),
                refreshed.directory_count()
            ),
            (11, 1, 2)
        );
        assert!(refreshed.validate().is_ok());
    }

    #[test]
    fn scan_excludes_matching_subtrees_and_rejects_invalid_regex() {
        let temporary = tempfile::tempdir().unwrap();
        fs::create_dir(temporary.path().join("ignored")).unwrap();
        fs::write(temporary.path().join("ignored/data"), [0; 10]).unwrap();
        fs::write(temporary.path().join("keep"), [0; 2]).unwrap();
        let options = ScanOptions {
            exclude: vec!["[/\\\\]ignored$".to_owned()],
            ..ScanOptions::default()
        };
        let snapshot = scan(
            &[temporary.path().to_owned()],
            &options,
            &ScanControl::default(),
        )
        .unwrap();
        assert_eq!(snapshot.total_size(false), 2);
        let options = ScanOptions {
            exclude: vec!["[".to_owned()],
            ..ScanOptions::default()
        };
        assert!(
            scan(
                &[temporary.path().to_owned()],
                &options,
                &ScanControl::default()
            )
            .is_err()
        );
    }

    #[test]
    fn scan_preserves_missing_root_errors_and_rejects_no_viable_roots() {
        let temporary = tempfile::tempdir().unwrap();
        let missing = temporary.path().join("absent");
        let snapshot = scan(
            &[missing.clone(), temporary.path().to_owned()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        assert!(!snapshot.errors.is_empty());
        assert!(scan(&[missing], &ScanOptions::default(), &ScanControl::default()).is_err());
    }

    #[test]
    fn scan_returns_a_valid_cancelled_snapshot() {
        let temporary = tempfile::tempdir().unwrap();
        let control = ScanControl::default();
        control.cancelled.store(true, Ordering::Relaxed);
        control.paused.store(true, Ordering::Relaxed);
        let snapshot = scan(
            &[temporary.path().to_owned()],
            &ScanOptions::default(),
            &control,
        )
        .unwrap();
        assert!(snapshot.cancelled);
        assert!(snapshot.validate().is_ok());
    }

    #[test]
    fn checkpoint_waits_until_resumed() {
        let control = ScanControl::default();
        control.paused.store(true, Ordering::Relaxed);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(30));
                control.paused.store(false, Ordering::Relaxed);
            });
            let started = std::time::Instant::now();
            assert!(control.checkpoint());
            assert!(started.elapsed() >= std::time::Duration::from_millis(20));
        });
    }

    #[test]
    fn scan_supports_file_roots_and_updates_progress() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("file");
        fs::write(&path, [7; 5]).unwrap();
        let control = ScanControl::default();
        let snapshot = scan(&[path], &ScanOptions::default(), &control).unwrap();
        assert_eq!(
            (
                snapshot.total_size(false),
                snapshot.file_count(),
                snapshot.directory_count()
            ),
            (5, 1, 0)
        );
        assert_eq!(
            (
                control.visited.load(Ordering::Relaxed),
                control.bytes.load(Ordering::Relaxed)
            ),
            (1, 5)
        );
        assert!(snapshot.validate().is_ok());
    }

    #[test]
    fn allocation_progress_replaces_the_previous_hardlink_owner_charge() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("data");
        fs::write(&path, [1; 8]).unwrap();
        let metadata = fs::symlink_metadata(&path).unwrap();
        let mut snapshot = scan(
            &[temporary.path().to_owned()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        snapshot.nodes.clear();
        snapshot.roots.clear();
        let mut identities = HashMap::new();
        let mut directories = HashSet::new();
        let control = ScanControl::default();
        for (name, allocated) in [("z-original", 4096), ("a-alias", 8192)] {
            let mut node = node_from_metadata(path.clone(), None, &metadata);
            node.path = temporary.path().join(name);
            node.allocated_size = Some(allocated);
            let index = insert_node(
                &mut snapshot,
                &mut identities,
                &mut directories,
                node,
                &control,
            )
            .unwrap();
            snapshot.roots.push(index);
        }
        assert_eq!(control.allocated_bytes.load(Ordering::Relaxed), 8192);
        assert_eq!(control.bytes.load(Ordering::Relaxed), 16);
        assert_eq!(snapshot.total_size(true), 8192);
        assert!(snapshot.validate().is_ok());
    }

    #[test]
    fn allocation_progress_tracks_unknown_metadata_and_resets_for_a_new_scan() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("data");
        fs::write(&path, [1; 8]).unwrap();
        let metadata = fs::symlink_metadata(&path).unwrap();
        let control = ScanControl::default();
        let mut node = node_from_metadata(path.clone(), None, &metadata);
        node.allocated_size = None;
        update_progress(&control, &node);
        assert_eq!(control.unknown_allocations.load(Ordering::Relaxed), 1);
        assert_eq!(control.allocated_bytes.load(Ordering::Relaxed), 0);
        control.allocated_bytes.store(123, Ordering::Relaxed);
        let snapshot = scan(&[path], &ScanOptions::default(), &control).unwrap();
        assert_eq!(control.unknown_allocations.load(Ordering::Relaxed), 0);
        assert_eq!(
            Some(control.allocated_bytes.load(Ordering::Relaxed)),
            snapshot.nodes[snapshot.roots[0]].allocated_size
        );
    }

    #[cfg(unix)]
    #[test]
    fn scan_keeps_sparse_logical_bytes_separate_from_allocation_progress() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("sparse");
        fs::File::create(&path)
            .unwrap()
            .set_len(1024 * 1024)
            .unwrap();
        let control = ScanControl::default();
        let snapshot = scan(&[path], &ScanOptions::default(), &control).unwrap();
        assert_eq!(snapshot.total_size(false), 1024 * 1024);
        assert!(snapshot.total_size(true) < snapshot.total_size(false));
        assert_eq!(control.bytes.load(Ordering::Relaxed), 1024 * 1024);
        assert_eq!(
            control.allocated_bytes.load(Ordering::Relaxed),
            snapshot.total_size(true)
        );
    }

    #[cfg(unix)]
    #[test]
    fn scan_records_permission_denied_directories() {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempfile::tempdir().unwrap();
        let blocked = temporary.path().join("blocked");
        fs::create_dir(&blocked).unwrap();
        fs::write(blocked.join("secret"), [8; 10]).unwrap();
        let canonical = fs::canonicalize(&blocked).unwrap();
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o0)).unwrap();
        let access_denied = fs::read_dir(&blocked).is_err();
        let result = scan(
            &[temporary.path().to_owned()],
            &ScanOptions::default(),
            &ScanControl::default(),
        );
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
        let snapshot = result.unwrap();
        if access_denied {
            assert!(!snapshot.errors.is_empty());
            assert!(
                snapshot
                    .nodes
                    .iter()
                    .any(|node| node.path == canonical && node.error.is_some())
            );
        }
        assert!(snapshot.validate().is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn scan_does_not_follow_symlink_cycles_or_root_symlinks() {
        use std::os::unix::fs::symlink;
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join("data"), [0; 5]).unwrap();
        let link = temporary.path().join("loop");
        symlink(temporary.path(), &link).unwrap();
        let snapshot = scan(
            &[temporary.path().to_owned()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        assert_eq!((snapshot.nodes.len(), snapshot.total_size(false)), (3, 5));
        assert!(
            snapshot
                .nodes
                .iter()
                .any(|node| node.kind == EntryKind::Symlink && node.children.is_empty())
        );
        let snapshot = scan(
            &[link.join(".")],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        assert_eq!((snapshot.nodes.len(), snapshot.total_size(false)), (1, 0));
        assert_eq!(snapshot.nodes[0].kind, EntryKind::Symlink);
    }

    #[cfg(unix)]
    #[test]
    fn scan_does_not_count_external_file_or_directory_symlink_targets() {
        use std::os::unix::fs::symlink;
        let temporary = tempfile::tempdir().unwrap();
        let targets = tempfile::tempdir().unwrap();
        fs::write(targets.path().join("data"), [1; 8192]).unwrap();
        symlink(targets.path(), temporary.path().join("directory-link")).unwrap();
        symlink(
            targets.path().join("data"),
            temporary.path().join("file-link"),
        )
        .unwrap();
        let control = ScanControl::default();
        let snapshot = scan(
            &[temporary.path().to_owned()],
            &ScanOptions::default(),
            &control,
        )
        .unwrap();
        assert_eq!(
            (
                snapshot.total_size(false),
                snapshot.total_size(true),
                snapshot.file_count()
            ),
            (0, 0, 0)
        );
        assert_eq!(snapshot.nodes.len(), 3);
        assert_eq!(control.bytes.load(Ordering::Relaxed), 0);
        assert_eq!(control.allocated_bytes.load(Ordering::Relaxed), 0);
        assert!(snapshot.validate().is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn scan_counts_hardlink_logical_bytes_twice_and_allocation_once() {
        use std::os::unix::fs::MetadataExt;
        let temporary = tempfile::tempdir().unwrap();
        let original = temporary.path().join("original");
        fs::write(&original, [1; 8192]).unwrap();
        fs::hard_link(&original, temporary.path().join("alias")).unwrap();
        let blocks = fs::metadata(original).unwrap().blocks() * 512;
        let snapshot = scan(
            &[temporary.path().to_owned()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        assert_eq!(snapshot.total_size(false), 16384);
        assert_eq!(
            snapshot.nodes[snapshot.roots[0]].allocated_size,
            Some(blocks)
        );
        assert!(snapshot.validate().is_ok());
    }
}
