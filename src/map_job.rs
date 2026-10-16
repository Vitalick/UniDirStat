use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    sync::{
        Arc, Weak,
        mpsc::{self, Receiver, TryRecvError},
    },
};

use eframe::egui;
use unidirstat::{
    analysis::ExtensionStats,
    layout::{self, LayoutControl},
    model::{EntryKind, Snapshot},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapKind {
    Treemap,
    Flame,
    Sunburst,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapKey {
    pub scope: Option<usize>,
    pub allocated: bool,
    pub width: u32,
    pub height: u32,
    pub kind: MapKind,
}

#[derive(Default)]
pub struct MapData {
    pub width: u32,
    pub height: u32,
    pub tiles: Vec<layout::Tile>,
    pub sectors: Vec<layout::Sector>,
    pub frames: Vec<layout::Tile>,
    pub aggregates: Vec<layout::Tile>,
    frame_lookup: HashMap<usize, layout::Rect>,
}

impl MapData {
    pub fn frame(&self, node: usize) -> Option<layout::Tile> {
        self.frame_lookup
            .get(&node)
            .map(|&rect| layout::Tile { node, rect })
    }
}

#[derive(Default)]
pub struct ScopeSummary {
    pub depth: usize,
    pub extensions_logical: Vec<ExtensionStats>,
    pub extensions_allocated: Vec<ExtensionStats>,
    pub largest_logical: Vec<usize>,
    pub largest_allocated: Vec<usize>,
}

struct SummaryRequest {
    scope: Option<usize>,
    snapshot: Arc<Snapshot>,
    context: egui::Context,
}

struct SummaryTask {
    scope: Option<usize>,
    control: Arc<LayoutControl>,
    receiver: Receiver<Option<ScopeSummary>>,
}

struct MapRequest {
    key: MapKey,
    snapshot: Arc<Snapshot>,
    context: egui::Context,
}

struct MapTask {
    key: MapKey,
    control: Arc<LayoutControl>,
    receiver: Receiver<Option<MapData>>,
}

#[derive(Default)]
pub struct MapWorker {
    snapshot: Option<Weak<Snapshot>>,
    requested: Option<MapKey>,
    ready: Option<Arc<MapData>>,
    ready_key: Option<MapKey>,
    running: Option<MapTask>,
    queued: Option<MapRequest>,
    cache: VecDeque<(MapKey, Arc<MapData>)>,
    error: Option<String>,
    summary_requested: Option<Option<usize>>,
    summary_ready: Option<Arc<ScopeSummary>>,
    summary_running: Option<SummaryTask>,
    summary_queued: Option<SummaryRequest>,
    summary_cache: VecDeque<(Option<usize>, Arc<ScopeSummary>)>,
    summary_error: Option<String>,
}

impl MapWorker {
    pub fn request(&mut self, key: MapKey, snapshot: Arc<Snapshot>, context: &egui::Context) {
        self.request_summary(key.scope, Arc::clone(&snapshot), context);
        if self.requested == Some(key) {
            return;
        }
        self.requested = Some(key);
        if !self.ready_key.is_some_and(|ready| {
            ready.scope == key.scope && ready.allocated == key.allocated && ready.kind == key.kind
        }) {
            self.ready = None;
            self.ready_key = None;
        }
        self.error = None;
        self.queued = None;
        if let Some(task) = &self.running {
            task.control.cancel();
        }
        if let Some(position) = self.cache.iter().position(|entry| entry.0 == key)
            && let Some(cached) = self.cache.remove(position)
        {
            self.ready = Some(Arc::clone(&cached.1));
            self.ready_key = Some(key);
            self.cache.push_back(cached);
            return;
        }
        let request = MapRequest {
            key,
            snapshot,
            context: context.clone(),
        };
        if self.running.is_some() {
            self.queued = Some(request);
        } else {
            self.start(request);
        }
    }

    pub fn request_summary(
        &mut self,
        scope: Option<usize>,
        snapshot: Arc<Snapshot>,
        context: &egui::Context,
    ) {
        let identity = Arc::downgrade(&snapshot);
        if !self
            .snapshot
            .as_ref()
            .is_some_and(|current| current.ptr_eq(&identity))
        {
            self.clear();
            self.snapshot = Some(identity);
        }
        if self.summary_requested == Some(scope) {
            return;
        }
        self.summary_requested = Some(scope);
        self.summary_ready = None;
        self.summary_error = None;
        self.summary_queued = None;
        if let Some(task) = &self.summary_running {
            task.control.cancel();
        }
        if let Some(position) = self.summary_cache.iter().position(|entry| entry.0 == scope)
            && let Some(cached) = self.summary_cache.remove(position)
        {
            self.summary_ready = Some(Arc::clone(&cached.1));
            self.summary_cache.push_back(cached);
            return;
        }
        let request = SummaryRequest {
            scope,
            snapshot,
            context: context.clone(),
        };
        if self.summary_running.is_some() {
            self.summary_queued = Some(request);
        } else {
            self.start_summary(request);
        }
    }

    fn start_summary(&mut self, request: SummaryRequest) {
        let scope = request.scope;
        let control = Arc::new(LayoutControl::default());
        let worker_control = Arc::clone(&control);
        let (sender, receiver) = mpsc::sync_channel(1);
        let spawned = std::thread::Builder::new()
            .name("scope-summary".into())
            .spawn(move || {
                let data = compute_summary(&request.snapshot, request.scope, &worker_control);
                let _ = sender.send(data);
                request.context.request_repaint();
            });
        match spawned {
            Ok(_) => {
                self.summary_running = Some(SummaryTask {
                    scope,
                    control,
                    receiver,
                })
            }
            Err(error) => self.summary_error = Some(error.to_string()),
        }
    }

    fn poll_summary(&mut self) -> bool {
        let Some(task) = &self.summary_running else {
            return false;
        };
        let result = task.receiver.try_recv();
        if matches!(result, Err(TryRecvError::Empty)) {
            return false;
        }
        let Some(task) = self.summary_running.take() else {
            return false;
        };
        if self.summary_requested == Some(task.scope) && !task.control.is_cancelled() {
            match result {
                Ok(Some(data)) => {
                    let data = Arc::new(data);
                    self.summary_ready = Some(Arc::clone(&data));
                    self.summary_cache.push_back((task.scope, data));
                    while self.summary_cache.len() > 2 {
                        self.summary_cache.pop_front();
                    }
                }
                _ => self.summary_error = Some("Summary computation worker stopped".into()),
            }
        }
        if let Some(request) = self.summary_queued.take() {
            self.start_summary(request);
        }
        true
    }

    fn start(&mut self, request: MapRequest) {
        let key = request.key;
        let control = Arc::new(LayoutControl::default());
        let worker_control = Arc::clone(&control);
        let (sender, receiver) = mpsc::sync_channel(1);
        let spawned = std::thread::Builder::new()
            .name("map-layout".into())
            .spawn(move || {
                let data = compute(&request.snapshot, request.key, &worker_control);
                let _ = sender.send(data);
                request.context.request_repaint();
            });
        match spawned {
            Ok(_) => {
                self.running = Some(MapTask {
                    key,
                    control,
                    receiver,
                })
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    pub fn poll(&mut self) -> bool {
        let summary_changed = self.poll_summary();
        let Some(task) = &self.running else {
            return summary_changed;
        };
        let result = task.receiver.try_recv();
        if matches!(result, Err(TryRecvError::Empty)) {
            return summary_changed;
        }
        let Some(task) = self.running.take() else {
            return false;
        };
        if self.requested == Some(task.key) && !task.control.is_cancelled() {
            match result {
                Ok(Some(data)) => {
                    let data = Arc::new(data);
                    self.ready = Some(Arc::clone(&data));
                    self.ready_key = Some(task.key);
                    self.cache.push_back((task.key, data));
                    while self.cache.len() > 2 {
                        self.cache.pop_front();
                    }
                }
                _ => self.error = Some("Map computation worker stopped".into()),
            }
        }
        if let Some(request) = self.queued.take() {
            self.start(request);
        }
        true
    }

    pub fn data(&self) -> Option<&Arc<MapData>> {
        self.ready.as_ref()
    }

    pub fn pending(&self) -> bool {
        self.requested.is_some() && self.requested != self.ready_key && self.error.is_none()
    }

    pub fn summary(&self) -> Option<&Arc<ScopeSummary>> {
        self.summary_ready.as_ref()
    }

    pub fn summary_pending(&self) -> bool {
        self.summary_requested.is_some()
            && self.summary_ready.is_none()
            && self.summary_error.is_none()
    }

    pub fn summary_error(&self) -> Option<&str> {
        self.summary_error.as_deref()
    }

    pub fn progress(&self) -> (u64, u64) {
        self.running
            .as_ref()
            .filter(|task| self.requested == Some(task.key) && !task.control.is_cancelled())
            .map_or((0, 0), |task| task.control.progress())
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn clear(&mut self) {
        if let Some(task) = &self.running {
            task.control.cancel();
        }
        self.requested = None;
        self.snapshot = None;
        self.ready = None;
        self.ready_key = None;
        self.queued = None;
        self.cache.clear();
        self.error = None;
        if let Some(task) = &self.summary_running {
            task.control.cancel();
        }
        self.summary_requested = None;
        self.summary_ready = None;
        self.summary_queued = None;
        self.summary_cache.clear();
        self.summary_error = None;
    }
}

impl Drop for MapWorker {
    fn drop(&mut self) {
        if let Some(task) = &self.running {
            task.control.cancel();
        }
        if let Some(task) = &self.summary_running {
            task.control.cancel();
        }
    }
}

fn compute(snapshot: &Snapshot, key: MapKey, control: &LayoutControl) -> Option<MapData> {
    let selected;
    let roots = match key.scope {
        Some(index) => {
            selected = [index];
            selected.as_slice()
        }
        None => snapshot.roots.as_slice(),
    };
    let bounds = layout::Rect {
        x: 0.0,
        y: 0.0,
        width: key.width as f32,
        height: key.height as f32,
    };
    let mut data = MapData {
        width: key.width,
        height: key.height,
        ..MapData::default()
    };
    match key.kind {
        MapKind::Treemap => {
            let geometry =
                layout::treemap_hierarchy(snapshot, roots, key.allocated, bounds, control)?;
            data.tiles = geometry.tiles;
            data.aggregates = geometry.aggregates;
            let mut seen = HashSet::new();
            for frame in geometry.frames {
                if control.is_cancelled() {
                    return None;
                }
                data.frame_lookup.insert(frame.node, frame.rect);
                let rect = frame.rect;
                if seen.insert([
                    rect.x.to_bits(),
                    rect.y.to_bits(),
                    rect.width.to_bits(),
                    rect.height.to_bits(),
                ]) {
                    data.frames.push(frame);
                }
            }
        }
        MapKind::Flame => {
            data.tiles = layout::flame_visible(snapshot, roots, key.allocated, bounds, control)?;
        }
        MapKind::Sunburst => {
            data.sectors =
                layout::sunburst_visible(snapshot, roots, key.allocated, bounds, control)?;
        }
    }
    Some(data)
}

fn compute_summary(
    snapshot: &Snapshot,
    scope: Option<usize>,
    control: &LayoutControl,
) -> Option<ScopeSummary> {
    if control.is_cancelled() {
        return None;
    }
    let selected;
    let roots = match scope {
        Some(index) => {
            selected = [index];
            selected.as_slice()
        }
        None => snapshot.roots.as_slice(),
    };
    let mut extensions = BTreeMap::<String, (u64, u64, u64)>::new();
    let mut largest_logical = BTreeSet::new();
    let mut largest_allocated = BTreeSet::new();
    let mut visited = HashSet::new();
    let mut max_depth = 0;
    let mut pending = vec![(roots, 0)];
    while let Some((indices, depth)) = pending.pop() {
        for &index in indices {
            if control.is_cancelled() {
                return None;
            }
            let Some(node) = snapshot.nodes.get(index) else {
                continue;
            };
            if !visited.insert(index) {
                continue;
            }
            max_depth = max_depth.max(depth);
            if node.kind == EntryKind::Directory {
                pending.push((node.children.as_slice(), depth + 1));
            } else if node.kind == EntryKind::File {
                let entry = extensions.entry(node.extension()).or_default();
                entry.0 = entry.0.saturating_add(node.logical_size);
                entry.1 = entry.1.saturating_add(node.size(true));
                entry.2 = entry.2.saturating_add(1);
                for (largest, size) in [
                    (&mut largest_logical, node.logical_size),
                    (&mut largest_allocated, node.size(true)),
                ] {
                    largest.insert((Reverse(size), index));
                    if largest.len() > 5 {
                        largest.pop_last();
                    }
                }
            }
        }
    }
    let mut logical = BTreeMap::new();
    let mut allocated = BTreeMap::new();
    for (extension, (logical_bytes, allocated_bytes, files)) in extensions {
        if control.is_cancelled() {
            return None;
        }
        logical.insert((Reverse(logical_bytes), extension.clone()), files);
        allocated.insert((Reverse(allocated_bytes), extension), files);
    }
    let mut result = ScopeSummary {
        depth: max_depth,
        largest_logical: largest_logical
            .into_iter()
            .map(|(_, index)| index)
            .collect(),
        largest_allocated: largest_allocated
            .into_iter()
            .map(|(_, index)| index)
            .collect(),
        ..ScopeSummary::default()
    };
    for (ordered, output) in [
        (logical, &mut result.extensions_logical),
        (allocated, &mut result.extensions_allocated),
    ] {
        for ((Reverse(bytes), extension), files) in ordered {
            if control.is_cancelled() {
                return None;
            }
            output.push(ExtensionStats {
                extension,
                bytes,
                files,
            });
        }
    }
    if control.is_cancelled() {
        None
    } else {
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        time::{Duration, Instant},
    };

    use unidirstat::model::{EntryKind, Node};

    use super::*;

    fn snapshot() -> Arc<Snapshot> {
        Arc::new(Snapshot {
            version: 1,
            roots: vec![0],
            nodes: (0..3)
                .map(|index| Node {
                    path: PathBuf::from(format!("node-{index}")),
                    parent: (index > 0).then_some(0),
                    children: if index == 0 { vec![1, 2] } else { vec![] },
                    kind: if index == 0 {
                        EntryKind::Directory
                    } else {
                        EntryKind::File
                    },
                    logical_size: [100, 60, 40][index],
                    allocated_size: Some([100, 20, 80][index]),
                    files: 0,
                    directories: 0,
                    modified: None,
                    readonly: false,
                    permissions: None,
                    owner: None,
                    file_id: None,
                    error: None,
                })
                .collect(),
            errors: vec![],
            started: 0,
            elapsed_ms: 0,
            cancelled: false,
        })
    }

    fn key() -> MapKey {
        MapKey {
            scope: None,
            allocated: false,
            width: 120,
            height: 80,
            kind: MapKind::Treemap,
        }
    }

    fn finish(worker: &mut MapWorker) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while worker.pending() && Instant::now() < deadline {
            worker.poll();
            std::thread::yield_now();
        }
        assert!(!worker.pending(), "map worker timed out");
        assert!(worker.error().is_none(), "{:?}", worker.error());
    }

    fn finish_summary(worker: &mut MapWorker) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while worker.summary_pending() && Instant::now() < deadline {
            worker.poll();
            std::thread::yield_now();
        }
        assert!(!worker.summary_pending(), "summary worker timed out");
        assert!(
            worker.summary_error().is_none(),
            "{:?}",
            worker.summary_error()
        );
    }

    #[test]
    fn changed_scope_hides_previous_data_and_ignores_stale_jobs() {
        let snapshot = snapshot();
        let context = egui::Context::default();
        let mut worker = MapWorker::default();
        worker.request(key(), Arc::clone(&snapshot), &context);
        finish(&mut worker);
        assert_eq!(worker.data().unwrap().tiles.len(), 2);

        worker.request(
            MapKey {
                scope: Some(1),
                ..key()
            },
            Arc::clone(&snapshot),
            &context,
        );
        assert!(worker.data().is_none());
        worker.request(
            MapKey {
                scope: Some(2),
                ..key()
            },
            snapshot,
            &context,
        );
        finish(&mut worker);

        assert_eq!(
            worker
                .data()
                .unwrap()
                .tiles
                .iter()
                .map(|tile| tile.node)
                .collect::<Vec<_>>(),
            [2]
        );
    }

    #[test]
    fn cache_keeps_two_results_without_copying_the_vectors() {
        let snapshot = snapshot();
        let context = egui::Context::default();
        let mut worker = MapWorker::default();
        worker.request(key(), Arc::clone(&snapshot), &context);
        finish(&mut worker);
        let original = Arc::clone(worker.data().unwrap());
        worker.request(
            MapKey {
                allocated: true,
                ..key()
            },
            Arc::clone(&snapshot),
            &context,
        );
        finish(&mut worker);
        worker.request(key(), Arc::clone(&snapshot), &context);
        assert!(Arc::ptr_eq(&original, worker.data().unwrap()));
        worker.request(
            MapKey {
                width: 240,
                ..key()
            },
            snapshot,
            &context,
        );
        finish(&mut worker);
        assert_eq!(worker.cache.len(), 2);
    }

    #[test]
    fn clear_discards_finished_and_in_flight_snapshot_data() {
        let context = egui::Context::default();
        let mut worker = MapWorker::default();
        worker.request(key(), snapshot(), &context);
        worker.clear();
        worker.poll();

        assert!(worker.data().is_none());
        assert!(!worker.pending());
        assert_eq!(worker.progress(), (0, 0));
    }

    #[test]
    fn a_new_snapshot_cannot_reuse_a_cached_map() {
        let context = egui::Context::default();
        let mut worker = MapWorker::default();
        worker.request(key(), snapshot(), &context);
        finish(&mut worker);
        let original = Arc::clone(worker.data().unwrap());
        worker.request(key(), snapshot(), &context);
        assert!(worker.data().is_none());
        finish(&mut worker);

        assert!(!Arc::ptr_eq(&original, worker.data().unwrap()));
    }

    #[test]
    fn repeated_resize_requests_compute_only_the_latest_waiting_size() {
        let context = egui::Context::default();
        let snapshot = snapshot();
        let mut worker = MapWorker::default();
        for width in 100..1_100 {
            worker.request(MapKey { width, ..key() }, Arc::clone(&snapshot), &context);
        }
        finish(&mut worker);

        let right = worker
            .data()
            .unwrap()
            .tiles
            .iter()
            .map(|tile| tile.rect.x + tile.rect.width)
            .fold(0.0_f32, f32::max);
        assert_eq!(right, 1_099.0);
    }

    #[test]
    fn resizing_retains_the_previous_geometry_until_the_new_detail_is_ready() {
        let context = egui::Context::default();
        let snapshot = snapshot();
        let mut worker = MapWorker::default();
        worker.request(key(), Arc::clone(&snapshot), &context);
        finish(&mut worker);
        let original = Arc::clone(worker.data().unwrap());

        worker.request(
            MapKey {
                width: 240,
                ..key()
            },
            snapshot,
            &context,
        );

        assert!(worker.pending());
        assert!(
            worker
                .data()
                .is_some_and(|data| Arc::ptr_eq(data, &original))
        );
        assert_eq!((original.width, original.height), (120, 80));
        finish(&mut worker);
        assert!(!Arc::ptr_eq(worker.data().unwrap(), &original));
        assert_eq!(
            (worker.data().unwrap().width, worker.data().unwrap().height),
            (240, 80)
        );
    }

    #[test]
    fn scoped_summary_excludes_files_outside_the_selected_directory() {
        let mut snapshot = snapshot();
        let nodes = &mut Arc::make_mut(&mut snapshot).nodes;
        let mut inside = nodes[1].clone();
        inside.path = PathBuf::from("root/folder/inside.txt");
        inside.parent = Some(1);
        nodes[1].kind = EntryKind::Directory;
        nodes[1].children = vec![3];
        nodes[2].path = PathBuf::from("root/outside.bin");
        nodes.push(inside);

        let summary = compute_summary(&snapshot, Some(1), &LayoutControl::default()).unwrap();

        assert_eq!(summary.largest_logical, [3]);
        assert_eq!(summary.largest_allocated, [3]);
        assert_eq!(summary.extensions_logical.len(), 1);
        assert_eq!(summary.extensions_logical[0].extension, "txt");
        assert_eq!(summary.extensions_logical[0].bytes, 60);
        assert_eq!(summary.extensions_allocated[0].bytes, 20);
    }

    #[test]
    fn scoped_summary_depth_excludes_a_deeper_branch_outside_the_scope() {
        let mut snapshot = snapshot();
        let nodes = &mut Arc::make_mut(&mut snapshot).nodes;
        let file = nodes[1].clone();
        nodes[1].kind = EntryKind::Directory;
        nodes[1].children = vec![3];
        nodes[2].kind = EntryKind::Directory;
        nodes[2].children = vec![4];
        nodes.push(Node {
            parent: Some(1),
            ..file.clone()
        });
        nodes.push(Node {
            kind: EntryKind::Directory,
            children: vec![5],
            parent: Some(2),
            ..file.clone()
        });
        nodes.push(Node {
            parent: Some(4),
            ..file
        });

        let scoped = compute_summary(&snapshot, Some(1), &LayoutControl::default()).unwrap();
        let complete = compute_summary(&snapshot, None, &LayoutControl::default()).unwrap();

        assert_eq!(scoped.depth, 1);
        assert_eq!(complete.depth, 3);
    }

    #[test]
    fn summaries_rank_logical_and_allocated_sizes_independently() {
        let summary = compute_summary(&snapshot(), None, &LayoutControl::default()).unwrap();

        assert_eq!(summary.largest_logical, [1, 2]);
        assert_eq!(summary.largest_allocated, [2, 1]);
    }

    #[test]
    fn summary_computation_honors_cancellation() {
        let control = LayoutControl::default();
        control.cancel();

        assert!(compute_summary(&snapshot(), None, &control).is_none());
    }

    #[test]
    fn viewport_and_metric_changes_reuse_the_same_scoped_summary() {
        let context = egui::Context::default();
        let snapshot = snapshot();
        let mut worker = MapWorker::default();
        worker.request_summary(None, Arc::clone(&snapshot), &context);
        finish_summary(&mut worker);
        let original = Arc::clone(worker.summary().unwrap());

        worker.request(
            MapKey {
                allocated: true,
                width: 480,
                ..key()
            },
            Arc::clone(&snapshot),
            &context,
        );
        assert!(Arc::ptr_eq(worker.summary().unwrap(), &original));
        worker.request_summary(Some(1), Arc::clone(&snapshot), &context);
        finish_summary(&mut worker);
        assert_eq!(worker.summary().unwrap().largest_logical, [1]);
        worker.request_summary(None, snapshot, &context);
        assert!(Arc::ptr_eq(worker.summary().unwrap(), &original));
    }

    #[test]
    fn a_finished_stale_summary_never_replaces_the_requested_scope() {
        let context = egui::Context::default();
        let snapshot = snapshot();
        let (sender, receiver) = mpsc::sync_channel(1);
        let control = Arc::new(LayoutControl::default());
        let mut worker = MapWorker::default();
        worker.snapshot = Some(Arc::downgrade(&snapshot));
        worker.summary_requested = Some(Some(1));
        worker.summary_running = Some(SummaryTask {
            scope: Some(1),
            control: Arc::clone(&control),
            receiver,
        });
        sender
            .send(Some(ScopeSummary {
                largest_logical: vec![1],
                ..ScopeSummary::default()
            }))
            .unwrap();

        worker.request_summary(Some(2), snapshot, &context);
        assert!(control.is_cancelled());
        worker.poll();
        assert!(worker.summary().is_none());
        finish_summary(&mut worker);

        assert_eq!(worker.summary().unwrap().largest_logical, [2]);
    }

    #[test]
    fn replacing_a_snapshot_discards_its_cached_summary() {
        let context = egui::Context::default();
        let mut worker = MapWorker::default();
        worker.request_summary(None, snapshot(), &context);
        finish_summary(&mut worker);
        let original = Arc::clone(worker.summary().unwrap());

        worker.request_summary(None, snapshot(), &context);
        assert!(worker.summary().is_none());
        finish_summary(&mut worker);

        assert!(!Arc::ptr_eq(worker.summary().unwrap(), &original));
    }

    #[test]
    fn identical_directory_frames_draw_once_but_keep_each_directory_addressable() {
        let mut snapshot = snapshot();
        let nodes = &mut Arc::make_mut(&mut snapshot).nodes;
        let template = nodes[1].clone();
        *nodes = (0_usize..4)
            .map(|index| Node {
                children: if index < 3 { vec![index + 1] } else { vec![] },
                parent: index.checked_sub(1),
                kind: if index < 3 {
                    EntryKind::Directory
                } else {
                    EntryKind::File
                },
                logical_size: 100,
                ..template.clone()
            })
            .collect();

        let data = compute(&snapshot, key(), &LayoutControl::default()).unwrap();

        assert_eq!(data.frames.len(), 1);
        for node in [0, 1, 2] {
            assert_eq!(
                data.frame(node),
                Some(layout::Tile {
                    node,
                    rect: layout::Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 120.0,
                        height: 80.0
                    },
                })
            );
        }
        assert!(data.frame(3).is_none());
        assert!(data.frame(usize::MAX).is_none());
    }

    #[test]
    fn changing_map_kind_replaces_geometry_instead_of_reusing_it() {
        let context = egui::Context::default();
        let snapshot = snapshot();
        let mut worker = MapWorker::default();
        worker.request(key(), Arc::clone(&snapshot), &context);
        finish(&mut worker);
        worker.request(
            MapKey {
                kind: MapKind::Sunburst,
                ..key()
            },
            Arc::clone(&snapshot),
            &context,
        );
        finish(&mut worker);
        assert!(worker.data().unwrap().tiles.is_empty());
        assert_eq!(worker.data().unwrap().sectors.len(), 3);

        worker.request(
            MapKey {
                kind: MapKind::Flame,
                ..key()
            },
            snapshot,
            &context,
        );
        finish(&mut worker);
        assert_eq!(worker.data().unwrap().tiles.len(), 3);
        assert!(worker.data().unwrap().sectors.is_empty());
    }
}
