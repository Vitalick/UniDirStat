use super::*;
use unidirstat::scanner::{DirectoryScanStatus, ScanView};

#[derive(Default)]
pub(super) struct LiveTree {
    initialized: bool,
    revision: Option<(u64, bool)>,
    refreshed: Option<Instant>,
    receiver: Option<Receiver<Option<SortedRows>>>,
    cancel: Option<Arc<AtomicBool>>,
    sort: Option<(SortBy, bool, bool)>,
    dirty: bool,
    pub(super) visible: Vec<FileRow>,
}

struct SortedRows {
    rows: Vec<(usize, usize)>,
    roots: Vec<usize>,
    revision: (u64, bool),
}

impl LiveTree {
    pub(super) fn initialized(&self) -> bool {
        self.initialized
    }
}

impl Drop for LiveTree {
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

struct SortKey {
    index: usize,
    value: Option<u64>,
    name: String,
    path: String,
}

fn live_view<R>(
    control: &ScanControl,
    cancel: &AtomicBool,
    finalized: bool,
    mut callback: impl FnMut(ScanView<'_>) -> R,
) -> Option<R> {
    loop {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        if let Some(result) = control
            .with_live_view(|view| (view.is_finalized() == finalized).then(|| callback(view)))
        {
            return result;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn sort_keys(
    keys: &[SortKey],
    sort: SortBy,
    descending: bool,
    cancel: &AtomicBool,
) -> Option<Vec<usize>> {
    let compare = |&left: &usize, &right: &usize| {
        let left = &keys[left];
        let right = &keys[right];
        let order = if sort == SortBy::Name {
            let order = left.name.cmp(&right.name);
            if descending { order.reverse() } else { order }
        } else {
            match (left.value, right.value) {
                (Some(left), Some(right)) if descending => right.cmp(&left),
                (Some(left), Some(right)) => left.cmp(&right),
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, None) => std::cmp::Ordering::Equal,
            }
        };
        order
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.index.cmp(&right.index))
    };
    let mut order: Vec<_> = (0..keys.len()).collect();
    for chunk in order.chunks_mut(1024) {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        chunk.sort_unstable_by(compare);
    }
    let mut scratch = vec![0; order.len()];
    let mut width = 1024;
    while width < order.len() {
        for start in (0..order.len()).step_by(width * 2) {
            let middle = (start + width).min(order.len());
            let end = (middle + width).min(order.len());
            let (mut left, mut right) = (start, middle);
            for (offset, target) in scratch[start..end].iter_mut().enumerate() {
                if offset % 1024 == 0 && cancel.load(Ordering::Relaxed) {
                    return None;
                }
                if right == end || (left < middle && !compare(&order[left], &order[right]).is_gt())
                {
                    *target = order[left];
                    left += 1;
                } else {
                    *target = order[right];
                    right += 1;
                }
            }
        }
        std::mem::swap(&mut order, &mut scratch);
        width = width.saturating_mul(2);
    }
    (!cancel.load(Ordering::Relaxed)).then_some(order)
}

fn sorted_children(
    control: &ScanControl,
    cancel: &AtomicBool,
    finalized: bool,
    parent: Option<usize>,
    sort: (SortBy, bool, bool),
) -> Option<Vec<usize>> {
    let mut keys = Vec::new();
    let mut length = usize::MAX;
    while keys.len() < length {
        let start = keys.len();
        let (count, batch) = live_view(control, cancel, finalized, |view| {
            let children = parent.map_or(view.snapshot.roots.as_slice(), |index| {
                view.snapshot.nodes[index].children.as_slice()
            });
            let count = children.len().min(length);
            let batch: Vec<_> = children[start.min(count)..count.min(start + 1024)]
                .iter()
                .map(|&index| {
                    let node = &view.snapshot.nodes[index];
                    SortKey {
                        index,
                        value: match sort.0 {
                            SortBy::Size if sort.2 => node.allocated_size,
                            SortBy::Size => Some(node.logical_size),
                            SortBy::Modified => node.modified,
                            SortBy::Name => None,
                        },
                        name: if sort.0 == SortBy::Name {
                            node.name().to_lowercase()
                        } else {
                            String::new()
                        },
                        path: String::new(),
                    }
                })
                .collect();
            (count, batch)
        })?;
        length = count;
        keys.extend(batch);
    }
    let order = sort_keys(&keys, sort.0, sort.1, cancel)?;
    let mut ties = Vec::new();
    for pair in order.windows(2) {
        let equal = if sort.0 == SortBy::Name {
            keys[pair[0]].name == keys[pair[1]].name
        } else {
            keys[pair[0]].value == keys[pair[1]].value
        };
        if equal {
            if ties.last() != Some(&pair[0]) {
                ties.push(pair[0]);
            }
            ties.push(pair[1]);
        }
    }
    for chunk in ties.chunks(1024) {
        let paths = live_view(control, cancel, finalized, |view| {
            chunk
                .iter()
                .map(|&position| {
                    view.snapshot.nodes[keys[position].index]
                        .path
                        .to_string_lossy()
                        .to_lowercase()
                })
                .collect::<Vec<_>>()
        })?;
        for (&position, path) in chunk.iter().zip(paths) {
            keys[position].path = path;
        }
    }
    let order = if ties.is_empty() {
        order
    } else {
        sort_keys(&keys, sort.0, sort.1, cancel)?
    };
    Some(
        order
            .into_iter()
            .map(|position| keys[position].index)
            .collect(),
    )
}

fn sorted_rows(
    control: &ScanControl,
    cancel: &AtomicBool,
    revision: (u64, bool),
    mut expanded: HashSet<usize>,
    initialize: bool,
    sort: (SortBy, bool, bool),
) -> Option<SortedRows> {
    let roots = sorted_children(control, cancel, revision.1, None, sort)?;
    if initialize {
        expanded.extend(&roots);
    }
    let mut pending: Vec<_> = roots.iter().rev().map(|&index| (index, 0)).collect();
    let mut rows = Vec::new();
    while let Some((index, depth)) = pending.pop() {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        rows.push((index, depth));
        if expanded.contains(&index) {
            pending.extend(
                sorted_children(control, cancel, revision.1, Some(index), sort)?
                    .into_iter()
                    .rev()
                    .map(|child| (child, depth + 1)),
            );
        }
    }
    Some(SortedRows {
        rows,
        roots: if initialize { roots } else { Vec::new() },
        revision,
    })
}

pub(super) struct FileRow {
    pub index: usize,
    pub depth: usize,
    pub name: String,
    pub path: String,
    pub extension: String,
    pub size: u64,
    pub modified: Option<u64>,
    pub directory: bool,
    pub expandable: bool,
    pub parent: Option<usize>,
    pub limited: bool,
    pub share: f32,
    pub total_share: f64,
    pub status: DirectoryScanStatus,
    pub duration: Option<Duration>,
}

impl FileRow {
    pub(super) fn new(
        snapshot: &Snapshot,
        index: usize,
        depth: usize,
        allocated: bool,
        view: Option<&ScanView<'_>>,
    ) -> Self {
        let node = &snapshot.nodes[index];
        Self {
            index,
            depth,
            name: node.name(),
            path: display_path(&node.path),
            extension: node.extension(),
            size: node.size(allocated),
            modified: node.modified,
            directory: node.kind == EntryKind::Directory,
            parent: node.parent,
            expandable: !node.children.is_empty(),
            limited: node.depth_limited,
            share: workspace::parent_share(snapshot, index, allocated),
            total_share: node.size(allocated) as f64 / snapshot.total_size(allocated).max(1) as f64,
            status: view
                .and_then(|view| view.directory_status(index))
                .unwrap_or_else(|| {
                    if node.scan_warning {
                        DirectoryScanStatus::Warning
                    } else if node.scan_failed || node.error.is_some() {
                        DirectoryScanStatus::Failed
                    } else if snapshot.cancelled && node.scan_duration_ns == 0 {
                        DirectoryScanStatus::Cancelled
                    } else {
                        DirectoryScanStatus::Complete
                    }
                }),
            duration: view.map_or_else(
                || (node.scan_duration_ns > 0).then(|| Duration::from_nanos(node.scan_duration_ns)),
                |view| view.scan_duration(index),
            ),
        }
    }
}

pub(super) fn duration_label(duration: Option<Duration>) -> String {
    match duration {
        None => tr("Unavailable").to_owned(),
        Some(duration) if duration.as_millis() == 0 => tr("<1 ms").to_owned(),
        Some(duration) if duration.as_secs() == 0 => {
            format!("{} {}", duration.as_millis(), tr("ms"))
        }
        Some(duration) => format!(
            "{}:{:02}.{:01}",
            duration.as_secs() / 60,
            duration.as_secs() % 60,
            duration.subsec_millis() / 100
        ),
    }
}

pub(super) fn paint_scan_status(
    ui: &mut egui::Ui,
    status: DirectoryScanStatus,
    paused: bool,
    reduced_motion: bool,
) {
    let dark = usize::from(ui.visuals().dark_mode);
    let green = [
        Color32::from_rgb(27, 114, 64),
        Color32::from_rgb(103, 213, 147),
    ][dark];
    let orange = [
        Color32::from_rgb(159, 89, 4),
        Color32::from_rgb(244, 163, 64),
    ][dark];
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
    let center = rect.center();
    let status = if paused
        && matches!(
            status,
            DirectoryScanStatus::Queued | DirectoryScanStatus::Scanning
        ) {
        DirectoryScanStatus::Queued
    } else {
        status
    };
    let label = match status {
        DirectoryScanStatus::Complete => {
            ui.painter().add(egui::Shape::line(
                vec![
                    center + Vec2::new(-5.0, 0.0),
                    center + Vec2::new(-1.0, 4.0),
                    center + Vec2::new(6.0, -5.0),
                ],
                Stroke::new(1.8, green),
            ));
            tr("Complete")
        }
        DirectoryScanStatus::Queued => {
            for offset in [-4.0, 2.0] {
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(
                        center + Vec2::new(offset, -5.0),
                        Vec2::new(2.5, 10.0),
                    ),
                    0.5,
                    orange,
                );
            }
            if paused {
                tr("Paused")
            } else {
                tr("Waiting for an available worker")
            }
        }
        DirectoryScanStatus::Scanning => {
            if reduced_motion {
                ui.painter()
                    .circle_stroke(center, 6.0, Stroke::new(1.5, ACCENT));
                ui.painter()
                    .circle_filled(center + Vec2::new(0.0, -6.0), 2.0, ACCENT);
            } else {
                ui.put(rect, egui::Spinner::new().size(16.0).color(ACCENT));
            }
            tr("Scanning")
        }
        DirectoryScanStatus::Warning => {
            ui.painter().add(egui::Shape::closed_line(
                vec![
                    center + Vec2::new(0.0, -7.0),
                    center + Vec2::new(7.0, 6.0),
                    center + Vec2::new(-7.0, 6.0),
                ],
                Stroke::new(1.5, orange),
            ));
            ui.painter().line_segment(
                [center + Vec2::new(0.0, -2.0), center + Vec2::new(0.0, 1.0)],
                Stroke::new(1.8, orange),
            );
            ui.painter()
                .circle_filled(center + Vec2::new(0.0, 3.5), 0.9, orange);
            tr("Completed with some scan errors")
        }
        DirectoryScanStatus::Failed => {
            let color = ERROR_TEXT[dark];
            ui.painter().line_segment(
                [center + Vec2::new(0.0, -6.0), center + Vec2::new(0.0, 1.0)],
                Stroke::new(2.0, color),
            );
            ui.painter()
                .circle_filled(center + Vec2::new(0.0, 5.0), 1.3, color);
            tr("No entries could be read")
        }
        DirectoryScanStatus::Cancelled => {
            ui.painter().line_segment(
                [center - Vec2::new(5.0, 0.0), center + Vec2::new(5.0, 0.0)],
                Stroke::new(1.5, muted(ui)),
            );
            tr("Cancelled")
        }
    };
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, label));
    response.on_hover_text(label);
}

impl UniDirStatApp {
    pub(super) fn live_files(&mut self, ui: &mut egui::Ui) {
        let control = Arc::clone(&self.control);
        let revision = control.with_live_view(|view| (view.revision, view.is_finalized()));
        let sort = (self.sort_by, self.descending, self.settings.allocated);
        let tree = &mut self.live_tree;
        if tree.sort != Some(sort) {
            tree.dirty = true;
        }
        if tree.dirty {
            if let Some(cancel) = tree.cancel.take() {
                cancel.store(true, Ordering::Relaxed);
            }
            tree.receiver = None;
        }
        if let Some(result) = tree.receiver.as_ref().map(Receiver::try_recv) {
            match result {
                Ok(result) => {
                    tree.receiver = None;
                    if let Some(result) = result
                        && revision.is_some_and(|revision| revision.1 == result.revision.1)
                    {
                        self.rows = result.rows;
                        self.expanded.extend(result.roots);
                        tree.initialized = true;
                        tree.revision = Some(result.revision);
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => tree.receiver = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        let due = tree
            .refreshed
            .is_none_or(|last| last.elapsed() >= Duration::from_millis(400));
        if tree.receiver.is_none()
            && let Some(revision) = revision
            && (tree.dirty || (due && tree.revision != Some(revision)))
        {
            let (sender, receiver) = mpsc::channel();
            let cancel = Arc::new(AtomicBool::new(false));
            tree.cancel = Some(Arc::clone(&cancel));
            tree.receiver = Some(receiver);
            tree.sort = Some(sort);
            tree.dirty = false;
            tree.refreshed = Some(Instant::now());
            let expanded = self.expanded.clone();
            let initialize = !tree.initialized;
            let context = ui.ctx().clone();
            std::thread::spawn(move || {
                let result = sorted_rows(&control, &cancel, revision, expanded, initialize, sort);
                let _ = sender.send(result);
                context.request_repaint();
            });
        }
        self.file_table_contents(ui, None);
        ui.ctx().request_repaint_after(Duration::from_millis(200));
    }

    pub(super) fn live_expansion_changed(&mut self) {
        self.live_tree.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_sort_merges_multiple_batches_in_descending_order() {
        let keys: Vec<_> = (0..2050)
            .map(|index| SortKey {
                index,
                value: Some(((index * 17) % 2050) as u64),
                name: String::new(),
                path: String::new(),
            })
            .collect();
        let result = sort_keys(&keys, SortBy::Size, true, &AtomicBool::new(false)).unwrap();
        assert_eq!(result.len(), keys.len());
        assert!(
            result
                .windows(2)
                .all(|pair| keys[pair[0]].value >= keys[pair[1]].value)
        );
        assert!(sort_keys(&keys, SortBy::Size, true, &AtomicBool::new(true)).is_none());
    }

    #[test]
    fn completed_error_icons_distinguish_warning_triangles_and_total_failure() {
        for (status, warning) in [
            (DirectoryScanStatus::Warning, true),
            (DirectoryScanStatus::Failed, false),
        ] {
            let context = egui::Context::default();
            context.set_visuals(egui::Visuals::dark());
            let output = context.run(Default::default(), |context| {
                egui::CentralPanel::default()
                    .show(context, |ui| paint_scan_status(ui, status, false, true));
            });
            assert_eq!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Path(path) if path.closed && path.points.len() == 3)), warning);
            assert_eq!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Circle(circle) if circle.fill == ERROR_TEXT[1])), !warning);
        }
    }

    #[test]
    fn scan_status_icons_show_green_completion_orange_waiting_and_active_spinner() {
        for (status, paused, expected_pause_bars, expected_check, expected_ring) in [
            (DirectoryScanStatus::Complete, false, 0, true, false),
            (DirectoryScanStatus::Queued, false, 2, false, false),
            (DirectoryScanStatus::Scanning, false, 0, false, true),
            (DirectoryScanStatus::Scanning, true, 2, false, false),
        ] {
            let context = egui::Context::default();
            context.set_visuals(egui::Visuals::dark());
            let output = context.run(egui::RawInput::default(), |context| {
                egui::CentralPanel::default()
                    .show(context, |ui| paint_scan_status(ui, status, paused, true));
            });
            let orange = Color32::from_rgb(244, 163, 64);
            let green = Color32::from_rgb(103, 213, 147);
            assert_eq!(output.shapes.iter().filter(|shape| matches!(&shape.shape, egui::Shape::Rect(rect) if rect.fill == orange)).count(), expected_pause_bars);
            assert_eq!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Path(path) if path.points.len() == 3 && path.stroke.color == egui::epaint::ColorMode::Solid(green))), expected_check);
            assert_eq!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Circle(circle) if circle.stroke.color == ACCENT)), expected_ring);
        }
    }

    #[test]
    fn live_rows_sort_siblings_by_size_and_refresh_when_only_sizes_change() {
        let temporary = tempfile::tempdir().unwrap();
        std::fs::write(temporary.path().join("a"), [7; 1]).unwrap();
        std::fs::write(temporary.path().join("b"), [7; 2]).unwrap();
        let control = ScanControl::default();
        control.enable_live_view();
        for (size, expected) in [(1, ["b", "a"]), (3, ["a", "b"])] {
            std::fs::write(temporary.path().join("a"), vec![7; size]).unwrap();
            let mut snapshot = scanner::scan(
                &[temporary.path().into()],
                &ScanOptions::default(),
                &control,
            )
            .unwrap();
            snapshot.nodes[0].children.sort_unstable();
            control.set_completed_live_snapshot(Arc::new(snapshot));
            let revision = control
                .with_live_view(|view| (view.revision, view.is_finalized()))
                .unwrap();
            let result = sorted_rows(
                &control,
                &AtomicBool::new(false),
                revision,
                HashSet::new(),
                true,
                (SortBy::Size, true, false),
            )
            .unwrap();
            let names = control
                .with_live_view(|view| {
                    result.rows[1..]
                        .iter()
                        .map(|&(index, _)| view.snapshot.nodes[index].name())
                        .collect::<Vec<_>>()
                })
                .unwrap();
            assert_eq!(names, expected);
        }
    }

    #[test]
    fn live_sort_matches_final_sort_for_expanded_branches_and_all_headers() {
        let temporary = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temporary.path().join("deep/nested/closed")).unwrap();
        std::fs::write(temporary.path().join("deep/nested/closed/hidden"), [7; 3]).unwrap();
        std::fs::write(temporary.path().join("z"), [7; 4]).unwrap();
        std::fs::write(temporary.path().join("A"), [7; 4]).unwrap();
        let control = ScanControl::default();
        control.enable_live_view();
        let mut snapshot = scanner::scan(
            &[temporary.path().into()],
            &ScanOptions::default(),
            &control,
        )
        .unwrap();
        for node in &mut snapshot.nodes {
            node.modified = match node.name().as_str() {
                "z" => Some(10),
                "A" => Some(20),
                _ => None,
            };
        }
        let expanded = snapshot
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| {
                matches!(node.name().as_str(), "deep" | "nested") || node.parent.is_none()
            })
            .map(|(index, _)| index)
            .collect::<HashSet<_>>();
        let snapshot = Arc::new(snapshot);
        control.set_completed_live_snapshot(Arc::clone(&snapshot));
        let revision = control
            .with_live_view(|view| (view.revision, view.is_finalized()))
            .unwrap();
        let index =
            SearchIndex::with_control(Arc::clone(&snapshot), 1, &ScanControl::default()).unwrap();
        for sort in [SortBy::Size, SortBy::Name, SortBy::Modified] {
            for descending in [false, true] {
                let query = SearchQuery {
                    sort: match sort {
                        SortBy::Size => SearchSort::Size,
                        SortBy::Name => SearchSort::Name,
                        SortBy::Modified => SearchSort::Modified,
                    },
                    descending,
                    ..SearchQuery::default()
                };
                let expected = build_rows(
                    &snapshot,
                    &index,
                    &query,
                    false,
                    &expanded,
                    &ScanControl::default(),
                )
                .unwrap();
                let result = sorted_rows(
                    &control,
                    &AtomicBool::new(false),
                    revision,
                    expanded.clone(),
                    false,
                    (sort, descending, false),
                )
                .unwrap();
                assert_eq!(result.rows, expected);
                assert!(
                    !result
                        .rows
                        .iter()
                        .any(|&(index, _)| snapshot.nodes[index].name() == "hidden")
                );
                assert!(
                    result
                        .rows
                        .iter()
                        .any(|&(index, depth)| snapshot.nodes[index].name() == "closed"
                            && depth == 3)
                );
            }
        }
        assert!(
            sorted_rows(
                &control,
                &AtomicBool::new(true),
                revision,
                expanded.clone(),
                false,
                (SortBy::Size, true, false)
            )
            .is_none()
        );
        assert!(
            sorted_rows(
                &control,
                &AtomicBool::new(false),
                (revision.0, false),
                expanded,
                false,
                (SortBy::Size, true, false)
            )
            .is_none()
        );
    }

    #[test]
    fn timing_distinguishes_unavailable_submillisecond_and_long_scans() {
        assert_eq!(duration_label(None), tr("Unavailable"));
        assert_eq!(duration_label(Some(Duration::from_nanos(10))), tr("<1 ms"));
        assert_eq!(duration_label(Some(Duration::from_millis(1250))), "0:01.2");
        assert_eq!(duration_label(Some(Duration::from_secs(125))), "2:05.0");
    }
}
