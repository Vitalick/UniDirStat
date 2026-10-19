use super::*;

const FAILED: u32 = 1 << 31;

/// The current work state of a directory and its descendants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectoryScanStatus {
    Queued,
    Scanning,
    Complete,
    Warning,
    Failed,
    Cancelled,
}

/// A borrowed view of the scan. Keep callbacks short so discovery can resume.
pub struct ScanView<'a> {
    pub snapshot: &'a Snapshot,
    pub revision: u64,
    progress: &'a DirectoryProgress,
    finalized: bool,
    cancelled: bool,
}

impl ScanView<'_> {
    /// Queued directories have no worker currently processing them or their descendants.
    pub fn directory_status(&self, index: usize) -> Option<DirectoryScanStatus> {
        if self.snapshot.nodes.get(index)?.kind != EntryKind::Directory {
            return None;
        }
        Some(if self.progress.work_finished(index) {
            if self.directory_failed(index) {
                if self.progress.successes[index] {
                    DirectoryScanStatus::Warning
                } else {
                    DirectoryScanStatus::Failed
                }
            } else {
                DirectoryScanStatus::Complete
            }
        } else if self.cancelled {
            DirectoryScanStatus::Cancelled
        } else if self.progress.active.as_ref().is_some_and(|active| {
            active.iter().any(|&active| {
                self.snapshot.nodes[active]
                    .path
                    .starts_with(&self.snapshot.nodes[index].path)
            })
        }) {
            DirectoryScanStatus::Scanning
        } else {
            DirectoryScanStatus::Queued
        })
    }
    /// The scanner has handed off its finalized snapshot, including final child order.
    pub fn is_finalized(&self) -> bool {
        self.finalized
    }
    /// True only after this directory and every descendant finished without errors.
    pub fn directory_finished(&self, index: usize) -> bool {
        self.snapshot
            .nodes
            .get(index)
            .is_some_and(|node| node.kind == EntryKind::Directory && self.progress.finished(index))
    }

    /// True when this directory or an enumerated descendant encountered an error.
    pub fn directory_failed(&self, index: usize) -> bool {
        self.snapshot.nodes.get(index).is_some_and(|node| {
            node.kind == EntryKind::Directory
                && self
                    .progress
                    .pending
                    .get(index)
                    .is_some_and(|pending| pending & FAILED != 0)
        })
    }

    /// Metadata time for files, or subtree wall time for directories (including active work).
    pub fn scan_duration(&self, index: usize) -> Option<Duration> {
        let node = self.snapshot.nodes.get(index)?;
        if let Some(started) = self.progress.started.get(&index) {
            return Some(started.elapsed());
        }
        (node.scan_duration_ns > 0).then(|| Duration::from_nanos(node.scan_duration_ns))
    }
}

#[derive(Debug)]
pub(super) enum LiveScan {
    Active(Arc<Mutex<ScanState>>),
    Detached(DirectoryProgress),
    Ready(Arc<Snapshot>, DirectoryProgress),
}

impl ScanControl {
    /// Enables borrowed progress views for scans started with this control.
    pub fn enable_live_view(&self) {
        self.live_enabled.store(true, Ordering::Release);
    }

    /// Releases borrowed progress storage and any retained completed snapshot.
    pub fn clear_live_view(&self) {
        *self
            .live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    /// Returns None while the scanner holds the tree or transfers the final snapshot.
    /// The callback must not call back into this control's live view methods.
    pub fn with_live_view<R>(&self, callback: impl FnOnce(ScanView<'_>) -> R) -> Option<R> {
        let live = self.live.try_lock().ok()?;
        match live.as_ref()? {
            LiveScan::Active(state) => {
                let state = Arc::clone(state);
                drop(live);
                let state = state.try_lock().ok()?;
                let progress = state.progress.as_ref()?;
                Some(callback(ScanView {
                    snapshot: &state.snapshot,
                    revision: progress.revision,
                    progress,
                    finalized: false,
                    cancelled: self.is_cancelled(),
                }))
            }
            LiveScan::Ready(snapshot, progress) => Some(callback(ScanView {
                snapshot,
                revision: progress.revision,
                progress,
                finalized: true,
                cancelled: self.is_cancelled() || snapshot.cancelled,
            })),
            LiveScan::Detached(_) => None,
        }
    }

    /// Shares the scanner's returned snapshot during subsequent indexing work.
    pub fn set_completed_live_snapshot(&self, snapshot: Arc<Snapshot>) {
        let mut live = self
            .live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match live.take() {
            Some(LiveScan::Detached(mut progress)) => {
                progress.revision += 1;
                *live = Some(LiveScan::Ready(snapshot, progress));
            }
            previous => *live = previous,
        }
    }
}

#[derive(Debug)]
pub(super) struct DirectoryProgress {
    // One listing marker plus unfinished direct subdirectories; the high bit retains failures.
    pending: Vec<u32>,
    successes: Vec<bool>,
    started: HashMap<usize, Instant>,
    active: Option<HashSet<usize>>,
    pub revision: u64,
}

impl DirectoryProgress {
    fn work_finished(&self, index: usize) -> bool {
        self.pending
            .get(index)
            .is_some_and(|pending| pending & !FAILED == 0)
    }

    pub fn finished(&self, index: usize) -> bool {
        self.pending.get(index) == Some(&0)
    }

    pub fn new(snapshot: &Snapshot, live: bool) -> Self {
        Self {
            pending: snapshot
                .nodes
                .iter()
                .map(|node| u32::from(node.kind == EntryKind::Directory))
                .collect(),
            successes: vec![false; snapshot.nodes.len()],
            started: HashMap::new(),
            active: live.then(HashSet::new),
            revision: 1,
        }
    }

    pub fn start(&mut self, index: usize) {
        self.started.insert(index, Instant::now());
        if let Some(active) = &mut self.active {
            active.insert(index);
        }
        self.revision += 1;
    }

    pub fn finish_listing(&mut self, index: usize) {
        if let Some(active) = &mut self.active {
            active.remove(&index);
        }
        self.revision += 1;
    }

    pub fn stop_timers(&mut self) {
        self.started = HashMap::new();
        self.active = None;
    }

    pub fn insert(&mut self, node: &Node, parent: usize) {
        let directory = node.kind == EntryKind::Directory;
        self.pending.push(u32::from(directory));
        self.successes.push(false);
        if directory {
            // Overflow conservatively retains the failure bit, never a false completion.
            self.pending[parent] = self.pending[parent].saturating_add(1);
        }
        if node.error.is_some() {
            self.fail(parent);
        } else if !directory {
            self.successes[parent] = true;
        }
    }

    pub fn fail(&mut self, index: usize) {
        self.pending[index] |= FAILED;
        self.revision += 1;
    }

    pub fn finish(&mut self, snapshot: &mut Snapshot, mut index: usize) {
        loop {
            let pending = &mut self.pending[index];
            if *pending & !FAILED == 0 {
                break;
            }
            *pending -= 1;
            if *pending & !FAILED != 0 {
                break;
            }
            let failed = *pending & FAILED;
            if failed == 0
                && snapshot.nodes[index].children.is_empty()
                && !snapshot.nodes[index].depth_limited
            {
                self.successes[index] = true;
            }
            snapshot.nodes[index].scan_failed = failed != 0 && !self.successes[index];
            snapshot.nodes[index].scan_warning = failed != 0 && self.successes[index];
            if let Some(started) = self.started.remove(&index) {
                snapshot.nodes[index].scan_duration_ns = duration_ns(started.elapsed());
            }
            let Some(parent) = snapshot.nodes[index].parent else {
                break;
            };
            self.pending[parent] |= failed;
            self.successes[parent] |= self.successes[index];
            index = parent;
        }
        self.revision += 1;
    }
}

pub(super) fn duration_ns(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos())
        .unwrap_or(u64::MAX)
        .max(1)
}

pub(super) struct LiveTotals {
    logical: u64,
    allocated: Option<u64>,
    files: u64,
    directories: u64,
}

impl Default for LiveTotals {
    fn default() -> Self {
        Self {
            logical: 0,
            allocated: empty_allocation(),
            files: 0,
            directories: 0,
        }
    }
}

impl LiveTotals {
    pub fn add(&mut self, node: &Node) {
        self.logical = self.logical.saturating_add(node.logical_size);
        self.allocated = self
            .allocated
            .zip(node.allocated_size)
            .map(|(total, size)| total.saturating_add(size));
        self.files = self.files.saturating_add(node.files);
        self.directories = self
            .directories
            .saturating_add(u64::from(node.kind == EntryKind::Directory));
    }

    pub fn apply(self, snapshot: &mut Snapshot, mut index: usize) {
        loop {
            let node = &mut snapshot.nodes[index];
            node.logical_size = node.logical_size.saturating_add(self.logical);
            node.allocated_size = node
                .allocated_size
                .zip(self.allocated)
                .map(|(total, size)| total.saturating_add(size));
            node.files = node.files.saturating_add(self.files);
            node.directories = node.directories.saturating_add(self.directories);
            let Some(parent) = node.parent else {
                break;
            };
            index = parent;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_status_distinguishes_all_failed_from_mixed_nested_descendants() {
        for successful_file in [false, true] {
            let temporary = tempfile::tempdir().unwrap();
            fs::create_dir_all(temporary.path().join("child/blocked")).unwrap();
            if successful_file {
                fs::write(temporary.path().join("child/data"), b"data").unwrap();
            }
            let mut snapshot = scan(
                &[temporary.path().into()],
                &ScanOptions::default(),
                &ScanControl::default(),
            )
            .unwrap();
            let mut root_only = snapshot.clone();
            root_only.nodes.truncate(1);
            let mut progress = DirectoryProgress::new(&root_only, true);
            for node in &snapshot.nodes[1..] {
                progress.insert(node, node.parent.unwrap());
            }
            let child = snapshot
                .nodes
                .iter()
                .position(|node| node.name() == "child")
                .unwrap();
            let blocked = snapshot
                .nodes
                .iter()
                .position(|node| node.name() == "blocked")
                .unwrap();
            snapshot.nodes[blocked].scan_duration_ns = 0;
            progress.start(blocked);
            progress.fail(blocked);
            for index in [blocked, child, 0] {
                progress.finish(&mut snapshot, index);
            }
            let view = ScanView {
                snapshot: &snapshot,
                revision: progress.revision,
                progress: &progress,
                finalized: true,
                cancelled: false,
            };
            let expected = if successful_file {
                DirectoryScanStatus::Warning
            } else {
                DirectoryScanStatus::Failed
            };
            for index in [child, 0] {
                assert_eq!(view.directory_status(index), Some(expected));
                assert_eq!(snapshot.nodes[index].scan_warning, successful_file);
                assert_eq!(snapshot.nodes[index].scan_failed, !successful_file);
                assert!(!view.directory_finished(index));
            }
            assert_eq!(
                view.directory_status(blocked),
                Some(DirectoryScanStatus::Failed)
            );
            assert!(snapshot.nodes[blocked].scan_failed);
            assert!(snapshot.nodes[blocked].scan_duration_ns > 0);
        }
    }

    #[test]
    fn directory_errors_do_not_hide_pending_or_active_work() {
        let temporary = tempfile::tempdir().unwrap();
        fs::create_dir(temporary.path().join("child")).unwrap();
        let mut snapshot = scan(
            &[temporary.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        let mut progress = DirectoryProgress::new(&snapshot, true);
        progress.pending[0] = 2;
        progress.fail(0);
        let status = |snapshot: &Snapshot, progress: &DirectoryProgress, cancelled| {
            ScanView {
                snapshot,
                revision: progress.revision,
                progress,
                finalized: false,
                cancelled,
            }
            .directory_status(0)
        };
        assert_eq!(
            status(&snapshot, &progress, false),
            Some(DirectoryScanStatus::Queued)
        );
        progress.start(1);
        assert_eq!(
            status(&snapshot, &progress, false),
            Some(DirectoryScanStatus::Scanning)
        );
        assert_eq!(
            status(&snapshot, &progress, true),
            Some(DirectoryScanStatus::Cancelled)
        );
        progress.finish_listing(1);
        progress.finish(&mut snapshot, 1);
        assert_eq!(
            status(&snapshot, &progress, false),
            Some(DirectoryScanStatus::Queued)
        );
        progress.finish(&mut snapshot, 0);
        assert_eq!(
            status(&snapshot, &progress, false),
            Some(DirectoryScanStatus::Warning)
        );
    }
}
