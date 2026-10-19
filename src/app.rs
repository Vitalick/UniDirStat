use std::{
    collections::{HashMap, HashSet},
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::file_colors::FileColors;
use crate::i18n::{Language, set_language, tr, trf};
use crate::map_job::{MapData, MapKey, MapKind, MapWorker};
use crate::map_view::MapViewport;
use crate::selection::Selection;
use crate::volumes::{Volume, mounted_volumes};
use eframe::egui::{
    self, Align, Color32, CornerRadius, FontId, Layout, RichText, Sense, Stroke, StrokeKind, Vec2,
};
use notify::{RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use unidirstat::{
    actions,
    analysis::{self, DuplicateReport, ExtensionStats, HashAlgorithm},
    model::{EntryKind, Node, Snapshot, display_path},
    report,
    scanner::{self, ScanControl, ScanOptions, ScanPhase},
    search::{SearchIndex, SearchQuery, SearchSort, SearchTarget},
};

const ACCENT: Color32 = Color32::from_rgb(119, 131, 255);
mod map_canvas;
mod workspace;
use workspace::language_icon;
const PRIMARY_BUTTON: Color32 = Color32::from_rgb(90, 102, 220);
const BODY_SIZE: f32 = 16.0;
const HELP_SIZE: f32 = 14.0;
const CONTROL_HEIGHT: f32 = 40.0;
const ROW_HEIGHT: f32 = 38.0;
const WATCH_PATH_LIMIT: usize = 1024;
const DEFAULT_COLUMN_WIDTHS: [f32; 4] = [0.0, 132.0, 144.0, 176.0];
const CONTROL_BORDER: [Color32; 2] = [
    Color32::from_rgb(112, 126, 147),
    Color32::from_rgb(99, 115, 140),
];
const ERROR_TEXT: [Color32; 2] = [
    Color32::from_rgb(164, 34, 59),
    Color32::from_rgb(255, 163, 178),
];
const WARNING_TEXT: [Color32; 2] = [
    Color32::from_rgb(133, 77, 9),
    Color32::from_rgb(246, 193, 104),
];
const INPUT_MARGIN: egui::Margin = egui::Margin::symmetric(12, 8);

fn interface_fonts() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "Ubuntu-Regular".into(),
        Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/fonts/Ubuntu-Regular.ttf"
        ))),
    );
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "Ubuntu-Regular".into());
    fonts
}
const PALETTE: [Color32; 10] = [
    Color32::from_rgb(116, 132, 238),
    Color32::from_rgb(80, 182, 170),
    Color32::from_rgb(194, 131, 221),
    Color32::from_rgb(228, 164, 103),
    Color32::from_rgb(91, 162, 216),
    Color32::from_rgb(214, 123, 149),
    Color32::from_rgb(157, 183, 108),
    Color32::from_rgb(146, 146, 218),
    Color32::from_rgb(86, 178, 191),
    Color32::from_rgb(200, 173, 117),
];

#[derive(Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
enum MapView {
    #[default]
    Treemap,
    Flame,
    Sunburst,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    language: Language,
    theme: Theme,
    reduced_motion: bool,
    mac_access_reviewed: bool,
    font_scale: f32,
    show_modified: bool,
    show_share: bool,
    threads: usize,
    allocated: bool,
    tile_gap: f32,
    labels: bool,
    map_view: MapView,
    exclude: String,
    one_file_system: bool,
    price_per_gib: f64,
    filters: Vec<SavedFilter>,
    hash_algorithm: HashAlgorithm,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            language: Language::System,
            theme: Theme::System,
            reduced_motion: false,
            mac_access_reviewed: false,
            font_scale: 0.8,
            show_modified: true,
            show_share: true,
            threads: 0,
            allocated: false,
            tile_gap: 2.0,
            labels: true,
            map_view: MapView::Treemap,
            exclude: String::new(),
            one_file_system: false,
            price_per_gib: 0.023,
            filters: Vec::new(),
            hash_algorithm: HashAlgorithm::default(),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Overview,
    Files,
    Largest,
    Types,
    Duplicates,
    Storage,
    Issues,
    Settings,
}

impl Page {
    fn title(self) -> &'static str {
        match self {
            Self::Overview => tr("Overview"),
            Self::Files => tr("All files"),
            Self::Largest => tr("Largest files"),
            Self::Types => tr("File types"),
            Self::Duplicates => tr("Duplicates"),
            Self::Storage => tr("Storage insights"),
            Self::Issues => tr("Scan issues"),
            Self::Settings => tr("Settings"),
        }
    }
    fn subtitle(self) -> &'static str {
        match self {
            Self::Overview => tr("A little perspective on what takes up space."),
            Self::Files => tr("Explore your folders, one level at a time."),
            Self::Largest => tr("The biggest opportunities to reclaim space."),
            Self::Types => tr("Understand your storage by file extension."),
            Self::Duplicates => tr("Find identical files with full content verification."),
            Self::Storage => tr("File age, permissions and estimated storage cost."),
            Self::Issues => tr("Paths that could not be fully inspected."),
            Self::Settings => tr("Make this space yours."),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum SortBy {
    Size,
    Name,
    Modified,
}

#[derive(Clone, Serialize, Deserialize)]
struct SavedFilter {
    name: String,
    query: SearchQuery,
}

struct PreparedSnapshot {
    snapshot: Arc<Snapshot>,
    index: Option<Arc<SearchIndex>>,
    extensions: [Vec<ExtensionStats>; 2],
    largest: Vec<usize>,
    age: ([u64; 4], [u64; 4]),
    analysis_complete: bool,
    error: Option<String>,
    refresh_view: Option<Box<RefreshView>>,
}

struct RefreshView {
    selected: Option<usize>,
    selection: Vec<usize>,
    primary: Option<usize>,
    scope: Option<usize>,
    search_scope: Option<usize>,
    map_focus: Option<usize>,
    expanded: HashSet<usize>,
    refreshed: Option<usize>,
}

impl RefreshView {
    fn remap(
        &mut self,
        previous: &Snapshot,
        current: &Snapshot,
        control: &ScanControl,
    ) -> Result<(), String> {
        let requested = self
            .selected
            .iter()
            .chain(&self.selection)
            .chain(self.primary.iter())
            .chain(self.scope.iter())
            .chain(self.search_scope.iter())
            .chain(self.map_focus.iter())
            .chain(&self.expanded)
            .chain(self.refreshed.iter());
        let paths: HashMap<_, _> = requested
            .filter_map(|&index| {
                previous
                    .nodes
                    .get(index)
                    .map(|node| (node.path.as_path(), index))
            })
            .collect();
        let mut mapping = HashMap::with_capacity(paths.len());
        for (index, node) in current.nodes.iter().enumerate() {
            if index % 256 == 0 && !control.checkpoint() {
                return Err(
                    tr("Folder recalculation cancelled. Previous results are preserved.").into(),
                );
            }
            if let Some(&previous) = paths.get(node.path.as_path()) {
                mapping.insert(previous, index);
            }
        }
        let remap = |index: usize| mapping.get(&index).copied();
        let directory = |index: usize| {
            remap(index).filter(|&index| current.nodes[index].kind == EntryKind::Directory)
        };
        self.selected = self.selected.and_then(remap);
        self.selection = self
            .selection
            .iter()
            .filter_map(|&index| remap(index))
            .collect();
        self.primary = self.primary.and_then(remap);
        self.scope = self.scope.and_then(directory);
        self.search_scope = self.search_scope.and_then(directory);
        self.map_focus = self.map_focus.and_then(directory);
        self.refreshed = self.refreshed.and_then(directory);
        self.expanded = self
            .expanded
            .iter()
            .filter_map(|&index| directory(index))
            .collect();
        Ok(())
    }
}

struct RowsReady {
    generation: u64,
    index: Option<Arc<SearchIndex>>,
    rows: Result<Vec<(usize, usize)>, String>,
}

impl RowsReady {
    fn is_current(&self, generation: u64, control: &ScanControl) -> bool {
        self.generation == generation && !control.is_cancelled()
    }
}

fn build_rows(
    snapshot: &Snapshot,
    index: &SearchIndex,
    query: &SearchQuery,
    flat: bool,
    expanded: &HashSet<usize>,
    control: &ScanControl,
) -> Result<Vec<(usize, usize)>, String> {
    if flat {
        let found = index.search(query, control)?;
        control.set_phase(ScanPhase::Mapping);
        control.start_work(found.len() as u64);
        let mut rows = Vec::with_capacity(found.len());
        for index in found {
            if !control.checkpoint() {
                return Err(tr("Cancelled").into());
            }
            rows.push((index, 0));
            control.advance_work(1);
        }
        return Ok(rows);
    }
    let mut roots = snapshot.roots.clone();
    index.sort_nodes(&mut roots, query, control)?;
    let mut pending: Vec<_> = roots.into_iter().rev().map(|index| (index, 0)).collect();
    let mut rows = Vec::new();
    while let Some((node_index, depth)) = pending.pop() {
        if !control.checkpoint() {
            return Err(tr("Cancelled").into());
        }
        rows.push((node_index, depth));
        if expanded.contains(&node_index) {
            let mut children = snapshot.nodes[node_index].children.clone();
            index.sort_nodes(&mut children, query, control)?;
            pending.extend(children.into_iter().rev().map(|child| (child, depth + 1)));
        }
    }
    control.set_phase(ScanPhase::Mapping);
    control.start_work(rows.len() as u64);
    control.advance_work(rows.len() as u64);
    Ok(rows)
}

fn save_filter(settings: &mut Settings, name: &str, query: &SearchQuery) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Err(tr("Use a filter name between 1 and 80 characters").into());
    }
    let mut query = query.clone();
    query.scope = None;
    let saved = SavedFilter {
        name: name.into(),
        query,
    };
    if let Some(existing) = settings
        .filters
        .iter_mut()
        .find(|filter| filter.name == name)
    {
        *existing = saved;
    } else {
        if settings.filters.len() >= 128 {
            return Err(tr("At most 128 filters can be saved").into());
        }
        settings.filters.push(saved);
    }
    Ok(())
}

fn has_filters(query: &SearchQuery) -> bool {
    !query.text.is_empty()
        || query.min_size.is_some()
        || query.max_size.is_some()
        || query.min_age_seconds.is_some()
        || query.max_age_seconds.is_some()
        || !query.kinds.is_empty()
        || query.extension.is_some()
        || query.owner.is_some()
        || query.scope.is_some()
        || query.empty_directories
}

enum JobResult {
    Scan(Result<PreparedSnapshot, String>),
    Load(Result<PreparedSnapshot, String>),
    Duplicates(DuplicateReport),
    Permissions(PathBuf, Result<String, String>),
    Cleanup(Result<usize, String>),
    Io(Result<String, String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JobKind {
    Scan,
    Refresh,
    Load,
    Duplicates,
    SaveReport,
    SaveDuplicates,
    FileOperation,
}

impl JobKind {
    fn label(self) -> &'static str {
        match self {
            Self::Scan => tr("Scanning"),
            Self::Refresh => tr("Refreshing folder"),
            Self::Load => tr("Opening snapshot"),
            Self::Duplicates => tr("Hashing duplicates"),
            Self::SaveReport => tr("Saving report"),
            Self::SaveDuplicates => tr("Saving duplicates"),
            Self::FileOperation => tr("File operation"),
        }
    }

    fn cancellable(self) -> bool {
        matches!(self, Self::Scan | Self::Refresh | Self::Duplicates)
    }

    fn reports_work(self) -> bool {
        matches!(
            self,
            Self::Scan | Self::Refresh | Self::Load | Self::Duplicates
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JobState {
    Running,
    Paused,
    Cancelling,
}

fn job_state(control: &ScanControl) -> JobState {
    if control.is_cancelled() {
        JobState::Cancelling
    } else if control.is_paused() {
        JobState::Paused
    } else {
        JobState::Running
    }
}

fn queue_watch_paths(
    event: notify::Event,
    sender: &mpsc::SyncSender<PathBuf>,
    changed: &AtomicBool,
) {
    if event.need_rescan()
        || event.paths.is_empty()
        || matches!(
            event.kind,
            notify::EventKind::Any | notify::EventKind::Other
        )
    {
        changed.store(true, Ordering::Relaxed);
        return;
    }
    for path in event.paths {
        if sender.try_send(path).is_err() {
            changed.store(true, Ordering::Relaxed);
            break;
        }
    }
}

fn page_needs_rows(page: Page, largest: bool) -> bool {
    matches!(page, Page::Overview | Page::Files | Page::Largest)
        && (page == Page::Largest) != largest
}

fn click_modifiers(context: &egui::Context) -> egui::Modifiers {
    context.input(|input| {
        input
            .events
            .iter()
            .rev()
            .find_map(|event| match event {
                egui::Event::PointerButton {
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers,
                    ..
                } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or(input.modifiers)
    })
}

fn phase_label(phase: ScanPhase) -> &'static str {
    match phase {
        ScanPhase::Idle => tr("Starting"),
        ScanPhase::Discovering => tr("Discovering files"),
        ScanPhase::Finalizing => tr("Finalizing totals"),
        ScanPhase::Complete => tr("Preparing results"),
        ScanPhase::Indexing => tr("Indexing files"),
        ScanPhase::Summarizing => tr("Calculating statistics"),
        ScanPhase::Sorting => tr("Sorting results"),
        ScanPhase::Mapping => tr("Building file view"),
        ScanPhase::Filtering => tr("Filtering entries"),
        ScanPhase::Sampling => tr("Comparing file samples"),
        ScanPhase::Hashing => tr("Verifying full contents"),
        ScanPhase::Validating => tr("Validating file identities"),
    }
}

fn show_work_progress(ui: &mut egui::Ui, control: &ScanControl) {
    let total = control.work_total.load(Ordering::Relaxed);
    let done = control.work_done.load(Ordering::Relaxed).min(total);
    if total > 0 {
        ui.monospace(trf(
            "{stage}: {done} / {total}",
            &[
                ("stage", phase_label(control.phase()).into()),
                ("done", done.to_string()),
                ("total", total.to_string()),
            ],
        ));
        ui.add(egui::ProgressBar::new(done as f32 / total as f32).desired_height(5.0));
    } else {
        ui.label(phase_label(control.phase()));
    }
}

#[derive(Clone, Copy)]
enum SelectionView {
    Files,
    Overview,
    Duplicates,
    Single,
}

fn duplicate_file_order(rows: &[DuplicateRow]) -> Vec<(usize, usize)> {
    rows.iter()
        .filter_map(|row| match row {
            DuplicateRow::File(index) => Some((*index, 0)),
            _ => None,
        })
        .collect()
}

fn load_settings(path: &Path) -> Result<Settings, String> {
    const LIMIT: u64 = 1024 * 1024;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Settings::default());
        }
        Err(error) => return Err(error.to_string()),
    };
    if file.metadata().map_err(|error| error.to_string())?.len() > LIMIT {
        return Err(tr("Preferences exceed the 1 MiB limit").into());
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > LIMIT {
        return Err(tr("Preferences exceed the 1 MiB limit").into());
    }
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}
enum Cleanup {
    Trash(usize),
    TrashEmpty(usize),
    Move(usize, PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DuplicateRow {
    Group(usize),
    Hash(usize),
    File(usize),
}

fn duplicate_hash_label(algorithm: HashAlgorithm, hash: &str) -> String {
    let algorithm = match algorithm {
        HashAlgorithm::Sha256 => "SHA-256",
        HashAlgorithm::Sha512 => "SHA-512",
    };
    format!("{algorithm} {hash}")
}

fn duplicate_summary(report: &DuplicateReport) -> (Vec<DuplicateRow>, u64) {
    let mut rows = Vec::new();
    let mut reclaimable = 0_u64;
    for (index, group) in report.groups.iter().enumerate() {
        rows.push(DuplicateRow::Group(index));
        rows.push(DuplicateRow::Hash(index));
        rows.extend(group.nodes.iter().copied().map(DuplicateRow::File));
        reclaimable = reclaimable.saturating_add(
            group
                .size
                .saturating_mul(group.nodes.len().saturating_sub(1) as u64),
        );
    }
    (rows, reclaimable)
}

fn storage_buckets(snapshot: &Snapshot, timestamp: u64) -> ([u64; 4], [u64; 4]) {
    storage_buckets_with_control(snapshot, timestamp, &ScanControl::default()).unwrap_or_default()
}

fn storage_buckets_with_control(
    snapshot: &Snapshot,
    timestamp: u64,
    control: &ScanControl,
) -> Option<([u64; 4], [u64; 4])> {
    control.set_phase(ScanPhase::Summarizing);
    control.start_work(snapshot.nodes.len() as u64);
    let mut sizes = [0_u64; 4];
    let mut files = [0_u64; 4];
    for node in &snapshot.nodes {
        if !control.checkpoint() {
            return None;
        }
        control.advance_work(1);
        if node.kind != EntryKind::File {
            continue;
        }
        let bucket = match node.modified {
            Some(modified) if timestamp.saturating_sub(modified) < 30 * 86_400 => 0,
            Some(modified) if timestamp.saturating_sub(modified) < 180 * 86_400 => 1,
            Some(_) => 2,
            None => 3,
        };
        sizes[bucket] = sizes[bucket].saturating_add(node.logical_size);
        files[bucket] = files[bucket].saturating_add(1);
    }
    Some((sizes, files))
}

fn prepare_snapshot(
    mut snapshot: Snapshot,
    threads: usize,
    control: &ScanControl,
) -> PreparedSnapshot {
    snapshot.cancelled |= control.is_cancelled();
    let mut prepared = PreparedSnapshot {
        snapshot: Arc::new(snapshot),
        index: None,
        extensions: [Vec::new(), Vec::new()],
        largest: Vec::new(),
        age: ([0; 4], [0; 4]),
        analysis_complete: false,
        error: None,
        refresh_view: None,
    };
    let analysis = (|| {
        prepared.index = Some(Arc::new(SearchIndex::with_control(
            Arc::clone(&prepared.snapshot),
            threads,
            control,
        )?));
        prepared.extensions[0] =
            analysis::extensions_with_control(&prepared.snapshot, false, control)?;
        prepared.extensions[1] =
            analysis::extensions_with_control(&prepared.snapshot, true, control)?;
        prepared.largest = analysis::largest_files_with_control(&prepared.snapshot, 5, control)?;
        prepared.age = storage_buckets_with_control(&prepared.snapshot, now(), control)
            .ok_or_else(|| tr("Cancelled").to_owned())?;
        Ok::<(), String>(())
    })();
    prepared.analysis_complete = analysis.is_ok();
    if let Err(error) = analysis
        && !control.is_cancelled()
    {
        prepared.error = Some(error);
    }
    if control.is_cancelled() {
        prepared.index = None;
        if let Some(snapshot) = Arc::get_mut(&mut prepared.snapshot) {
            snapshot.cancelled = true;
        }
    }
    prepared
}

fn muted(ui: &egui::Ui) -> Color32 {
    if ui.visuals().dark_mode {
        Color32::from_rgb(137, 148, 169)
    } else {
        Color32::from_rgb(94, 107, 130)
    }
}

fn extension_label(extension: &str) -> &str {
    if extension == "(no extension)" {
        tr("No extension")
    } else {
        extension
    }
}

fn kind_label(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::File => tr("File"),
        EntryKind::Directory => tr("Directory"),
        EntryKind::Symlink => tr("Symlink"),
        EntryKind::Other => tr("Other"),
    }
}

pub struct UniDirStatApp {
    settings: Settings,
    settings_path: Option<PathBuf>,
    page: Page,
    snapshot: Option<Arc<Snapshot>>,
    live: bool,
    roots: Vec<PathBuf>,
    path_input: String,
    receiver: Option<Receiver<JobResult>>,
    job_kind: Option<JobKind>,
    control: Arc<ScanControl>,
    job_started: Instant,
    progress_marker: (u64, u64, u64, u8),
    progress_changed: Instant,
    duplicates: Option<Arc<DuplicateReport>>,
    duplicate_rows: Vec<DuplicateRow>,
    duplicate_reclaimable: u64,
    extensions: [Vec<ExtensionStats>; 2],
    file_colors: Arc<FileColors>,
    selected: Option<usize>,
    selection: Selection,
    rows_largest: bool,
    last_page: Page,
    access_open: bool,
    scan_after_access: bool,
    scope: Option<usize>,
    expanded: HashSet<usize>,
    search_query: SearchQuery,
    search_index: Option<Arc<SearchIndex>>,
    search_receiver: Option<Receiver<RowsReady>>,
    search_control: Arc<ScanControl>,
    search_generation: u64,
    search_started: Instant,
    filter_name: String,
    filter_error: Option<String>,
    rows: Vec<(usize, usize)>,
    sort_by: SortBy,
    descending: bool,
    rebuild: bool,
    status: String,
    error: Option<String>,
    cleanup: Option<Cleanup>,
    refresh_after_cleanup: bool,
    watching: bool,
    watcher: Option<notify::RecommendedWatcher>,
    changed: Arc<AtomicBool>,
    watch_paths: Option<(mpsc::SyncSender<PathBuf>, Receiver<PathBuf>)>,
    pending_watch_paths: HashSet<PathBuf>,
    watch_full_scan: bool,
    last_refreshed: Option<(PathBuf, usize)>,
    last_change: Option<Instant>,
    last_dark: Option<bool>,
    map_worker: MapWorker,
    map_viewport: MapViewport,
    map_focus: Option<usize>,
    reveal_row: Option<usize>,
    column_widths: [f32; 4],
    summary_open: bool,
    compact_map: bool,
    permission_details: Option<(PathBuf, String)>,
    volumes: Vec<Volume>,
    largest: Vec<usize>,
    age_sizes: [u64; 4],
    age_files: [u64; 4],
    age_day: u64,
    analysis_complete: bool,
    watch_errors: Option<Receiver<String>>,
}

impl UniDirStatApp {
    pub fn new(
        context: &eframe::CreationContext<'_>,
        roots: Vec<PathBuf>,
        load: Option<PathBuf>,
        options: ScanOptions,
        settings_path: Option<PathBuf>,
    ) -> Self {
        set_language(Language::System);
        let (mut settings, settings_path, settings_error) = if let Some(path) = settings_path {
            match load_settings(&path) {
                Ok(settings) => (settings, Some(path), None),
                Err(error) => (
                    Settings::default(),
                    None,
                    Some(format!("{}: {error}", display_path(&path))),
                ),
            }
        } else {
            (
                context
                    .storage
                    .and_then(|storage| eframe::get_value(storage, "settings"))
                    .unwrap_or_default(),
                None,
                None,
            )
        };
        set_language(settings.language);
        if !options.exclude.is_empty() {
            settings.exclude = options.exclude.join("\n");
        }
        settings.one_file_system =
            cfg!(unix) && (settings.one_file_system || options.one_file_system);
        if options.threads > 0 {
            settings.threads = options.threads;
        }
        settings.threads = settings.threads.min(64);
        settings.font_scale = settings.font_scale.clamp(0.75, 2.0);
        context.egui_ctx.set_fonts(interface_fonts());
        context.egui_ctx.set_zoom_factor(settings.font_scale);
        settings.tile_gap = settings.tile_gap.clamp(1.0, 6.0);
        settings.price_per_gib = settings.price_per_gib.clamp(0.0, 1000.0);
        let access_open =
            cfg!(target_os = "macos") && !settings.mac_access_reviewed && load.is_none();
        let mut app = Self {
            settings,
            settings_path,
            page: Page::Overview,
            snapshot: None,
            live: false,
            roots,
            path_input: String::new(),
            receiver: None,
            job_kind: None,
            control: Arc::default(),
            job_started: Instant::now(),
            progress_marker: (0, 0, 0, 0),
            progress_changed: Instant::now(),
            duplicates: None,
            duplicate_rows: Vec::new(),
            duplicate_reclaimable: 0,
            extensions: [Vec::new(), Vec::new()],
            file_colors: Arc::default(),
            selected: None,
            selection: Selection::default(),
            rows_largest: false,
            last_page: Page::Overview,
            access_open,
            scan_after_access: false,
            scope: None,
            expanded: HashSet::new(),
            search_query: SearchQuery::default(),
            search_index: None,
            search_receiver: None,
            search_control: Arc::default(),
            search_generation: 0,
            search_started: Instant::now(),
            filter_name: String::new(),
            filter_error: None,
            rows: Vec::new(),
            sort_by: SortBy::Size,
            descending: true,
            rebuild: true,
            status: tr("Ready to explore").into(),
            error: settings_error.map(|details| {
                trf(
                    "Preferences were not loaded: {details}",
                    &[("details", details)],
                )
            }),
            cleanup: None,
            refresh_after_cleanup: false,
            watching: false,
            watcher: None,
            changed: Arc::new(AtomicBool::new(false)),
            watch_paths: None,
            pending_watch_paths: HashSet::new(),
            watch_full_scan: false,
            last_refreshed: None,
            last_change: None,
            last_dark: None,
            map_worker: MapWorker::default(),
            map_viewport: MapViewport::default(),
            map_focus: None,
            reveal_row: None,
            column_widths: DEFAULT_COLUMN_WIDTHS,
            summary_open: false,
            compact_map: false,
            permission_details: None,
            volumes: mounted_volumes(),
            largest: Vec::new(),
            age_sizes: [0; 4],
            age_files: [0; 4],
            age_day: 0,
            analysis_complete: false,
            watch_errors: None,
        };
        app.apply_theme(&context.egui_ctx);
        if let Some(path) = load {
            app.load(path, &context.egui_ctx);
        } else if !app.roots.is_empty() {
            app.start_scan(&context.egui_ctx);
        }
        app
    }

    fn apply_theme(&mut self, context: &egui::Context) {
        context.set_theme(match self.settings.theme {
            Theme::System => egui::ThemePreference::System,
            Theme::Light => egui::ThemePreference::Light,
            Theme::Dark => egui::ThemePreference::Dark,
        });
        let animation_time = if self.settings.reduced_motion {
            0.0
        } else {
            0.16
        };
        if context.style().animation_time != animation_time {
            context.style_mut(|style| style.animation_time = animation_time);
        }
        let dark = context.style().visuals.dark_mode;
        if self.last_dark == Some(dark) {
            return;
        }
        self.last_dark = Some(dark);
        let mut style = (*context.style()).clone();
        style.spacing.item_spacing = Vec2::new(10.0, 10.0);
        style.spacing.button_padding = Vec2::new(12.0, 8.0);
        style.spacing.interact_size.y = CONTROL_HEIGHT;
        style
            .text_styles
            .insert(egui::TextStyle::Heading, FontId::proportional(29.0));
        style
            .text_styles
            .insert(egui::TextStyle::Body, FontId::proportional(BODY_SIZE));
        style
            .text_styles
            .insert(egui::TextStyle::Button, FontId::proportional(BODY_SIZE));
        style
            .text_styles
            .insert(egui::TextStyle::Small, FontId::proportional(HELP_SIZE));
        style
            .text_styles
            .insert(egui::TextStyle::Monospace, FontId::monospace(BODY_SIZE));
        style.visuals.error_fg_color = ERROR_TEXT[usize::from(dark)];
        style.visuals.warn_fg_color = WARNING_TEXT[usize::from(dark)];
        style.visuals.weak_text_color = Some(if dark {
            Color32::from_rgb(162, 173, 194)
        } else {
            Color32::from_rgb(84, 97, 120)
        });
        style.visuals.panel_fill = if dark {
            Color32::from_rgb(17, 21, 31)
        } else {
            Color32::from_rgb(246, 247, 251)
        };
        style.visuals.window_fill = if dark {
            Color32::from_rgb(25, 30, 43)
        } else {
            Color32::WHITE
        };
        style.visuals.extreme_bg_color = if dark {
            Color32::from_rgb(13, 17, 26)
        } else {
            Color32::from_rgb(236, 239, 246)
        };
        style.visuals.faint_bg_color = if dark {
            Color32::from_rgb(24, 29, 41)
        } else {
            Color32::from_rgb(239, 242, 248)
        };
        let text = if dark {
            Color32::from_rgb(218, 225, 237)
        } else {
            Color32::from_rgb(39, 50, 67)
        };
        let accent = if dark { ACCENT } else { PRIMARY_BUTTON };
        let separator = if dark {
            Color32::from_rgb(43, 54, 73)
        } else {
            Color32::from_rgb(210, 217, 229)
        };
        style.visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, text);
        style.visuals.widgets.noninteractive.bg_fill = style.visuals.window_fill;
        style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, separator);
        style.visuals.window_stroke = Stroke::new(1.0_f32, separator);
        style.visuals.selection.bg_fill = if dark {
            Color32::from_rgb(45, 53, 85)
        } else {
            Color32::from_rgb(223, 229, 255)
        };
        style.visuals.selection.stroke = Stroke::new(
            2.0_f32,
            if dark {
                Color32::from_rgb(177, 187, 255)
            } else {
                Color32::from_rgb(61, 73, 168)
            },
        );
        style.visuals.hyperlink_color = if dark { ACCENT } else { PRIMARY_BUTTON };
        for (widget, fill) in [
            (
                &mut style.visuals.widgets.inactive,
                if dark {
                    Color32::from_rgb(26, 33, 47)
                } else {
                    Color32::from_rgb(239, 242, 248)
                },
            ),
            (
                &mut style.visuals.widgets.hovered,
                if dark {
                    Color32::from_rgb(35, 45, 63)
                } else {
                    Color32::from_rgb(227, 233, 245)
                },
            ),
            (
                &mut style.visuals.widgets.active,
                if dark {
                    Color32::from_rgb(43, 55, 76)
                } else {
                    Color32::from_rgb(216, 224, 242)
                },
            ),
            (
                &mut style.visuals.widgets.open,
                if dark {
                    Color32::from_rgb(31, 41, 58)
                } else {
                    Color32::from_rgb(228, 235, 248)
                },
            ),
        ] {
            widget.bg_fill = fill;
            widget.weak_bg_fill = fill;
            widget.bg_stroke = Stroke::new(1.0_f32, CONTROL_BORDER[usize::from(dark)]);
            widget.fg_stroke = Stroke::new(1.0_f32, text);
            widget.corner_radius = CornerRadius::same(7);
            widget.expansion = 0.0;
        }
        style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, accent);
        style.visuals.widgets.active.bg_stroke = Stroke::new(2.0_f32, accent);
        style.visuals.widgets.open.bg_stroke = Stroke::new(1.0_f32, accent);
        style.visuals.code_bg_color = style.visuals.widgets.inactive.bg_fill;
        context.set_style(style);
    }

    fn job(
        &mut self,
        kind: JobKind,
        context: &egui::Context,
        task: impl FnOnce() -> JobResult + Send + 'static,
    ) {
        let (sender, receiver) = mpsc::channel();
        let context = context.clone();
        self.receiver = Some(receiver);
        self.job_kind = Some(kind);
        self.job_started = Instant::now();
        self.progress_changed = Instant::now();
        std::thread::spawn(move || {
            let result = task();
            let _ = sender.send(result);
            context.request_repaint();
        });
    }

    fn start_scan(&mut self, context: &egui::Context) {
        if self.roots.is_empty() || self.receiver.is_some() {
            return;
        }
        if cfg!(target_os = "macos") && !self.settings.mac_access_reviewed {
            self.access_open = true;
            self.scan_after_access = true;
            return;
        }
        self.cleanup = None;
        self.refresh_after_cleanup = false;
        self.last_refreshed = None;
        self.search_control.cancel();
        self.search_receiver = None;
        self.control = Arc::default();
        let roots = self.roots.clone();
        let control = Arc::clone(&self.control);
        let options = self.scan_options();
        self.job(JobKind::Scan, context, move || {
            JobResult::Scan(
                scanner::scan(&roots, &options, &control)
                    .map(|snapshot| prepare_snapshot(snapshot, options.threads, &control)),
            )
        });
    }

    fn scan_options(&self) -> ScanOptions {
        ScanOptions {
            exclude: self
                .settings
                .exclude
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(str::to_owned)
                .collect(),
            one_file_system: cfg!(unix) && self.settings.one_file_system,
            threads: self.settings.threads,
        }
    }

    fn recalculation_target(&self) -> Option<usize> {
        if !self.live || self.receiver.is_some() {
            return None;
        }
        let snapshot = self.snapshot.as_ref()?;
        let directory = |index: usize| {
            snapshot
                .nodes
                .get(index)
                .filter(|node| node.kind == EntryKind::Directory)
                .map(|_| index)
        };
        self.selection
            .primary()
            .or(self.selected)
            .and_then(|index| {
                let node = snapshot.nodes.get(index)?;
                match node.kind {
                    EntryKind::Directory => Some(index),
                    EntryKind::File => node.parent.and_then(directory),
                    _ => None,
                }
            })
            .or_else(|| self.scope.and_then(directory))
    }

    fn refresh_folder(&mut self, index: usize, context: &egui::Context) {
        if !self.live || self.receiver.is_some() {
            return;
        }
        let Some(snapshot) = self.snapshot.as_ref().map(Arc::clone) else {
            return;
        };
        if !snapshot
            .nodes
            .get(index)
            .is_some_and(|node| node.kind == EntryKind::Directory)
        {
            return;
        }
        let mut view = RefreshView {
            selected: self.selected,
            selection: self.selection.indices().collect(),
            primary: self.selection.primary(),
            scope: self.scope,
            search_scope: self.search_query.scope,
            map_focus: self.map_focus,
            expanded: self.expanded.clone(),
            refreshed: Some(index),
        };
        self.control = Arc::default();
        self.search_control.cancel();
        self.search_receiver = None;
        self.cleanup = None;
        self.refresh_after_cleanup = false;
        self.rebuild = true;
        let control = Arc::clone(&self.control);
        let options = self.scan_options();
        self.job(JobKind::Refresh, context, move || {
            JobResult::Scan(
                scanner::refresh_subtree(&snapshot, index, &options, &control).and_then(|fresh| {
                    let mut prepared = prepare_snapshot(fresh, options.threads, &control);
                    if control.is_cancelled() {
                        return Err(tr(
                            "Folder recalculation cancelled. Previous results are preserved.",
                        )
                        .into());
                    }
                    view.remap(&snapshot, &prepared.snapshot, &control)?;
                    prepared.refresh_view = Some(Box::new(view));
                    Ok(prepared)
                }),
            )
        });
    }

    fn load(&mut self, path: PathBuf, context: &egui::Context) {
        self.cleanup = None;
        self.refresh_after_cleanup = false;
        self.last_refreshed = None;
        self.search_control.cancel();
        self.search_receiver = None;
        self.control = Arc::default();
        let control = Arc::clone(&self.control);
        let threads = self.settings.threads;
        self.job(JobKind::Load, context, move || {
            JobResult::Load(
                report::load_snapshot(&path)
                    .map(|snapshot| prepare_snapshot(snapshot, threads, &control)),
            )
        });
    }

    fn poll(&mut self, context: &egui::Context) {
        self.map_worker.poll();
        if self.receiver.is_some() {
            let marker = (
                self.control.visited.load(Ordering::Relaxed),
                self.control.bytes.load(Ordering::Relaxed),
                self.control.work_done.load(Ordering::Relaxed),
                self.control.phase() as u8,
            );
            if marker != self.progress_marker {
                self.progress_marker = marker;
                self.progress_changed = Instant::now();
            }
        }
        let result = self.receiver.as_ref().map(Receiver::try_recv);
        if let Some(Ok(result)) = result {
            self.receiver = None;
            let job_kind = self.job_kind.take();
            match result {
                JobResult::Scan(result) | JobResult::Load(result) => {
                    let result =
                        if job_kind == Some(JobKind::Refresh) && self.control.is_cancelled() {
                            Err(tr(
                                "Folder recalculation cancelled. Previous results are preserved.",
                            )
                            .into())
                        } else {
                            result
                        };
                    match result {
                        Ok(prepared) => {
                            let snapshot = prepared.snapshot;
                            self.cleanup = None;
                            self.live = matches!(job_kind, Some(JobKind::Scan | JobKind::Refresh));
                            if !self.live {
                                self.watching = false;
                                self.watcher = None;
                                self.watch_paths = None;
                                self.pending_watch_paths.clear();
                                self.watch_full_scan = false;
                                self.last_change = None;
                            }
                            self.roots = snapshot
                                .roots
                                .iter()
                                .map(|&index| snapshot.nodes[index].path.clone())
                                .collect();
                            self.status = if snapshot.cancelled {
                                tr("Scan cancelled | partial results").into()
                            } else {
                                trf(
                                    "{files} files scanned in {seconds} s",
                                    &[
                                        ("files", snapshot.file_count().to_string()),
                                        (
                                            "seconds",
                                            format!("{:.2}", snapshot.elapsed_ms as f64 / 1000.0),
                                        ),
                                    ],
                                )
                            };
                            if !self.live {
                                self.status.push_str(" | ");
                                self.status.push_str(tr("Read-only snapshot"));
                            }
                            self.expanded = snapshot.roots.iter().copied().collect();
                            self.extensions = prepared.extensions;
                            self.largest = prepared.largest;
                            self.analysis_complete = prepared.analysis_complete;
                            if let Some(error) = prepared.error {
                                self.error = Some(error);
                            }
                            let timestamp = now();
                            (self.age_sizes, self.age_files) = prepared.age;
                            self.age_day = timestamp / 86_400;
                            self.selected = None;
                            self.selection.clear();
                            self.scope = None;
                            self.map_viewport = MapViewport::default();
                            self.map_focus = None;
                            self.map_worker.clear();
                            self.file_colors =
                                Arc::new(FileColors::from_extensions(&self.extensions[0]));
                            self.map_worker.set_colors(Arc::clone(&self.file_colors));
                            self.duplicates = None;
                            self.duplicate_rows.clear();
                            self.duplicate_reclaimable = 0;
                            self.snapshot = Some(snapshot);
                            self.search_index = prepared.index;
                            self.search_control.cancel();
                            self.search_receiver = None;
                            self.rows.clear();
                            self.search_query.scope = None;
                            self.permission_details = None;
                            self.reveal_row = None;
                            if job_kind != Some(JobKind::Refresh) {
                                self.last_refreshed = None;
                            }
                            if let Some(view) = prepared.refresh_view {
                                self.selected = view.selected;
                                for index in view.selection {
                                    self.selection.click(index, &[], true, false);
                                }
                                if let Some(primary) = view.primary {
                                    self.selection.right_click(primary);
                                }
                                self.scope = view.scope;
                                self.search_query.scope = view.search_scope;
                                self.map_focus = view.map_focus;
                                self.expanded = view.expanded;
                                self.last_refreshed = view.refreshed.and_then(|index| {
                                    self.snapshot
                                        .as_ref()?
                                        .nodes
                                        .get(index)
                                        .map(|node| (node.path.clone(), index))
                                });
                            }
                            self.rebuild = true;
                            if self.watching && job_kind != Some(JobKind::Refresh) {
                                self.install_watcher(context);
                            }
                        }
                        Err(error) => {
                            self.rebuild |= job_kind == Some(JobKind::Refresh);
                            if self.refresh_after_cleanup {
                                self.status =
                                    tr("Item moved to trash. Scan results are outdated.").into();
                                self.error = Some(trf(
                                    "The item was moved to trash, but folder recalculation did not finish: {error}",
                                    &[("error", error)],
                                ));
                            } else {
                                self.error = Some(error);
                            }
                        }
                    }
                    self.refresh_after_cleanup = false;
                }
                JobResult::Duplicates(report) => {
                    self.status = trf(
                        "{groups} duplicate groups | {errors} read errors",
                        &[
                            ("groups", report.groups.len().to_string()),
                            ("errors", report.errors.len().to_string()),
                        ],
                    );
                    if report.cancelled {
                        self.status.push_str(" | ");
                        self.status.push_str(tr("Cancelled"));
                    }
                    (self.duplicate_rows, self.duplicate_reclaimable) = duplicate_summary(&report);
                    self.duplicates = Some(Arc::new(report));
                }
                JobResult::Io(result) => match result {
                    Ok(message) => {
                        self.status = message;
                    }
                    Err(error) => self.error = Some(error),
                },
                JobResult::Cleanup(result) => match result {
                    Ok(parent) => {
                        self.status =
                            tr("Item moved to trash. Recalculating its parent folder.").into();
                        self.refresh_folder(parent, context);
                        self.refresh_after_cleanup = self.job_kind == Some(JobKind::Refresh);
                        if !self.refresh_after_cleanup {
                            self.status =
                                tr("Item moved to trash. Scan results are outdated.").into();
                        }
                    }
                    Err(error) => self.error = Some(error),
                },
                JobResult::Permissions(path, result) => match result {
                    Ok(details) => {
                        self.permission_details = Some((path, details));
                    }
                    Err(error) => self.error = Some(error),
                },
            }
        } else if let Some(Err(mpsc::TryRecvError::Disconnected)) = result {
            self.receiver = None;
            self.rebuild |= self.job_kind.take() == Some(JobKind::Refresh);
            if self.refresh_after_cleanup {
                self.status = tr("Item moved to trash. Scan results are outdated.").into();
                self.error = Some(tr("Folder recalculation stopped unexpectedly. The item was already moved to trash; scan results are outdated.").into());
                self.refresh_after_cleanup = false;
            } else {
                self.error = Some(tr("The background job stopped unexpectedly. Your files were not automatically changed.").into());
            }
        }
        if let Some(result) = self.search_receiver.as_ref().map(Receiver::try_recv) {
            match result {
                Ok(result) => {
                    self.search_receiver = None;
                    if result.is_current(self.search_generation, &self.search_control) {
                        self.search_index = result.index;
                        match result.rows {
                            Ok(rows) => {
                                self.rows = rows;
                                self.filter_error = None;
                            }
                            Err(error) if !self.search_control.is_cancelled() => {
                                self.rows.clear();
                                self.filter_error = Some(error);
                            }
                            Err(_) => {}
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.search_receiver = None;
                    self.filter_error = Some(tr("Search stopped unexpectedly").into());
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        let mut watch_activity = self.changed.swap(false, Ordering::Relaxed);
        self.watch_full_scan |= watch_activity;
        if let Some((_, receiver)) = &self.watch_paths {
            for path in receiver.try_iter().take(WATCH_PATH_LIMIT) {
                watch_activity = true;
                if !self.watch_full_scan {
                    self.pending_watch_paths.insert(path);
                    if self.pending_watch_paths.len() > WATCH_PATH_LIMIT {
                        self.pending_watch_paths.clear();
                        self.watch_full_scan = true;
                    }
                }
            }
        }
        if watch_activity {
            self.last_change = Some(Instant::now());
        }
        if let Some(Ok(error)) = self.watch_errors.as_ref().map(Receiver::try_recv) {
            self.error = Some(trf(
                "Filesystem watcher stopped: {error}",
                &[("error", error)],
            ));
            self.watching = false;
            self.watcher = None;
            self.watch_errors = None;
        }
        if self.watching
            && self.live
            && self.receiver.is_none()
            && self
                .last_change
                .is_some_and(|instant| instant.elapsed() > Duration::from_secs(2))
        {
            self.last_change = None;
            let target = self
                .last_refreshed
                .as_ref()
                .filter(|(folder, index)| {
                    !self.watch_full_scan
                        && !self.pending_watch_paths.is_empty()
                        && self
                            .snapshot
                            .as_ref()
                            .and_then(|snapshot| snapshot.nodes.get(*index))
                            .is_some_and(|node| {
                                node.kind == EntryKind::Directory && node.path == *folder
                            })
                        && self.pending_watch_paths.iter().all(|path| {
                            path.is_absolute()
                                && path.starts_with(folder)
                                && !path
                                    .components()
                                    .any(|component| component == std::path::Component::ParentDir)
                        })
                })
                .map(|(_, index)| *index);
            self.pending_watch_paths.clear();
            self.watch_full_scan = false;
            if let Some(index) = target {
                self.refresh_folder(index, context);
            } else {
                self.start_scan(context);
            }
        }
        if self.receiver.is_some()
            || self.search_receiver.is_some()
            || self.map_worker.pending()
            || (self.watching && self.last_change.is_some())
        {
            context.request_repaint_after(Duration::from_millis(150));
        }
    }

    fn install_watcher(&mut self, context: &egui::Context) {
        self.watcher = None;
        self.watch_errors = None;
        if !self.watching || !self.live {
            self.watch_paths = None;
            self.pending_watch_paths.clear();
            self.watch_full_scan = false;
            self.last_change = None;
            return;
        }
        let (path_sender, _) = self
            .watch_paths
            .get_or_insert_with(|| mpsc::sync_channel(WATCH_PATH_LIMIT));
        let path_sender = path_sender.clone();
        let changed = Arc::clone(&self.changed);
        let context = context.clone();
        let (error_sender, error_receiver) = mpsc::channel();
        self.watch_errors = Some(error_receiver);
        let result =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| match event {
                Ok(event) if !matches!(event.kind, notify::EventKind::Access(_)) => {
                    queue_watch_paths(event, &path_sender, &changed);
                    context.request_repaint();
                }
                Err(error) => {
                    let _ = error_sender.send(error.to_string());
                    context.request_repaint();
                }
                _ => {}
            });
        match result {
            Ok(mut watcher) => {
                for root in &self.roots {
                    if let Err(error) = watcher.watch(root, RecursiveMode::Recursive) {
                        self.error = Some(trf(
                            "Could not watch {path}: {error}",
                            &[("path", display_path(root)), ("error", error.to_string())],
                        ));
                        self.watching = false;
                        return;
                    }
                }
                self.watcher = Some(watcher);
            }
            Err(error) => {
                self.error = Some(error.to_string());
                self.watching = false;
            }
        }
    }

    fn sidebar(&mut self, context: &egui::Context) {
        if context.content_rect().width() < 1100.0 || context.content_rect().height() < 720.0 {
            egui::TopBottomPanel::top("compact-navigation")
                .frame(
                    egui::Frame::new()
                        .fill(context.style().visuals.panel_fill)
                        .inner_margin(12),
                )
                .show(context, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        egui::ComboBox::new("compact-page", tr("View"))
                            .selected_text(self.page.title())
                            .width(150.0)
                            .show_ui(ui, |ui| {
                                for page in [
                                    Page::Overview,
                                    Page::Files,
                                    Page::Largest,
                                    Page::Types,
                                    Page::Duplicates,
                                    Page::Storage,
                                    Page::Issues,
                                ] {
                                    ui.selectable_value(&mut self.page, page, page.title());
                                }
                            });
                        if ui.button(tr("Settings")).clicked() {
                            self.page = Page::Settings;
                        }
                        if context.content_rect().height() < 500.0 {
                            ui.menu_button(tr("Actions"), |ui| self.header_actions(ui));
                        }
                    });
                    self.appearance_controls(ui, true);
                });
            return;
        }
        egui::SidePanel::left("sidebar")
            .exact_width(224.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(if context.style().visuals.dark_mode {
                        Color32::from_rgb(21, 26, 38)
                    } else {
                        Color32::WHITE
                    })
                    .inner_margin(18),
            )
            .show(context, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let (icon, _) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::hover());
                    crate::icon::paint(ui.painter(), icon);
                    ui.label(RichText::new("UniDirStat").size(21.0).strong());
                });
                ui.add_space(12.0);
                ui.label(
                    RichText::new(tr("WORKSPACE"))
                        .size(HELP_SIZE)
                        .color(muted(ui))
                        .strong(),
                );
                for page in [
                    Page::Overview,
                    Page::Files,
                    Page::Largest,
                    Page::Types,
                    Page::Duplicates,
                    Page::Storage,
                    Page::Issues,
                ] {
                    let selected = self.page == page;
                    let blend = context.animate_bool_with_time(
                        ui.id().with(("navigation", page as u8)),
                        selected,
                        if self.settings.reduced_motion {
                            0.0
                        } else {
                            0.16
                        },
                    );
                    let text = RichText::new(page.title()).color(ui.visuals().text_color());
                    if ui
                        .add_sized(
                            [188.0, CONTROL_HEIGHT],
                            egui::Button::new(text)
                                .selected(selected)
                                .frame(selected)
                                .fill(ui.visuals().selection.bg_fill.linear_multiply(blend)),
                        )
                        .clicked()
                    {
                        self.page = page;
                        self.rebuild |= page_needs_rows(page, self.rows_largest);
                    }
                }
                let controls = egui::Rect::from_min_max(
                    egui::pos2(
                        ui.max_rect().left(),
                        (ui.max_rect().bottom() - 204.0).max(ui.cursor().top()),
                    ),
                    ui.max_rect().max,
                );
                let mut controls_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(controls)
                        .layout(Layout::top_down(Align::LEFT)),
                );
                if self.snapshot.is_some() {
                    controls_ui.label(
                        RichText::new(if self.live {
                            tr("Live filesystem")
                        } else {
                            tr("Saved snapshot")
                        })
                        .color(muted(&controls_ui))
                        .size(HELP_SIZE),
                    );
                }
                self.appearance_controls(&mut controls_ui, false);
                controls_ui.separator();
                if controls_ui
                    .add_sized(
                        [188.0, CONTROL_HEIGHT],
                        egui::Button::new(tr("Settings")).selected(self.page == Page::Settings),
                    )
                    .clicked()
                {
                    self.page = Page::Settings;
                }
                controls_ui.label(
                    RichText::new(trf(
                        "UniDirStat {version} | Rust",
                        &[("version", env!("CARGO_PKG_VERSION").to_owned())],
                    ))
                    .size(HELP_SIZE)
                    .color(muted(&controls_ui)),
                );
            });
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        let compact_header = ui.ctx().content_rect().height() < 500.0;
        if !compact_header && ui.available_width() < 900.0 {
            ui.heading(self.page.title());
            ui.label(RichText::new(self.page.subtitle()).color(muted(ui)));
            ui.horizontal_wrapped(|ui| self.header_actions(ui));
        } else if !compact_header {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.heading(self.page.title());
                    ui.label(RichText::new(self.page.subtitle()).color(muted(ui)));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    self.header_actions(ui)
                });
            });
        }
        ui.add_space(if compact_header { 4.0 } else { 12.0 });
        if self.receiver.is_some() {
            self.job_card(ui);
        }
        if let Some(error) = self.error.clone() {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    trf("Error: {details}", &[("details", error)]),
                );
                ui.label(tr(
                    "Try again, choose another folder, or check file access.",
                ));
                if ui.button(tr("Dismiss")).clicked() {
                    self.error = None;
                }
            });
        }
    }

    fn header_actions(&mut self, ui: &mut egui::Ui) {
        if ui
            .add_enabled(
                self.receiver.is_none(),
                egui::Button::new(RichText::new(tr("Add folder")).color(Color32::WHITE))
                    .fill(PRIMARY_BUTTON),
            )
            .clicked()
            && let Some(paths) = rfd::FileDialog::new()
                .set_title(tr("Choose folders to scan"))
                .pick_folders()
        {
            self.roots.extend(paths);
            self.start_scan(ui.ctx());
        }
        if ui
            .add_enabled(
                self.receiver.is_none() && self.snapshot.is_some(),
                egui::Button::new(tr("Refresh")),
            )
            .clicked()
        {
            self.start_scan(ui.ctx());
        }
        ui.menu_button(tr("Reports"), |ui| {
            let idle = self.receiver.is_none();
            if ui
                .add_enabled(idle, egui::Button::new(tr("Open snapshot...")))
                .clicked()
            {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter(tr("UniDirStat snapshot"), &["json", "csv"])
                    .pick_file()
                {
                    self.load(path, ui.ctx());
                }
                ui.close();
            }
            for (label, extension) in [
                (tr("Save JSON snapshot..."), "json"),
                (tr("Export CSV report..."), "csv"),
            ] {
                if ui
                    .add_enabled(idle && self.snapshot.is_some(), egui::Button::new(label))
                    .clicked()
                {
                    if let Some(path) = rfd::FileDialog::new()
                        .set_file_name(format!("unidirstat.{extension}"))
                        .add_filter(tr("Report"), &[extension])
                        .save_file()
                        && let Some(snapshot) = self.snapshot.as_ref().map(Arc::clone)
                    {
                        self.job(JobKind::SaveReport, ui.ctx(), move || {
                            JobResult::Io(
                                (if extension == "json" {
                                    report::save_snapshot(&path, &snapshot)
                                } else {
                                    report::export_csv(&path, &snapshot)
                                })
                                .map(|()| trf("Saved {path}", &[("path", display_path(&path))])),
                            )
                        });
                    }
                    ui.close();
                }
            }
            if ui
                .add_enabled(
                    idle && self.snapshot.is_some(),
                    egui::Button::new(tr("Export permission metadata...")),
                )
                .clicked()
            {
                if let Some(path) = rfd::FileDialog::new()
                    .set_file_name("permissions.csv")
                    .add_filter(tr("Permission metadata"), &["csv", "json"])
                    .save_file()
                    && let Some(snapshot) = self.snapshot.as_ref().map(Arc::clone)
                {
                    self.job(JobKind::SaveReport, ui.ctx(), move || {
                        JobResult::Io(
                            report::export_permissions(&path, &snapshot)
                                .map(|()| trf("Saved {path}", &[("path", display_path(&path))])),
                        )
                    });
                }
                ui.close();
            }
        });
    }

    fn appearance_controls(&mut self, ui: &mut egui::Ui, compact: bool) {
        let previous = self.settings.language;
        let layout = if compact {
            Layout::left_to_right(Align::Center).with_main_wrap(true)
        } else {
            Layout::top_down(Align::LEFT)
        };
        ui.with_layout(layout, |ui| {
            ui.horizontal(|ui| {
                language_icon(ui);
                let language = match self.settings.language {
                    Language::System => tr("System language"),
                    Language::English => "English",
                    Language::Russian => "Русский",
                };
                let response = egui::ComboBox::from_id_salt("sidebar-language")
                    .width(146.0)
                    .selected_text(language)
                    .show_ui(ui, |ui| {
                        for (value, label) in [
                            (Language::System, tr("System language")),
                            (Language::English, "English"),
                            (Language::Russian, "Русский"),
                        ] {
                            ui.selectable_value(&mut self.settings.language, value, label);
                        }
                    })
                    .response;
                response.widget_info(|| {
                    let mut info =
                        egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, true, tr("Language"));
                    info.current_text_value = Some(language.into());
                    info
                });
            });
            if previous != self.settings.language {
                set_language(self.settings.language);
                self.status = tr("Language updated").into();
            }
            let theme = match self.settings.theme {
                Theme::System => tr("System"),
                Theme::Light => tr("Light"),
                Theme::Dark => tr("Dark"),
            };
            let response = egui::ComboBox::from_id_salt("sidebar-theme")
                .width(184.0)
                .selected_text(theme)
                .show_ui(ui, |ui| {
                    for (value, label) in [
                        (Theme::System, tr("System")),
                        (Theme::Light, tr("Light")),
                        (Theme::Dark, tr("Dark")),
                    ] {
                        ui.selectable_value(&mut self.settings.theme, value, label);
                    }
                })
                .response;
            response.widget_info(|| {
                let mut info =
                    egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, true, tr("Theme"));
                info.current_text_value = Some(theme.into());
                info
            });
        });
    }

    fn job_card(&mut self, ui: &mut egui::Ui) {
        let kind = self.job_kind;
        let state = job_state(&self.control);
        card(ui).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    if state == JobState::Running && !self.settings.reduced_motion { ui.spinner(); }
                    ui.strong(kind.map_or(tr("Working"), JobKind::label));
                    if kind.is_some_and(JobKind::cancellable) {
                        if ui
                            .add_enabled(
                                state != JobState::Cancelling,
                                egui::Button::new(tr("Cancel")),
                            )
                            .clicked()
                        {
                            self.control.cancel();
                        }
                        if ui
                            .add_enabled(
                                state != JobState::Cancelling,
                                egui::Button::new(if state == JobState::Paused {
                                    tr("Resume")
                                } else {
                                    tr("Pause")
                                }),
                            )
                            .clicked()
                        {
                            if state == JobState::Paused {
                                self.control.resume();
                            } else {
                                self.control.pause();
                            }
                        }
                        match state {
                            JobState::Cancelling => {
                                ui.label(tr("Cancelling..."));
                            }
                            JobState::Paused => {
                                ui.label(tr(
                                    "Pause requested; waiting for current filesystem operations.",
                                ));
                            }
                            JobState::Running => {}
                        }
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    if matches!(kind, Some(JobKind::Scan | JobKind::Refresh | JobKind::Duplicates)) { ui.label(job_progress(
                        &self.control,
                        self.job_started.elapsed().as_secs_f64(),
                        matches!(kind, Some(JobKind::Scan | JobKind::Refresh)),
                    )); } else { ui.label(trf("Elapsed: {seconds} s", &[("seconds", format!("{:.1}", self.job_started.elapsed().as_secs_f64()))])); }
                });
                if kind.is_some_and(JobKind::reports_work) {
                    show_work_progress(ui, &self.control);
                }
                if matches!(kind, Some(JobKind::Scan | JobKind::Refresh)) && self.control.phase() == ScanPhase::Discovering {
                    if let Some(path) = self.control.current_path() {
                        ui.add(egui::Label::new(trf("Last directory: {path}", &[("path", display_path(&path))])).truncate()).on_hover_text(display_path(&path));
                    }
                    if state == JobState::Running && self.progress_changed.elapsed() >= Duration::from_secs(3) {
                        ui.label(trf("No new results for {seconds} s. A filesystem operation or permission dialog may be waiting.", &[("seconds", self.progress_changed.elapsed().as_secs().to_string())]));
                    }
                }
            });
    }
}

fn card(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::new()
        .fill(ui.visuals().window_fill)
        .corner_radius(12)
        .inner_margin(18)
        .stroke(Stroke::new(
            1.0_f32,
            if ui.visuals().dark_mode {
                Color32::from_rgb(39, 46, 63)
            } else {
                Color32::from_rgb(226, 231, 240)
            },
        ))
}

fn color_dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(8.0, 16.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 3.0, color);
}

fn job_progress(control: &ScanControl, seconds: f64, scanning: bool) -> String {
    let entries = control.visited.load(Ordering::Relaxed).to_string();
    let bytes = format_bytes(control.bytes.load(Ordering::Relaxed));
    let seconds = format!("{seconds:.1}");
    if scanning {
        return trf(
            "{entries} entries | logical {logical} | allocated {allocated} | {seconds} s",
            &[
                ("entries", entries),
                ("logical", bytes),
                (
                    "allocated",
                    if control.unknown_allocations.load(Ordering::Relaxed) == 0 {
                        format_bytes(control.allocated_bytes.load(Ordering::Relaxed))
                    } else {
                        tr("Unavailable").into()
                    },
                ),
                ("seconds", seconds),
            ],
        );
    }
    trf(
        "{entries} entries | {bytes} | {seconds} s",
        &[("entries", entries), ("bytes", bytes), ("seconds", seconds)],
    )
}

fn format_bytes(bytes: u64) -> String {
    let units = [
        tr("B"),
        tr("KiB"),
        tr("MiB"),
        tr("GiB"),
        tr("TiB"),
        tr("PiB"),
        tr("EiB"),
    ];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < units.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", units[0])
    } else {
        format!("{value:.2} {}", units[unit])
    }
}

fn tile_text_color(background: Color32) -> Color32 {
    let linear = |channel: u8| {
        let channel = f32::from(channel) / 255.0;
        if channel <= 0.04045 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    };
    let luminance = linear(background.r()) * 0.2126
        + linear(background.g()) * 0.7152
        + linear(background.b()) * 0.0722;
    if luminance > 0.179 {
        Color32::BLACK
    } else {
        Color32::WHITE
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

impl UniDirStatApp {
    fn welcome(&mut self, ui: &mut egui::Ui) {
        ui.add_space(28.0);
        card(ui).show(ui, |ui| {
            ui.add_space(10.0);
            ui.label(
                RichText::new(tr("Make room for what matters."))
                    .size(31.0)
                    .strong(),
            );
            ui.label(
                RichText::new(tr(
                    "Choose a folder or drive. See every file in a whole new light.",
                ))
                .color(muted(ui)),
            );
            ui.add_space(20.0);
            let path_label = ui.label(tr("Folder path"));
            let input_width = if ui.available_width() < 460.0 {
                ui.available_width()
            } else {
                ui.available_width() - 180.0
            };
            ui.horizontal_wrapped(|ui| {
                let response = ui
                    .add_sized(
                        [input_width.max(1.0), CONTROL_HEIGHT],
                        egui::TextEdit::singleline(&mut self.path_input)
                            .margin(INPUT_MARGIN)
                            .min_size(Vec2::new(0.0, CONTROL_HEIGHT))
                            .hint_text(tr("Enter a folder path...")),
                    )
                    .labelled_by(path_label.id);
                let explore = ui
                    .add_sized(
                        [160.0, CONTROL_HEIGHT],
                        egui::Button::new(
                            RichText::new(tr("Explore folder")).color(Color32::WHITE),
                        )
                        .fill(PRIMARY_BUTTON),
                    )
                    .clicked()
                    || (response.lost_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter)));
                if explore && !self.path_input.trim().is_empty() {
                    self.roots = vec![PathBuf::from(self.path_input.trim())];
                    self.start_scan(ui.ctx());
                }
            });
            ui.add_space(10.0);
            ui.label(
                RichText::new(tr("Or use Add folder to select several locations at once."))
                    .small()
                    .color(muted(ui)),
            );
        });
        ui.add_space(20.0);
        ui.label(
            RichText::new(tr("YOUR VOLUMES"))
                .small()
                .color(muted(ui))
                .strong(),
        );
        for index in 0..self.volumes.len() {
            let Volume {
                path,
                name,
                total,
                available,
                system_data_group,
            } = self.volumes[index].clone();
            if total == 0 {
                continue;
            }
            card(ui).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.strong(if name.is_empty() {
                            display_path(&path)
                        } else {
                            name
                        });
                        ui.label(RichText::new(display_path(&path)).small().color(muted(ui)));
                        if system_data_group {
                            ui.label(
                                RichText::new(tr("macOS System + Data volume group"))
                                    .small()
                                    .color(muted(ui)),
                            );
                        }
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add_enabled(self.receiver.is_none(), egui::Button::new(tr("Explore")))
                            .clicked()
                        {
                            self.roots = vec![path];
                            self.start_scan(ui.ctx());
                        }
                        ui.label(trf(
                            "{free} free of {total}",
                            &[
                                ("free", format_bytes(available)),
                                ("total", format_bytes(total)),
                            ],
                        ));
                    });
                });
                ui.add(
                    egui::ProgressBar::new(1.0 - available as f32 / total as f32)
                        .fill(ACCENT)
                        .desired_height(5.0),
                );
            });
        }
    }

    fn overview(&mut self, ui: &mut egui::Ui, snapshot: &Arc<Snapshot>) {
        self.workspace(ui, snapshot);
    }

    fn map(&mut self, ui: &mut egui::Ui, snapshot: &Arc<Snapshot>, height: f32) {
        self.map_canvas(ui, snapshot, height);
    }

    fn filters(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            let search = ui
                .add_sized(
                    [ui.available_width().min(360.0), CONTROL_HEIGHT],
                    egui::TextEdit::singleline(&mut self.search_query.text)
                        .margin(INPUT_MARGIN)
                        .min_size(Vec2::new(0.0, CONTROL_HEIGHT))
                        .hint_text(tr("Search files and folders...")),
                );
            search.widget_info(|| {
                let mut info = egui::WidgetInfo::labeled(
                    egui::WidgetType::TextEdit, ui.is_enabled(), tr("Search files and folders"),
                );
                info.current_text_value = Some(self.search_query.text.clone());
                info
            });
            changed |= search.changed();
            changed |= ui
                .checkbox(&mut self.search_query.regex, tr("Regex"))
                .changed();
            let mut name_only = self.search_query.target == SearchTarget::Name;
            if ui.checkbox(&mut name_only, tr("Name only")).changed() {
                self.search_query.target = if name_only {
                    SearchTarget::Name
                } else {
                    SearchTarget::Path
                };
                changed = true;
            }
            if ui.button(tr("Clear")).clicked() {
                self.search_query = SearchQuery::default();
                changed = true;
            }
            ui.menu_button(tr("Advanced filters"), |ui| {
                let viewport = ui.ctx().content_rect().size();
                ui.set_width((viewport.x - 48.0).clamp(160.0, 640.0));
                egui::ScrollArea::vertical()
                    .max_height((viewport.y - 80.0).max(40.0))
                    .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (label, field, unit) in [
                    (
                        tr("Min MiB"),
                        &mut self.search_query.min_size,
                        1_048_576_u64,
                    ),
                    (
                        tr("Max MiB"),
                        &mut self.search_query.max_size,
                        1_048_576_u64,
                    ),
                    (
                        tr("Older than days"),
                        &mut self.search_query.min_age_seconds,
                        86_400,
                    ),
                    (
                        tr("Younger than days"),
                        &mut self.search_query.max_age_seconds,
                        86_400,
                    ),
                ] {
                    let mut enabled = field.is_some();
                    let field_label = ui.checkbox(&mut enabled, label);
                    if field_label.changed() {
                        *field = enabled.then_some(0);
                        changed = true;
                    }
                    if let Some(value) = field {
                        let mut display = *value as f64 / unit as f64;
                        if ui
                            .add(
                                egui::DragValue::new(&mut display)
                                    .range(0.0..=1e12)
                                    .speed(1.0),
                            )
                            .labelled_by(field_label.id)
                            .changed()
                        {
                            *value = (display * unit as f64) as u64;
                            changed = true;
                        }
                    }
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.label(tr("Kind"));
                for kind in [
                    EntryKind::File,
                    EntryKind::Directory,
                    EntryKind::Symlink,
                    EntryKind::Other,
                ] {
                    let mut enabled = self.search_query.kinds.contains(&kind);
                    if ui.checkbox(&mut enabled, kind_label(kind)).changed() {
                        self.search_query.kinds.retain(|entry| *entry != kind);
                        if enabled {
                            self.search_query.kinds.push(kind);
                        }
                        changed = true;
                    }
                }
                let mut owner_enabled = self.search_query.owner.is_some();
                let owner_label = ui.add_enabled(
                        cfg!(unix),
                        egui::Checkbox::new(&mut owner_enabled, tr("Owner UID")),
                    );
                if owner_label.changed() {
                    self.search_query.owner = owner_enabled.then_some(0);
                    changed = true;
                }
                if let Some(owner) = &mut self.search_query.owner {
                    changed |= ui
                        .add(egui::DragValue::new(owner).range(0..=u32::MAX))
                        .labelled_by(owner_label.id)
                        .changed();
                }
                let mut extension_enabled = self.search_query.extension.is_some();
                let extension_label = ui.checkbox(&mut extension_enabled, tr("Extension"));
                if extension_label.changed() {
                    self.search_query.extension = extension_enabled.then(String::new);
                    changed = true;
                }
                if let Some(extension) = &mut self.search_query.extension {
                    changed |= ui
                        .add_sized([110.0, 24.0], egui::TextEdit::singleline(extension).margin(INPUT_MARGIN).min_size(Vec2::new(0.0, CONTROL_HEIGHT)))
                        .labelled_by(extension_label.id)
                        .changed();
                }
                if ui.button(tr("Empty folder candidates")).clicked() {
                    self.search_query = SearchQuery {
                        empty_directories: true,
                        ..SearchQuery::default()
                    };
                    changed = true;
                }
            });
            if self.search_query.empty_directories {
                ui.label(tr("Candidates are empty in the scan. Exclusions can hide contents; cleanup rechecks the live folder."));
            }
            ui.horizontal_wrapped(|ui| {
                let filter_label = ui.label(tr("Filter name"));
                ui.add_sized(
                    [140.0, 24.0],
                    egui::TextEdit::singleline(&mut self.filter_name).margin(INPUT_MARGIN).min_size(Vec2::new(0.0, CONTROL_HEIGHT)).hint_text(tr("Filter name")),
                ).labelled_by(filter_label.id);
                if ui.button(tr("Save filter")).clicked() {
                    let mut query = self.search_query.clone();
                    query.sort = match self.sort_by {
                        SortBy::Size => SearchSort::Size,
                        SortBy::Name => SearchSort::Name,
                        SortBy::Modified => SearchSort::Modified,
                    };
                    query.descending = self.descending;
                    if let Err(error) = save_filter(&mut self.settings, &self.filter_name, &query) {
                        self.filter_error = Some(error);
                    }
                }
                let mut remove = None;
                egui::ComboBox::from_id_salt("saved-filters")
                    .selected_text(tr("Saved filters"))
                    .show_ui(ui, |ui| {
                        for (index, saved) in self.settings.filters.iter().enumerate() {
                            ui.horizontal(|ui| {
                                if ui.selectable_label(false, &saved.name).clicked() {
                                    self.search_query = saved.query.clone();
                                    self.sort_by = match saved.query.sort {
                                        SearchSort::Size => SortBy::Size,
                                        SearchSort::Name => SortBy::Name,
                                        SearchSort::Modified => SortBy::Modified,
                                    };
                                    self.descending = saved.query.descending;
                                    self.filter_name = saved.name.clone();
                                    changed = true;
                                    ui.close();
                                }
                                if ui.button(tr("Remove")).clicked() {
                                    remove = Some(index);
                                }
                            });
                        }
                    });
                if let Some(index) = remove {
                    self.settings.filters.remove(index);
                }
            });
                    });
            });
            let target = self.recalculation_target();
            if ui.add_enabled(target.is_some(), egui::Button::new(tr("Recalculate folder"))).clicked()
                && let Some(index) = target
            {
                self.refresh_folder(index, ui.ctx());
            }
        });
        if changed {
            self.filter_error = None;
            self.rebuild = true;
        }
        if let Some(error) = &self.filter_error {
            ui.colored_label(
                ui.visuals().error_fg_color,
                trf("Search error: {error}", &[("error", error.to_owned())]),
            );
            ui.label(tr(
                "Edit the search, turn off Regex, or clear filters to try again.",
            ));
        }
    }

    fn rebuild_rows(&mut self, context: &egui::Context) {
        if !self.rebuild || self.receiver.is_some() {
            return;
        }
        let Some(snapshot) = self.snapshot.as_ref().map(Arc::clone) else {
            return;
        };
        self.rebuild = false;
        self.search_started = Instant::now();
        self.rows_largest = self.page == Page::Largest;
        self.search_control.cancel();
        self.search_control = Arc::default();
        self.search_generation = self.search_generation.wrapping_add(1);
        let generation = self.search_generation;
        let control = Arc::clone(&self.search_control);
        let mut query = self.search_query.clone();
        query.allocated = self.settings.allocated;
        query.sort = match self.sort_by {
            SortBy::Size => SearchSort::Size,
            SortBy::Name => SearchSort::Name,
            SortBy::Modified => SearchSort::Modified,
        };
        query.descending = self.descending;
        if self.page == Page::Largest {
            query.kinds = vec![EntryKind::File];
            query.scope = self.scope;
        }
        let flat = self.page == Page::Largest || has_filters(&query);
        let expanded = self.expanded.clone();
        let index = self.search_index.as_ref().map(Arc::clone);
        let threads = self.settings.threads;
        let (sender, receiver) = mpsc::channel();
        self.search_receiver = Some(receiver);
        let context = context.clone();
        std::thread::spawn(move || {
            let index = index.map(Ok).unwrap_or_else(|| {
                SearchIndex::with_control(Arc::clone(&snapshot), threads, &control).map(Arc::new)
            });
            let result = match index {
                Ok(index) => {
                    let rows = build_rows(&snapshot, &index, &query, flat, &expanded, &control);
                    RowsReady {
                        generation,
                        index: Some(index),
                        rows,
                    }
                }
                Err(error) => RowsReady {
                    generation,
                    index: None,
                    rows: Err(error),
                },
            };
            let _ = sender.send(result);
            context.request_repaint();
        });
    }

    fn files(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        self.file_controls(ui);
        self.file_table(ui, snapshot);
    }

    fn types(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        if !self.analysis_complete {
            ui.label(tr(
                "Analysis was cancelled. Refresh to calculate all statistics.",
            ));
            return;
        }
        let summary = self.map_worker.summary().map(Arc::clone);
        let extensions = if self.scope.is_some() {
            let Some(summary) = summary.as_ref() else {
                if !self.settings.reduced_motion {
                    ui.spinner();
                }
                ui.label(tr("Calculating statistics"));
                return;
            };
            if self.settings.allocated {
                &summary.extensions_allocated
            } else {
                &summary.extensions_logical
            }
        } else {
            &self.extensions[usize::from(self.settings.allocated)]
        };
        let total = self
            .scope
            .map_or_else(
                || snapshot.total_size(self.settings.allocated),
                |index| snapshot.nodes[index].size(self.settings.allocated),
            )
            .max(1) as f64;
        card(ui).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong(tr("Extension"));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(tr("Click a type to find its files"));
                });
            });
            ui.separator();
            egui::ScrollArea::vertical().id_salt("types").show_rows(
                ui,
                44.0,
                extensions.len(),
                |ui, range| {
                    for index in range {
                        let stats = &extensions[index];
                        ui.horizontal(|ui| {
                            color_dot(ui, self.file_colors.color(&stats.extension));
                            if ui
                                .add_sized(
                                    [140.0, 30.0],
                                    egui::Button::new(extension_label(&stats.extension))
                                        .frame(false),
                                )
                                .clicked()
                            {
                                self.search_query.extension = Some(stats.extension.clone());
                                self.search_query.scope = self.scope;
                                self.page = Page::Files;
                                self.compact_map = false;
                                self.summary_open = false;
                                self.rebuild = true;
                            }
                            ui.add_sized(
                                [90.0, 24.0],
                                egui::Label::new(trf(
                                    "{files} files",
                                    &[("files", stats.files.to_string())],
                                )),
                            );
                            ui.add_sized(
                                [100.0, 24.0],
                                egui::Label::new(RichText::new(format_bytes(stats.bytes)).strong()),
                            );
                            ui.add(
                                egui::ProgressBar::new((stats.bytes as f64 / total) as f32)
                                    .fill(self.file_colors.color(&stats.extension))
                                    .desired_width((ui.available_width() - 65.0).max(50.0))
                                    .desired_height(7.0),
                            );
                            ui.label(format!("{:.1}%", stats.bytes as f64 / total * 100.0));
                        });
                    }
                },
            );
        });
    }

    fn duplicates_page(&mut self, ui: &mut egui::Ui, snapshot: &Arc<Snapshot>) {
        card(ui).show(ui,|ui| {
            ui.label(tr("Exact full-file hashing; hard links are counted once. No files are removed automatically."));
            ui.horizontal_wrapped(|ui| {
                ui.label(tr("Hash algorithm"));
                egui::ComboBox::from_id_salt("hash-algorithm").selected_text(self.settings.hash_algorithm.as_str()).show_ui(ui, |ui| {
                    for algorithm in [HashAlgorithm::Sha256, HashAlgorithm::Sha512] { ui.selectable_value(&mut self.settings.hash_algorithm, algorithm, algorithm.as_str()); }
                });
                if ui.add_enabled(self.receiver.is_none() && self.live,egui::Button::new(tr("Find duplicate files")).fill(ACCENT)).clicked() {
                    self.control=Arc::default(); let control=Arc::clone(&self.control); let snapshot=Arc::clone(snapshot);
                    let options = analysis::DuplicateOptions { algorithm: self.settings.hash_algorithm, threads: self.settings.threads };
                    self.job(JobKind::Duplicates,ui.ctx(),move || JobResult::Duplicates(analysis::find_duplicates_with_options(&snapshot,&options,&control)));
                }
                if !self.live { ui.label(RichText::new(tr("Refresh this snapshot to inspect files on this device.")).small().color(muted(ui))); }
                if ui.add_enabled(self.receiver.is_none() && self.duplicates.is_some(),egui::Button::new(tr("Export duplicates..."))).clicked()
                    && let Some(path)=rfd::FileDialog::new().set_file_name("duplicates.json").add_filter(tr("Report"), &["json", "csv"]).save_file()
                    && let Some(report)=self.duplicates.as_ref().map(Arc::clone) {
                    let snapshot = Arc::clone(snapshot);
                    self.job(JobKind::SaveDuplicates,ui.ctx(),move || JobResult::Io((if path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("csv")) { report::export_duplicates_csv(&path,&report,&snapshot) } else { report::save_duplicates(&path,&report) }).map(|()|trf("Saved {path}",&[("path",display_path(&path))]))));
                }
            });
        });
        if let Some(report) = self.duplicates.as_ref().map(Arc::clone) {
            ui.label(
                RichText::new(trf(
                    "{groups} groups | up to {bytes} redundant logical bytes",
                    &[
                        ("groups", report.groups.len().to_string()),
                        ("bytes", format_bytes(self.duplicate_reclaimable)),
                    ],
                ))
                .size(18.0)
                .strong(),
            );
            egui::ScrollArea::vertical()
                .id_salt("duplicates")
                .auto_shrink([false, false])
                .show_rows(ui, 32.0, self.duplicate_rows.len(), |ui, range| {
                    for row in range {
                        let width = ui.available_width();
                        match self.duplicate_rows[row] {
                            DuplicateRow::Group(index) => {
                                let group = &report.groups[index];
                                ui.add_sized(
                                    [width, 32.0],
                                    egui::Label::new(
                                        RichText::new(trf(
                                            "{copies} copies | {size} each",
                                            &[
                                                ("copies", group.nodes.len().to_string()),
                                                ("size", format_bytes(group.size)),
                                            ],
                                        ))
                                        .strong(),
                                    )
                                    .truncate(),
                                );
                            }
                            DuplicateRow::Hash(index) => {
                                let hash = duplicate_hash_label(
                                    report.algorithm,
                                    &report.groups[index].hash,
                                );
                                ui.add_sized(
                                    [width, 32.0],
                                    egui::Label::new(
                                        RichText::new(&hash).small().monospace().color(muted(ui)),
                                    )
                                    .truncate(),
                                )
                                .on_hover_text(hash);
                            }
                            DuplicateRow::File(index) => {
                                let path = display_path(&snapshot.nodes[index].path);
                                let response = ui
                                    .add_sized(
                                        [width, 32.0],
                                        egui::Button::new(&path)
                                            .selected(self.selection.contains(index))
                                            .frame(false)
                                            .truncate(),
                                    )
                                    .on_hover_text(path);
                                self.item_response(
                                    response,
                                    index,
                                    snapshot,
                                    SelectionView::Duplicates,
                                );
                            }
                        }
                    }
                });
            for error in report.errors.iter().take(20) {
                ui.colored_label(ui.visuals().warn_fg_color, error);
            }
        }
    }

    fn storage(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        if !self.analysis_complete {
            ui.label(tr(
                "Analysis was cancelled. Refresh to calculate all statistics.",
            ));
            return;
        }
        let timestamp = now();
        if self.age_day != timestamp / 86_400 {
            (self.age_sizes, self.age_files) = storage_buckets(snapshot, timestamp);
            self.age_day = timestamp / 86_400;
        }
        card(ui).show(ui,|ui| {
            ui.strong(tr("Storage age & cost"));
            ui.label(RichText::new(tr("Estimate only. Logical GiB x monthly price; no compression, deduplication, tier or provider charges assumed.")).color(muted(ui)));
            ui.horizontal(|ui| { ui.label(tr("Monthly price per GiB")); ui.add(egui::DragValue::new(&mut self.settings.price_per_gib).range(0.0..=1000.0).speed(0.001).max_decimals(4)); });
            egui::Grid::new("age-grid").num_columns(4).spacing([30.0,15.0]).show(ui,|ui| {
                for header in [tr("Last modified"),tr("Files"),tr("Logical size"),tr("Estimated / month")] { ui.strong(header); } ui.end_row();
                for (index,label) in [tr("Within 30 days"),tr("30-180 days"),tr("Over 180 days"),tr("Unknown")].into_iter().enumerate() { ui.label(label); ui.label(self.age_files[index].to_string()); ui.label(format_bytes(self.age_sizes[index])); ui.label(format!("{:.2}",self.age_sizes[index] as f64/1_073_741_824.0*self.settings.price_per_gib)); ui.end_row(); }
            });
        });
        ui.add_space(12.0);
        card(ui).show(ui,|ui| { ui.strong(tr("Permissions")); ui.label(tr("Select a file or folder in All files to inspect permissions and ownership. Unix mode and UID are shown where available; Windows reports read-only status. Full ACL editing is not available.")); });
    }

    fn settings_page(&mut self, ui: &mut egui::Ui) {
        card(ui).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.strong(tr("Appearance"));
            ui.checkbox(&mut self.settings.reduced_motion, tr("Reduce motion"));
            if cfg!(target_os = "macos") && ui.button(tr("Set up macOS file access...")).clicked() {
                self.access_open = true;
                self.scan_after_access = false;
            }
            let previous_language = self.settings.language;
            ui.horizontal_wrapped(|ui| {
                ui.label(tr("Language"));
                egui::ComboBox::from_id_salt("language")
                    .selected_text(match self.settings.language {
                        Language::System => tr("System language"),
                        Language::English => tr("English"),
                        Language::Russian => tr("Russian"),
                    })
                    .show_ui(ui, |ui| {
                        for (language, label) in [
                            (Language::System, tr("System language")),
                            (Language::English, tr("English")),
                            (Language::Russian, tr("Russian")),
                        ] {
                            if ui
                                .selectable_value(&mut self.settings.language, language, label)
                                .clicked()
                            {
                                ui.close();
                            }
                        }
                    });
            });
            if previous_language != self.settings.language {
                set_language(self.settings.language);
                self.status = tr("Language updated").into();
            }
            ui.label(
                RichText::new(tr(
                    "Follow your device, or choose a theme that feels right.",
                ))
                .color(muted(ui)),
            );
            ui.horizontal_wrapped(|ui| {
                ui.selectable_value(&mut self.settings.theme, Theme::System, tr("System"));
                ui.selectable_value(&mut self.settings.theme, Theme::Light, tr("Light"));
                ui.selectable_value(&mut self.settings.theme, Theme::Dark, tr("Dark"));
            });
            if ui
                .add(
                    egui::Slider::new(&mut self.settings.font_scale, 0.75..=2.0)
                        .text(tr("Interface scale"))
                        .fixed_decimals(2),
                )
                .changed()
            {
                ui.ctx().set_zoom_factor(self.settings.font_scale);
            }
            ui.checkbox(
                &mut self.settings.show_modified,
                tr("Show modified-date column"),
            );
            ui.checkbox(&mut self.settings.show_share, tr("Show size-share column"));
            ui.add_space(10.0);
            ui.checkbox(
                &mut self.settings.labels,
                tr("Show names and sizes on map tiles"),
            );
            ui.add(
                egui::Slider::new(&mut self.settings.tile_gap, 1.0..=6.0).text(tr("Tile spacing")),
            );
            let allocation_available = self.snapshot.as_ref().is_none_or(|snapshot| {
                snapshot
                    .roots
                    .iter()
                    .all(|&index| snapshot.nodes[index].allocated_size.is_some())
            });
            if ui
                .add_enabled(
                    allocation_available,
                    egui::Checkbox::new(
                        &mut self.settings.allocated,
                        tr("Use allocated size on disk"),
                    ),
                )
                .changed()
            {
                self.rebuild = true;
            }
            if !allocation_available {
                ui.label(
                    RichText::new(tr(
                        "Allocated size is unavailable for this scan. Logical bytes are displayed.",
                    ))
                    .small()
                    .color(muted(ui)),
                );
            }
        });
        ui.add_space(12.0);
        card(ui).show(ui,|ui| {
            ui.set_min_width(ui.available_width());
            ui.strong(tr("Scanning")); ui.label(RichText::new(tr("Applied on the next scan. Symlinks and Windows reparse points are never followed.")).color(muted(ui)));
            ui.horizontal_wrapped(|ui| { ui.label(tr("Scan workers")); ui.add(egui::DragValue::new(&mut self.settings.threads).range(0..=64)); ui.label(tr("0 uses available CPU threads")); });
            ui.add_enabled(cfg!(unix),egui::Checkbox::new(&mut self.settings.one_file_system,tr("Stay on the same filesystem (Unix)")));
            ui.label(tr("Exclude paths matching these regular expressions (one per line)"));
            ui.add(egui::TextEdit::multiline(&mut self.settings.exclude).margin(INPUT_MARGIN).hint_text("(^|/)(node_modules|target|\\.git)(/|$)").desired_width(f32::INFINITY).desired_rows(4));
            for expression in self.settings.exclude.lines().filter(|line|!line.is_empty()) { if let Err(error)=regex::Regex::new(expression) { ui.colored_label(ui.visuals().error_fg_color,error.to_string()); } }
            let response=ui.add_enabled(self.live && self.receiver.is_none(),egui::Checkbox::new(&mut self.watching,tr("Watch filesystem changes and refresh automatically")));
            if response.changed() { self.install_watcher(ui.ctx()); }
            ui.label(RichText::new(tr("Watch notifications are debounced for two seconds. Large/network filesystems may exceed OS watcher limits.")).small().color(muted(ui)));
        });
        ui.add_space(12.0);
        card(ui).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.strong(tr("Updates"));
            ui.add_enabled(false, egui::Button::new(tr("Check for updates")));
            ui.label(tr("Releases have not been published yet. Update checks will be available after publication."));
        });
        ui.add_space(12.0);
        card(ui).show(ui,|ui| { ui.set_min_width(ui.available_width()); ui.strong(trf("UniDirStat {version}", &[("version", env!("CARGO_PKG_VERSION").to_owned())])); ui.label(tr("A Rust disk usage explorer inspired by WinDirStat. Native Windows, macOS and Linux architecture. Your scan stays on your computer.")); ui.hyperlink_to(tr("WinDirStat reference project"),"https://github.com/windirstat/windirstat"); ui.label(RichText::new(tr("Raw NTFS/MTP scanning, deleted-file recovery, ACL editing and OS-specific cleanup tools are not implemented.")).small().color(muted(ui))); });
    }

    fn inspector(&mut self, context: &egui::Context) {
        let Some(snapshot) = self.snapshot.as_ref().map(Arc::clone) else {
            return;
        };
        let Some(index) = self.selected else {
            return;
        };
        egui::SidePanel::right("inspector")
            .default_width(280.0)
            .width_range(230.0..=420.0)
            .frame(
                egui::Frame::new()
                    .fill(context.style().visuals.window_fill)
                    .inner_margin(16),
            )
            .show(context, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("inspector-scroll")
                    .show(ui, |ui| self.inspector_contents(ui, &snapshot, index));
            });
    }

    fn inspector_contents(&mut self, ui: &mut egui::Ui, snapshot: &Arc<Snapshot>, index: usize) {
        let context = ui.ctx().clone();
        let Some(node) = snapshot.nodes.get(index) else {
            return;
        };
        ui.horizontal(|ui| {
            ui.strong(tr("File details"));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button(tr("Close")).clicked() {
                    self.selected = None;
                    self.selection.clear();
                }
            });
        });
        ui.separator();
        if self.selection.len() > 1 {
            ui.strong(trf(
                "{count} selected",
                &[("count", self.selection.len().to_string())],
            ));
            ui.label(tr("Cmd/Ctrl toggles items; Shift selects a range. File actions apply to one item at a time."));
            if ui.button(tr("Copy selected paths")).clicked() {
                context.copy_text(
                    self.selection
                        .indices()
                        .filter_map(|index| snapshot.nodes.get(index))
                        .map(|node| display_path(&node.path))
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
            }
        }
        ui.label(RichText::new(node.name()).size(18.0).strong());
        ui.add(
            egui::Label::new(
                RichText::new(display_path(&node.path))
                    .small()
                    .color(muted(ui)),
            )
            .wrap(),
        );
        ui.add_space(10.0);
        for (label, value) in [
            (tr("Logical size"), format_bytes(node.logical_size)),
            (
                tr("Allocated size"),
                node.allocated_size
                    .map_or_else(|| tr("Unavailable").into(), format_bytes),
            ),
            (tr("Kind"), kind_label(node.kind).to_owned()),
            (tr("Modified"), age(node.modified)),
            (
                tr("Read only"),
                if node.readonly { tr("Yes") } else { tr("No") }.into(),
            ),
            (
                tr("Unix mode"),
                node.permissions.map_or_else(
                    || tr("Unavailable").into(),
                    |mode| format!("{:04o}", mode & 0o7777),
                ),
            ),
            (
                tr("Owner UID"),
                node.owner
                    .map_or_else(|| tr("Unavailable").into(), |owner| owner.to_string()),
            ),
        ] {
            ui.label(RichText::new(label).small().color(muted(ui)));
            ui.label(value);
        }
        if node.kind == EntryKind::Directory {
            ui.label(trf(
                "{files} files | {folders} folders",
                &[
                    ("files", node.files.to_string()),
                    ("folders", node.directories.to_string()),
                ],
            ));
            if ui.button(tr("Zoom into folder")).clicked() {
                self.map_focus = Some(index);
                self.page = Page::Overview;
            }
            if ui
                .add_enabled(
                    self.live && self.receiver.is_none(),
                    egui::Button::new(tr("Refresh this folder")),
                )
                .clicked()
            {
                self.refresh_folder(index, &context);
            }
        }
        ui.separator();
        if ui.button(tr("Copy path")).clicked() {
            context.copy_text(display_path(&node.path));
        }
        if ui.button(tr("Reveal in file manager")).clicked() {
            self.run_open(node, true);
        }
        if ui.button(tr("Open")).clicked() {
            self.run_open(node, false);
        }
        if ui
            .add_enabled(
                self.live && self.receiver.is_none(),
                egui::Button::new(tr("Inspect system ACL")),
            )
            .clicked()
        {
            let path = node.path.clone();
            self.job(JobKind::FileOperation, &context, move || {
                JobResult::Permissions(path.clone(), actions::permission_details(&path))
            });
        }
        let can_clean = self.live
            && self.receiver.is_none()
            && node.parent.is_some()
            && matches!(node.kind, EntryKind::File | EntryKind::Directory);
        if ui
            .add_enabled(can_clean, egui::Button::new(tr("Move...")))
            .clicked()
            && let Some(destination) = rfd::FileDialog::new()
                .set_title(tr("Choose destination folder"))
                .pick_folder()
        {
            self.cleanup = Some(Cleanup::Move(index, destination));
        }
        if ui
            .add_enabled(
                can_clean,
                egui::Button::new(
                    RichText::new(tr("Move to trash...")).color(ui.visuals().error_fg_color),
                ),
            )
            .clicked()
        {
            self.cleanup = Some(Cleanup::Trash(index));
        }
        if node.kind == EntryKind::Directory
            && node.children.is_empty()
            && node.error.is_none()
            && ui
                .add_enabled(
                    can_clean,
                    egui::Button::new(tr("Move empty folder to trash...")),
                )
                .clicked()
        {
            self.cleanup = Some(Cleanup::TrashEmpty(index));
        }
        if !self.live {
            ui.label(
                RichText::new(tr("Cleanup is disabled for imported snapshots."))
                    .small()
                    .color(muted(ui)),
            );
        }
        if let Some(error) = &node.error {
            ui.colored_label(ui.visuals().warn_fg_color, error);
        }
    }

    fn item_response(
        &mut self,
        response: egui::Response,
        index: usize,
        snapshot: &Snapshot,
        view: SelectionView,
    ) {
        if response.clicked() {
            let modifiers = click_modifiers(&response.ctx);
            let toggle = modifiers.command || modifiers.ctrl;
            match view {
                SelectionView::Files => {
                    self.selection
                        .click(index, &self.rows, toggle, modifiers.shift)
                }
                SelectionView::Overview => {
                    let summary = self.map_worker.summary().map(Arc::clone);
                    let largest = summary.as_ref().map_or(&self.largest, |summary| {
                        if self.settings.allocated {
                            &summary.largest_allocated
                        } else {
                            &summary.largest_logical
                        }
                    });
                    let order: Vec<_> = largest.iter().map(|&index| (index, 0)).collect();
                    self.selection.click(index, &order, toggle, modifiers.shift);
                }
                SelectionView::Duplicates => {
                    if modifiers.shift {
                        let order = duplicate_file_order(&self.duplicate_rows);
                        self.selection.click(index, &order, toggle, true);
                    } else {
                        self.selection.click(index, &[(index, 0)], toggle, false);
                    }
                }
                SelectionView::Single => self.selection.click(index, &[(index, 0)], toggle, false),
            }
            self.selected = self.selection.primary();
            if matches!(view, SelectionView::Single | SelectionView::Overview) {
                let mut parent = snapshot.nodes[index].parent;
                while let Some(ancestor) = parent {
                    self.rebuild |= self.expanded.insert(ancestor);
                    parent = snapshot.nodes[ancestor].parent;
                }
                self.reveal_row = Some(index);
            }
        }
        if response.secondary_clicked() {
            self.selection.right_click(index);
            if self.selected.is_some() {
                self.selected = self.selection.primary();
            }
        }
        response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::Button,
                response.enabled(),
                self.selection.contains(index),
                trf(
                    "{path} | {bytes} | {share}% of parent",
                    &[
                        ("path", display_path(&snapshot.nodes[index].path)),
                        (
                            "bytes",
                            format_bytes(snapshot.nodes[index].size(self.settings.allocated)),
                        ),
                        (
                            "share",
                            format!(
                                "{:.1}",
                                workspace::parent_share(snapshot, index, self.settings.allocated)
                                    * 100.0
                            ),
                        ),
                    ],
                ),
            )
        });
        let node = &snapshot.nodes[index];
        if response.double_clicked()
            && node.kind == EntryKind::Directory
            && !matches!(view, SelectionView::Single)
        {
            self.map_focus = Some(index);
            self.page = Page::Overview;
            response.ctx.request_repaint();
        }
        response.context_menu(|ui| {
            if ui.button(tr("Zoom to selection")).clicked() {
                self.map_focus = if node.kind == EntryKind::Directory {
                    Some(index)
                } else {
                    node.parent
                };
                self.page = Page::Overview;
                ui.close();
            }
            let single = self.selection.len() <= 1;
            if !single {
                ui.label(trf(
                    "{count} selected",
                    &[("count", self.selection.len().to_string())],
                ));
            }
            if ui.button(tr("Inspect")).clicked() {
                self.selected = self.selection.primary().or(Some(index));
                self.summary_open = true;
                ui.close();
            }
            if ui
                .button(if single {
                    tr("Copy path")
                } else {
                    tr("Copy selected paths")
                })
                .clicked()
            {
                let paths = if self.selection.is_empty() {
                    display_path(&node.path)
                } else {
                    self.selection
                        .indices()
                        .filter_map(|index| snapshot.nodes.get(index))
                        .map(|node| display_path(&node.path))
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                ui.ctx().copy_text(paths);
                ui.close();
            }
            if ui
                .add_enabled(single, egui::Button::new(tr("Open")))
                .clicked()
            {
                self.run_open(node, false);
                ui.close();
            }
            if ui
                .add_enabled(single, egui::Button::new(tr("Reveal in file manager")))
                .clicked()
            {
                self.run_open(node, true);
                ui.close();
            }
            if node.kind == EntryKind::Directory
                && ui
                    .add_enabled(
                        single && self.live && self.receiver.is_none(),
                        egui::Button::new(tr("Refresh this folder")),
                    )
                    .clicked()
            {
                self.refresh_folder(index, &response.ctx);
                ui.close();
            }
            if ui
                .add_enabled(
                    single && self.live && self.receiver.is_none() && node.parent.is_some(),
                    egui::Button::new(tr("Move to trash...")),
                )
                .clicked()
            {
                self.cleanup = Some(Cleanup::Trash(index));
                ui.close();
            }
        });
    }

    fn access_dialog(&mut self, context: &egui::Context) {
        if !self.access_open {
            return;
        }
        let mut open = true;
        let mut proceed = false;
        egui::Window::new(tr("Set up macOS file access"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(510.0)
            .show(context, |ui| {
                ui.label(tr("Before scanning the whole disk, add UniDirStat to Full Disk Access in System Settings → Privacy & Security. This avoids many separate folder-access prompts."));
                ui.label(tr("Full Disk Access can expose private files, messages and browser data. Enable it only if you want a whole-disk scan; limited scanning is also available."));
                ui.label(tr("UniDirStat cannot grant this permission or reliably verify it. If macOS asks you to restart the app, do so before scanning."));
                if ui.button(tr("Open macOS privacy settings")).clicked() {
                    #[cfg(target_os = "macos")]
                    if let Err(error) = std::process::Command::new("/usr/bin/open")
                        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
                        .spawn()
                    {
                        self.error = Some(error.to_string());
                    }
                }
                ui.horizontal_wrapped(|ui| {
                    proceed |= ui.button(tr("Continue after setup")).clicked();
                    proceed |= ui.button(tr("Use limited access")).clicked();
                });
                ui.label(RichText::new(tr("Protected paths may still be unavailable. Local ad-hoc builds can require permission again after an update.")).small().color(muted(ui)));
            });
        if proceed {
            self.settings.mac_access_reviewed = true;
            self.access_open = false;
            if std::mem::take(&mut self.scan_after_access) {
                self.start_scan(context);
            }
        } else if !open {
            self.access_open = false;
            self.scan_after_access = false;
        }
    }

    fn run_open(&mut self, node: &Node, reveal: bool) {
        if let Err(error) = actions::open_path(&node.path, reveal) {
            self.error = Some(error);
        }
    }

    fn confirm_cleanup(&mut self, context: &egui::Context) {
        let Some(cleanup) = &self.cleanup else {
            return;
        };
        let Some(snapshot) = self.snapshot.as_ref().map(Arc::clone) else {
            self.cleanup = None;
            return;
        };
        let index = match cleanup {
            Cleanup::Trash(index) | Cleanup::TrashEmpty(index) | Cleanup::Move(index, _) => *index,
        };
        let Some(node) = snapshot.nodes.get(index) else {
            self.cleanup = None;
            return;
        };
        let mut confirm = false;
        let mut cancel = false;
        egui::Modal::new(egui::Id::new("cleanup-confirmation")).show(context,|ui| {
            ui.set_width(440.0);
            ui.heading(match cleanup { Cleanup::Trash(_) | Cleanup::TrashEmpty(_)=>tr("Move to system trash?"),Cleanup::Move(_,_)=>tr("Move this item?") });
            ui.label(display_path(&node.path));
            if let Cleanup::Move(_,destination)=cleanup { ui.label(trf("Destination: {path}",&[("path",display_path(destination))])); ui.label(tr("Existing files will never be overwritten. Atomic moves require the same filesystem; cross-volume moves fail safely.")); }
            else if matches!(cleanup, Cleanup::TrashEmpty(_)) { ui.label(tr("The folder must still be empty when the operation runs.")); }
            else { ui.label(tr("The item and, for a folder, its contents will be sent to your system trash. Recovery depends on your operating system and filesystem.")); }
            ui.add_space(12.0); ui.horizontal(|ui| { cancel=ui.button(tr("Cancel")).clicked(); confirm=ui.button(tr("Confirm")).clicked(); });
        });
        if cancel {
            self.cleanup = None;
        } else if confirm {
            if !self.live || self.receiver.is_some() {
                self.cleanup = None;
                return;
            }
            let node = node.clone();
            if let Some(cleanup) = self.cleanup.take() {
                let refresh_parent = match &cleanup {
                    Cleanup::Trash(_) | Cleanup::TrashEmpty(_) => node.parent,
                    Cleanup::Move(_, _) => None,
                };
                self.job(JobKind::FileOperation, context, move || {
                    let result = match cleanup {
                        Cleanup::Trash(_) => actions::trash_node(&node),
                        Cleanup::TrashEmpty(_) => actions::trash_empty_folder(&node),
                        Cleanup::Move(_, destination) => actions::move_node(&node, &destination),
                    };
                    if let Some(parent) = refresh_parent {
                        JobResult::Cleanup(result.map(|()| parent))
                    } else {
                        JobResult::Io(result.map(|()| {
                            tr("File operation complete. Refresh to update the scan.").into()
                        }))
                    }
                });
            }
        }
    }
}

impl eframe::App for UniDirStatApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.apply_theme(context);
        self.poll(context);
        if context.input(|input| input.key_pressed(egui::Key::F5)) {
            self.start_scan(context);
        }
        if self.receiver.is_some() && self.job_kind.is_some_and(JobKind::cancellable) {
            if context.input(|input| input.key_pressed(egui::Key::Escape)) {
                self.control.cancel();
            }
            if context.input(|input| input.key_pressed(egui::Key::F6))
                && !self.control.is_cancelled()
            {
                if self.control.is_paused() {
                    self.control.resume();
                } else {
                    self.control.pause();
                }
            }
        }
        self.sidebar(context);
        if self.last_page != self.page {
            self.rebuild |= page_needs_rows(self.page, self.rows_largest);
            context.animate_value_with_time(egui::Id::new("page-opacity"), 0.0, 0.0);
            self.last_page = self.page;
        }
        if context
            .memory(|memory| memory.focused())
            .is_none_or(|id| egui::TextEdit::load_state(context, id).is_none())
            && context.input_mut(|input| {
                input.consume_key(egui::Modifiers::COMMAND, egui::Key::A)
                    || input.consume_key(egui::Modifiers::CTRL, egui::Key::A)
            })
        {
            if matches!(self.page, Page::Overview | Page::Files | Page::Largest) {
                self.selection.select_all(&self.rows);
            } else if self.page == Page::Duplicates {
                self.selection
                    .select_all(&duplicate_file_order(&self.duplicate_rows));
            }
            self.selected = self.selection.primary();
        }
        if !matches!(self.page, Page::Overview | Page::Files | Page::Settings) {
            self.inspector(context);
        }
        egui::TopBottomPanel::bottom("status")
            .frame(
                egui::Frame::new()
                    .fill(context.style().visuals.panel_fill)
                    .inner_margin(egui::Margin::symmetric(20, 8)),
            )
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Label::new(RichText::new(&self.status).small().color(muted(ui)))
                            .truncate(),
                    )
                    .on_hover_text(&self.status);
                });
            });
        let margin = if context.content_rect().width() < 1100.0 {
            12
        } else {
            24
        };
        egui::CentralPanel::default().frame(egui::Frame::new().fill(context.style().visuals.panel_fill).inner_margin(margin)).show(context,|ui| {
            ui.set_opacity(context.animate_value_with_time(egui::Id::new("page-opacity"),1.0,if self.settings.reduced_motion {0.0} else {0.16}));
            self.header(ui);
            if self.page==Page::Settings { egui::ScrollArea::vertical().show(ui,|ui|self.settings_page(ui)); return; }
            let Some(snapshot)=self.snapshot.as_ref().map(Arc::clone) else { egui::ScrollArea::vertical().show(ui,|ui|self.welcome(ui)); return; };
            if matches!(self.page, Page::Overview | Page::Files | Page::Types | Page::Largest) { self.map_worker.request_summary(self.scope, Arc::clone(&snapshot), ui.ctx()); }
            if self.settings.allocated && snapshot.roots.iter().any(|&index|snapshot.nodes[index].allocated_size.is_none()) { self.settings.allocated=false; self.rebuild=true;  }
            match self.page {
                Page::Overview|Page::Files=>self.overview(ui,&snapshot),
                Page::Largest=>self.files(ui,&snapshot),
                Page::Types=>self.types(ui,&snapshot),
                Page::Duplicates=>self.duplicates_page(ui,&snapshot),
                Page::Storage=>{ egui::ScrollArea::vertical().show(ui,|ui|self.storage(ui,&snapshot)); },
                Page::Issues=>{ ui.label(trf("{count} scan issues. Inaccessible paths are never silently treated as empty.",&[("count",snapshot.errors.len().to_string())])); egui::ScrollArea::vertical().show(ui,|ui| { for error in &snapshot.errors { ui.label(error); ui.separator(); } }); },
                Page::Settings=>{},
            }
        });
        self.access_dialog(context);
        self.confirm_cleanup(context);
        if let Some((path, details)) = &self.permission_details {
            let mut open = true;
            egui::Window::new(tr("System permissions"))
                .open(&mut open)
                .default_width(650.0)
                .show(context, |ui| {
                    ui.label(display_path(path));
                    if ui.button(tr("Copy path")).clicked() {
                        context.copy_text(display_path(path));
                    }
                    egui::ScrollArea::both().max_height(400.0).show(ui, |ui| {
                        ui.monospace(details);
                    });
                });
            if !open {
                self.permission_details = None;
            }
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if let Some(path) = &self.settings_path {
            if let Err(details) = report::write_json(path, &self.settings) {
                self.error = Some(trf(
                    "Could not save preferences: {details}",
                    &[("details", details)],
                ));
            }
        } else {
            eframe::set_value(storage, "settings", &self.settings);
        }
    }
}

impl Drop for UniDirStatApp {
    fn drop(&mut self) {
        self.control.cancel();
        self.search_control.cancel();
    }
}

fn age(modified: Option<u64>) -> String {
    modified.map_or_else(
        || tr("Unknown").into(),
        |modified| {
            let days = now().saturating_sub(modified) / 86_400;
            if days == 0 {
                tr("Today").into()
            } else if days == 1 {
                tr("Yesterday").into()
            } else {
                trf("{days} days ago", &[("days", days.to_string())])
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refresh_test_app(root: &Path) -> (UniDirStatApp, egui::Context) {
        let context = egui::Context::default();
        let creation = eframe::CreationContext::_new_kittest(context.clone());
        let mut app = UniDirStatApp::new(&creation, Vec::new(), None, ScanOptions::default(), None);
        app.settings.threads = 1;
        let snapshot = scanner::scan(
            &[root.into()],
            &ScanOptions {
                threads: 1,
                ..ScanOptions::default()
            },
            &ScanControl::default(),
        )
        .unwrap();
        queue_refresh_result(
            &mut app,
            JobKind::Scan,
            JobResult::Scan(Ok(prepare_snapshot(snapshot, 1, &ScanControl::default()))),
        );
        app.poll(&context);
        (app, context)
    }

    fn queue_refresh_result(app: &mut UniDirStatApp, kind: JobKind, result: JobResult) {
        let (sender, receiver) = mpsc::channel();
        app.receiver = Some(receiver);
        app.job_kind = Some(kind);
        assert!(sender.send(result).is_ok());
    }

    fn finish_refresh_jobs(app: &mut UniDirStatApp, context: &egui::Context) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.receiver.is_some() {
            assert!(Instant::now() < deadline, "background refresh timed out");
            app.poll(context);
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn refresh_node(app: &UniDirStatApp, name: &str) -> usize {
        app.snapshot
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .position(|node| node.name() == name)
            .unwrap()
    }

    #[test]
    fn folder_recalculation_rebuilds_global_file_color_ranks() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::write(fixture.path().join("large.raw"), [0; 100]).unwrap();
        std::fs::write(fixture.path().join("small.txt"), [0; 10]).unwrap();
        let (mut app, context) = refresh_test_app(fixture.path());
        assert_eq!(app.file_colors.index("raw"), 0);
        assert_eq!(app.file_colors.index("txt"), 1);
        std::fs::write(fixture.path().join("small.txt"), [0; 200]).unwrap();
        let root = app.snapshot.as_ref().unwrap().roots[0];
        app.refresh_folder(root, &context);
        finish_refresh_jobs(&mut app, &context);
        assert_eq!(app.file_colors.index("txt"), 0);
        assert_eq!(app.file_colors.index("raw"), 1);
    }

    #[test]
    fn folder_refresh_preserves_surviving_view_paths_after_arena_reordering() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::create_dir(fixture.path().join("changed")).unwrap();
        std::fs::create_dir(fixture.path().join("retained")).unwrap();
        std::fs::write(fixture.path().join("changed/old"), b"before").unwrap();
        std::fs::write(fixture.path().join("retained/keep"), b"keep").unwrap();
        let (mut app, context) = refresh_test_app(fixture.path());
        let changed = refresh_node(&app, "changed");
        let retained = refresh_node(&app, "retained");
        let keep = refresh_node(&app, "keep");
        app.selected = Some(keep);
        app.selection.click(changed, &[], false, false);
        app.selection.click(keep, &[], true, false);
        app.scope = Some(changed);
        app.search_query.scope = Some(changed);
        app.map_focus = Some(changed);
        app.expanded.extend([changed, retained]);
        app.reveal_row = Some(keep);
        app.duplicates = Some(Arc::new(DuplicateReport::default()));
        app.duplicate_rows.push(DuplicateRow::File(keep));
        app.duplicate_reclaimable = 4;
        std::fs::write(fixture.path().join("changed/new"), b"new").unwrap();
        app.refresh_folder(changed, &context);
        finish_refresh_jobs(&mut app, &context);
        let snapshot = app.snapshot.as_ref().unwrap();
        assert_eq!(
            app.selected.map(|index| snapshot.nodes[index].name()),
            Some("keep".into())
        );
        assert_eq!(
            app.selection
                .primary()
                .map(|index| snapshot.nodes[index].name()),
            Some("keep".into())
        );
        assert_eq!(
            app.scope.map(|index| snapshot.nodes[index].name()),
            Some("changed".into())
        );
        assert_eq!(app.search_query.scope, app.scope);
        assert_eq!(app.map_focus, app.scope);
        assert!(app.expanded.contains(&refresh_node(&app, "retained")));
        assert!(app.selection.contains(refresh_node(&app, "changed")));
        assert_eq!(app.reveal_row, None);
        assert!(app.duplicates.is_none());
        assert!(app.duplicate_rows.is_empty());
        assert_eq!(app.duplicate_reclaimable, 0);
        assert!(app.live);
    }

    #[test]
    fn folder_refresh_cancellation_after_preparation_preserves_the_previous_snapshot() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::write(fixture.path().join("old"), b"old").unwrap();
        let (mut app, context) = refresh_test_app(fixture.path());
        let previous = Arc::clone(app.snapshot.as_ref().unwrap());
        std::fs::write(fixture.path().join("new"), b"new").unwrap();
        let fresh = scanner::scan(
            &[fixture.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        let prepared = prepare_snapshot(fresh, 1, &ScanControl::default());
        app.rebuild = false;
        queue_refresh_result(&mut app, JobKind::Refresh, JobResult::Scan(Ok(prepared)));
        app.control.cancel();
        app.poll(&context);
        assert!(Arc::ptr_eq(app.snapshot.as_ref().unwrap(), &previous));
        assert!(app.rebuild);
    }

    #[test]
    fn completed_trash_refreshes_the_parent_without_rescanning_siblings() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::create_dir(fixture.path().join("changed")).unwrap();
        std::fs::create_dir(fixture.path().join("retained")).unwrap();
        let victim = fixture.path().join("changed/victim");
        std::fs::write(&victim, b"disposable").unwrap();
        std::fs::write(fixture.path().join("retained/keep"), b"keep").unwrap();
        let (mut app, context) = refresh_test_app(fixture.path());
        std::fs::write(
            fixture.path().join("retained/not-scanned"),
            b"outside refresh",
        )
        .unwrap();
        let index = refresh_node(&app, "victim");
        let parent = app.snapshot.as_ref().unwrap().nodes[index].parent.unwrap();
        app.selected = Some(index);
        app.selection.click(index, &[], false, false);
        std::fs::remove_file(&victim).unwrap();
        queue_refresh_result(
            &mut app,
            JobKind::FileOperation,
            JobResult::Cleanup(Ok(parent)),
        );
        finish_refresh_jobs(&mut app, &context);
        assert!(!victim.exists());
        let snapshot = app.snapshot.as_ref().unwrap();
        assert_eq!(snapshot.file_count(), 1);
        assert_eq!(snapshot.total_size(false), 4);
        assert!(
            snapshot
                .nodes
                .iter()
                .all(|node| node.name() != "victim" && node.name() != "not-scanned")
        );
        assert!(app.live);
        assert_eq!(app.selected, None);
        assert!(app.selection.is_empty());
    }

    #[test]
    fn folder_refresh_clears_directory_scopes_replaced_by_regular_files() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::create_dir(fixture.path().join("selected")).unwrap();
        let (mut app, context) = refresh_test_app(fixture.path());
        let selected = refresh_node(&app, "selected");
        app.scope = Some(selected);
        app.search_query.scope = Some(selected);
        app.map_focus = Some(selected);
        app.expanded.insert(selected);
        std::fs::remove_dir(fixture.path().join("selected")).unwrap();
        std::fs::write(fixture.path().join("selected"), b"replacement").unwrap();
        let root = app.snapshot.as_ref().unwrap().roots[0];
        app.refresh_folder(root, &context);
        finish_refresh_jobs(&mut app, &context);
        assert_eq!(app.scope, None);
        assert_eq!(app.search_query.scope, None);
        assert_eq!(app.map_focus, None);
        assert!(!app.expanded.contains(&refresh_node(&app, "selected")));
    }

    #[test]
    fn failed_folder_refresh_preserves_data_and_rebuilds_cancelled_search() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::create_dir(fixture.path().join("selected")).unwrap();
        let (mut app, context) = refresh_test_app(fixture.path());
        let selected = refresh_node(&app, "selected");
        let previous = Arc::clone(app.snapshot.as_ref().unwrap());
        app.rebuild = false;
        std::fs::remove_dir(fixture.path().join("selected")).unwrap();
        app.refresh_folder(selected, &context);
        finish_refresh_jobs(&mut app, &context);
        assert!(Arc::ptr_eq(app.snapshot.as_ref().unwrap(), &previous));
        assert!(app.rebuild);
        assert!(app.error.is_some());
    }

    #[test]
    fn completed_trash_refresh_failure_reports_that_cleanup_already_happened() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::create_dir(fixture.path().join("selected")).unwrap();
        let victim = fixture.path().join("selected/victim");
        std::fs::write(&victim, b"disposable").unwrap();
        let (mut app, context) = refresh_test_app(fixture.path());
        let parent = refresh_node(&app, "selected");
        let previous = Arc::clone(app.snapshot.as_ref().unwrap());
        std::fs::remove_file(&victim).unwrap();
        std::fs::remove_dir(fixture.path().join("selected")).unwrap();
        queue_refresh_result(
            &mut app,
            JobKind::FileOperation,
            JobResult::Cleanup(Ok(parent)),
        );
        finish_refresh_jobs(&mut app, &context);
        assert!(Arc::ptr_eq(app.snapshot.as_ref().unwrap(), &previous));
        assert_eq!(
            app.status,
            tr("Item moved to trash. Scan results are outdated.")
        );
        assert!(
            app.error.as_deref().unwrap().contains(
                tr("The item was moved to trash, but folder recalculation did not finish: {error}")
                    .split("{error}")
                    .next()
                    .unwrap()
            )
        );
        assert!(app.rebuild);
        assert!(app.live);
    }

    #[test]
    fn failed_cleanup_does_not_refresh_or_change_the_snapshot() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::write(fixture.path().join("keep"), b"keep").unwrap();
        let (mut app, context) = refresh_test_app(fixture.path());
        let previous = Arc::clone(app.snapshot.as_ref().unwrap());
        queue_refresh_result(
            &mut app,
            JobKind::FileOperation,
            JobResult::Cleanup(Err("validation refused".into())),
        );
        app.poll(&context);
        assert!(Arc::ptr_eq(app.snapshot.as_ref().unwrap(), &previous));
        assert!(app.receiver.is_none());
        assert_eq!(app.error.as_deref(), Some("validation refused"));
    }

    #[test]
    fn folder_recalculation_targets_only_live_idle_selected_or_scoped_directories() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::create_dir(fixture.path().join("folder")).unwrap();
        std::fs::write(fixture.path().join("folder/file"), b"file").unwrap();
        let (mut app, context) = refresh_test_app(fixture.path());
        let folder = refresh_node(&app, "folder");
        let file = refresh_node(&app, "file");
        assert_eq!(app.recalculation_target(), None);
        app.scope = Some(folder);
        assert_eq!(app.recalculation_target(), Some(folder));
        app.selection.click(file, &[], false, false);
        assert_eq!(app.recalculation_target(), Some(folder));
        app.scope = None;
        app.selection.click(folder, &[], false, false);
        assert_eq!(app.recalculation_target(), Some(folder));
        app.live = false;
        assert_eq!(app.recalculation_target(), None);
        app.refresh_folder(folder, &context);
        assert!(app.receiver.is_none());
        app.live = true;
        app.refresh_folder(file, &context);
        assert!(app.receiver.is_none());
        app.refresh_folder(usize::MAX, &context);
        assert!(app.receiver.is_none());
        app.refresh_folder(folder, &context);
        assert_eq!(app.recalculation_target(), None);
        finish_refresh_jobs(&mut app, &context);
    }

    #[test]
    fn watched_trash_events_refresh_only_the_recalculated_parent_and_keep_its_view() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::create_dir(fixture.path().join("changed")).unwrap();
        std::fs::create_dir(fixture.path().join("retained")).unwrap();
        std::fs::write(fixture.path().join("changed/victim"), b"disposable").unwrap();
        let (mut app, context) = refresh_test_app(fixture.path());
        let changed = refresh_node(&app, "changed");
        let victim = app.snapshot.as_ref().unwrap().nodes[refresh_node(&app, "victim")]
            .path
            .clone();
        std::fs::write(fixture.path().join("retained/outside"), b"outside refresh").unwrap();
        app.settings.mac_access_reviewed = true;
        app.watching = true;
        app.selected = Some(changed);
        app.selection.click(changed, &[], false, false);
        app.scope = Some(changed);
        let (sender, receiver) = mpsc::sync_channel(WATCH_PATH_LIMIT);
        app.watch_paths = Some((sender.clone(), receiver));
        std::fs::remove_file(&victim).unwrap();
        sender.send(victim).unwrap();
        app.last_change = Some(Instant::now() - Duration::from_secs(3));
        queue_refresh_result(
            &mut app,
            JobKind::FileOperation,
            JobResult::Cleanup(Ok(changed)),
        );
        finish_refresh_jobs(&mut app, &context);
        app.last_change = Some(Instant::now() - Duration::from_secs(3));
        app.poll(&context);
        assert_eq!(app.job_kind, Some(JobKind::Refresh));
        finish_refresh_jobs(&mut app, &context);
        let snapshot = app.snapshot.as_ref().unwrap();
        assert!(snapshot.nodes.iter().all(|node| node.name() != "outside"));
        assert_eq!(
            app.selected.map(|index| snapshot.nodes[index].name()),
            Some("changed".into())
        );
        assert_eq!(
            app.scope.map(|index| snapshot.nodes[index].name()),
            Some("changed".into())
        );
    }

    #[test]
    fn watcher_events_outside_the_refreshed_folder_keep_the_full_scan_fallback() {
        for parent_event in [false, true] {
            let fixture = tempfile::tempdir().unwrap();
            std::fs::create_dir(fixture.path().join("changed")).unwrap();
            std::fs::create_dir(fixture.path().join("retained")).unwrap();
            let (mut app, context) = refresh_test_app(fixture.path());
            app.settings.mac_access_reviewed = true;
            let changed = refresh_node(&app, "changed");
            app.refresh_folder(changed, &context);
            finish_refresh_jobs(&mut app, &context);
            std::fs::write(fixture.path().join("retained/outside"), b"outside refresh").unwrap();
            let snapshot = app.snapshot.as_ref().unwrap();
            let event = if parent_event {
                snapshot.nodes[snapshot.roots[0]].path.clone()
            } else {
                snapshot.nodes[refresh_node(&app, "retained")]
                    .path
                    .join("outside")
            };
            app.watching = true;
            let (sender, receiver) = mpsc::sync_channel(WATCH_PATH_LIMIT);
            app.watch_paths = Some((sender.clone(), receiver));
            sender.send(event).unwrap();
            app.poll(&context);
            app.last_change = Some(Instant::now() - Duration::from_secs(3));
            app.poll(&context);
            assert_eq!(app.job_kind, Some(JobKind::Scan));
            finish_refresh_jobs(&mut app, &context);
            assert!(
                app.snapshot
                    .as_ref()
                    .unwrap()
                    .nodes
                    .iter()
                    .any(|node| node.name() == "outside")
            );
            assert!(app.last_refreshed.is_none());
        }
    }

    #[test]
    fn watcher_unknown_notifications_keep_the_full_scan_fallback() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::create_dir(fixture.path().join("changed")).unwrap();
        let (mut app, context) = refresh_test_app(fixture.path());
        app.settings.mac_access_reviewed = true;
        let changed = refresh_node(&app, "changed");
        app.refresh_folder(changed, &context);
        finish_refresh_jobs(&mut app, &context);
        app.watching = true;
        app.changed.store(true, Ordering::Relaxed);
        app.poll(&context);
        app.last_change = Some(Instant::now() - Duration::from_secs(3));
        app.poll(&context);
        assert_eq!(app.job_kind, Some(JobKind::Scan));
        finish_refresh_jobs(&mut app, &context);
    }

    #[test]
    fn watcher_queue_and_pending_path_overflow_keep_the_full_scan_fallback() {
        for pending_overflow in [false, true] {
            let fixture = tempfile::tempdir().unwrap();
            std::fs::create_dir(fixture.path().join("changed")).unwrap();
            let (mut app, context) = refresh_test_app(fixture.path());
            app.settings.mac_access_reviewed = true;
            let changed = refresh_node(&app, "changed");
            app.refresh_folder(changed, &context);
            finish_refresh_jobs(&mut app, &context);
            let folder = app.last_refreshed.as_ref().unwrap().0.clone();
            app.watching = true;
            let (sender, receiver) = mpsc::sync_channel(WATCH_PATH_LIMIT);
            app.watch_paths = Some((sender.clone(), receiver));
            for index in 0..WATCH_PATH_LIMIT {
                queue_watch_paths(
                    notify::Event::new(notify::EventKind::Create(notify::event::CreateKind::File))
                        .add_path(folder.join(index.to_string())),
                    &sender,
                    &app.changed,
                );
            }
            if pending_overflow {
                app.poll(&context);
                assert_eq!(app.pending_watch_paths.len(), WATCH_PATH_LIMIT);
            }
            queue_watch_paths(
                notify::Event::new(notify::EventKind::Create(notify::event::CreateKind::File))
                    .add_path(folder.join("overflow")),
                &sender,
                &app.changed,
            );
            app.poll(&context);
            assert!(app.watch_full_scan);
            assert!(app.pending_watch_paths.len() <= WATCH_PATH_LIMIT);
            app.last_change = Some(Instant::now() - Duration::from_secs(3));
            app.poll(&context);
            assert_eq!(app.job_kind, Some(JobKind::Scan));
            finish_refresh_jobs(&mut app, &context);
        }
    }

    #[test]
    fn watcher_path_queue_marks_empty_unknown_and_rescan_events_for_a_full_scan() {
        let fixture = tempfile::tempdir().unwrap();
        let (sender, receiver) = mpsc::sync_channel(WATCH_PATH_LIMIT);
        let changed = AtomicBool::new(false);
        for event in [
            notify::Event::new(notify::EventKind::Create(notify::event::CreateKind::File)),
            notify::Event::new(notify::EventKind::Any).add_path(fixture.path().join("unknown")),
            notify::Event::new(notify::EventKind::Other).add_path(fixture.path().join("other")),
            notify::Event::new(notify::EventKind::Create(notify::event::CreateKind::File))
                .add_path(fixture.path().join("rescan"))
                .set_flag(notify::event::Flag::Rescan),
        ] {
            queue_watch_paths(event, &sender, &changed);
            assert!(changed.swap(false, Ordering::Relaxed));
            assert!(receiver.try_recv().is_err());
        }
    }

    #[test]
    fn watcher_path_queue_survives_watcher_reinstallation_after_a_full_scan() {
        let fixture = tempfile::tempdir().unwrap();
        let (mut app, context) = refresh_test_app(fixture.path());
        app.settings.mac_access_reviewed = true;
        app.watching = true;
        let (sender, receiver) = mpsc::sync_channel(WATCH_PATH_LIMIT);
        app.watch_paths = Some((sender.clone(), receiver));
        app.start_scan(&context);
        finish_refresh_jobs(&mut app, &context);
        app.watcher = None;
        let root = app.snapshot.as_ref().unwrap().nodes[app.snapshot.as_ref().unwrap().roots[0]]
            .path
            .clone();
        let path = root.join("late");
        std::fs::write(&path, b"new data").unwrap();
        sender.send(path).unwrap();
        app.poll(&context);
        assert!(app.last_change.is_some());
        app.last_change = Some(Instant::now() - Duration::from_secs(3));
        app.poll(&context);
        assert_eq!(app.job_kind, Some(JobKind::Scan));
        finish_refresh_jobs(&mut app, &context);
        assert!(
            app.snapshot
                .as_ref()
                .unwrap()
                .nodes
                .iter()
                .any(|node| node.name() == "late")
        );
    }

    #[test]
    fn watcher_dispatch_requires_a_live_snapshot_and_an_idle_job() {
        let fixture = tempfile::tempdir().unwrap();
        let (mut app, context) = refresh_test_app(fixture.path());
        app.settings.mac_access_reviewed = true;
        app.watching = true;
        app.live = false;
        app.last_change = Some(Instant::now() - Duration::from_secs(3));
        app.poll(&context);
        assert!(app.receiver.is_none());
        app.live = true;
        let (sender, receiver) = mpsc::channel();
        app.receiver = Some(receiver);
        app.job_kind = Some(JobKind::FileOperation);
        app.poll(&context);
        assert_eq!(app.job_kind, Some(JobKind::FileOperation));
        assert!(sender.send(JobResult::Io(Ok("complete".into()))).is_ok());
        app.poll(&context);
        assert_eq!(app.job_kind, Some(JobKind::Scan));
        finish_refresh_jobs(&mut app, &context);
    }

    #[test]
    fn click_keeps_its_command_modifier_if_released_in_the_same_frame() {
        let context = egui::Context::default();
        let mut toggle = false;
        let _ = context.run(
            egui::RawInput {
                events: vec![egui::Event::PointerButton {
                    pos: egui::Pos2::ZERO,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers {
                        command: true,
                        ..Default::default()
                    },
                }],
                ..Default::default()
            },
            |context| {
                toggle = click_modifiers(context).command;
            },
        );
        assert!(toggle);
    }

    #[test]
    fn duplicate_hash_caption_uses_the_completed_reports_algorithm() {
        assert_eq!(
            duplicate_hash_label(HashAlgorithm::Sha512, "abc"),
            "SHA-512 abc"
        );
        assert_eq!(
            duplicate_hash_label(HashAlgorithm::Sha256, "def"),
            "SHA-256 def"
        );
    }

    #[test]
    fn queued_search_success_is_discarded_after_cancellation() {
        let ready = RowsReady {
            generation: 7,
            index: None,
            rows: Ok(vec![(0, 0)]),
        };
        let control = ScanControl::default();
        assert!(ready.is_current(7, &control));
        assert!(!ready.is_current(8, &control));
        control.cancel();
        assert!(!ready.is_current(7, &control));
    }

    #[test]
    fn background_tree_rows_skip_filtering_collapsed_descendants() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::create_dir(fixture.path().join("folder")).unwrap();
        std::fs::write(fixture.path().join("folder/file"), b"data").unwrap();
        std::fs::write(fixture.path().join("small"), b"x").unwrap();
        std::fs::write(fixture.path().join("large"), [0; 64]).unwrap();
        let snapshot = Arc::new(
            scanner::scan(
                &[fixture.path().into()],
                &ScanOptions::default(),
                &ScanControl::default(),
            )
            .unwrap(),
        );
        let root = snapshot.roots[0];
        let index = SearchIndex::new(Arc::clone(&snapshot));
        let control = ScanControl::default();
        let rows = build_rows(
            &snapshot,
            &index,
            &SearchQuery::default(),
            false,
            &HashSet::from([root]),
            &control,
        )
        .unwrap();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0], (root, 0));
        assert_eq!(
            rows[1..]
                .iter()
                .map(|&(index, depth)| (snapshot.nodes[index].name(), depth))
                .collect::<Vec<_>>(),
            [
                ("large".into(), 1),
                ("folder".into(), 1),
                ("small".into(), 1)
            ]
        );
        assert_eq!(control.visited.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn background_flat_rows_include_matches_inside_collapsed_folders() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::create_dir(fixture.path().join("folder")).unwrap();
        std::fs::write(fixture.path().join("folder/file"), b"data").unwrap();
        let snapshot = Arc::new(
            scanner::scan(
                &[fixture.path().into()],
                &ScanOptions::default(),
                &ScanControl::default(),
            )
            .unwrap(),
        );
        let index = SearchIndex::new(Arc::clone(&snapshot));
        let query = SearchQuery {
            text: "file".into(),
            target: SearchTarget::Name,
            kinds: vec![EntryKind::File],
            ..SearchQuery::default()
        };
        let rows = build_rows(
            &snapshot,
            &index,
            &query,
            true,
            &HashSet::new(),
            &ScanControl::default(),
        )
        .unwrap();
        assert_eq!(
            rows.iter()
                .map(|&(index, depth)| (snapshot.nodes[index].name(), depth))
                .collect::<Vec<_>>(),
            [("file".into(), 0)]
        );
    }

    #[test]
    fn cancelled_preparation_does_not_start_full_analysis_passes() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::write(fixture.path().join("data.txt"), b"contents").unwrap();
        let snapshot = scanner::scan(
            &[fixture.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        let control = ScanControl::default();
        control.cancel();
        let prepared = prepare_snapshot(snapshot, 2, &control);
        assert!(prepared.snapshot.cancelled);
        assert!(prepared.index.is_none());
        assert!(prepared.extensions.iter().all(Vec::is_empty));
        assert!(prepared.largest.is_empty());
    }

    #[test]
    fn row_assembly_and_age_statistics_report_their_own_work() {
        let fixture = tempfile::tempdir().expect("fixture");
        std::fs::write(fixture.path().join("file"), [1; 4]).expect("file");
        let snapshot = Arc::new(
            scanner::scan(
                &[fixture.path().into()],
                &ScanOptions::default(),
                &ScanControl::default(),
            )
            .expect("scan"),
        );
        let control = ScanControl::default();
        let index = SearchIndex::new(Arc::clone(&snapshot));
        let rows = build_rows(
            &snapshot,
            &index,
            &SearchQuery::default(),
            true,
            &HashSet::new(),
            &control,
        )
        .expect("rows");
        assert_eq!(control.phase(), ScanPhase::Mapping);
        assert_eq!(control.work_done.load(Ordering::Relaxed), rows.len() as u64);
        assert_eq!(
            control.work_total.load(Ordering::Relaxed),
            rows.len() as u64
        );
        storage_buckets_with_control(&snapshot, now(), &control).expect("buckets");
        assert_eq!(control.phase(), ScanPhase::Summarizing);
        assert_eq!(
            control.work_done.load(Ordering::Relaxed),
            snapshot.nodes.len() as u64
        );
        assert_eq!(
            control.work_total.load(Ordering::Relaxed),
            snapshot.nodes.len() as u64
        );
    }

    #[test]
    fn saved_filters_roundtrip_without_snapshot_specific_indices() {
        let mut settings = Settings::default();
        let query = SearchQuery {
            text: "тест".into(),
            scope: Some(1234),
            min_size: Some(32),
            max_size: Some(99),
            owner: Some(501),
            ..SearchQuery::default()
        };
        save_filter(&mut settings, "  Work  ", &query).unwrap();
        let restored: Settings =
            serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
        assert_eq!(restored.filters[0].name, "Work");
        assert_eq!(restored.filters[0].query.scope, None);
        assert_eq!(restored.filters[0].query.owner, Some(501));
        assert!(save_filter(&mut settings, "   ", &query).is_err());
    }

    #[test]
    fn cancellation_wins_over_a_requested_pause() {
        let control = ScanControl::default();
        assert_eq!(job_state(&control), JobState::Running);
        control.pause();
        assert_eq!(job_state(&control), JobState::Paused);
        control.cancel();
        assert_eq!(job_state(&control), JobState::Cancelling);
    }

    #[test]
    fn non_scan_jobs_do_not_show_previous_scan_phase_progress() {
        assert!(JobKind::Scan.reports_work());
        assert!(JobKind::Refresh.reports_work());
        assert!(JobKind::Load.reports_work());
        assert!(!JobKind::SaveReport.reports_work());
        assert!(!JobKind::SaveDuplicates.reports_work());
        assert!(!JobKind::FileOperation.reports_work());
        assert!(JobKind::Duplicates.reports_work());
    }

    #[test]
    fn compact_search_keeps_advanced_filters_on_the_search_row() {
        let context = egui::Context::default();
        let creation = eframe::CreationContext::_new_kittest(context.clone());
        let mut app = UniDirStatApp::new(&creation, Vec::new(), None, ScanOptions::default(), None);
        let mut positions = Vec::new();
        for _ in 0..2 {
            let output = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(1700.0, 600.0),
                    )),
                    ..Default::default()
                },
                |context| {
                    egui::CentralPanel::default().show(context, |ui| app.filters(ui));
                },
            );
            positions = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Text(text)
                        if text.galley.text() == tr("Search files and folders...")
                            || text.galley.text() == tr("Advanced filters") =>
                    {
                        Some(text.pos.y)
                    }
                    _ => None,
                })
                .collect();
        }
        assert_eq!(positions.len(), 2);
        assert!(
            (positions[0] - positions[1]).abs() < 8.0,
            "search and filters must share one row: {positions:?}"
        );
    }

    #[test]
    fn saved_eighty_percent_scale_is_applied_without_clamping_up() {
        let fixture = tempfile::tempdir().unwrap();
        let settings = fixture.path().join("settings.json");
        std::fs::write(
            &settings,
            r#"{"font_scale":0.8,"mac_access_reviewed":true}"#,
        )
        .unwrap();
        let context = egui::Context::default();
        let creation = eframe::CreationContext::_new_kittest(context.clone());
        let app = UniDirStatApp::new(
            &creation,
            Vec::new(),
            None,
            ScanOptions::default(),
            Some(settings),
        );
        let _ = context.run(egui::RawInput::default(), |_| {});
        assert_eq!(app.settings.font_scale, 0.8);
        assert!((context.zoom_factor() - 0.8).abs() < 0.001);
    }

    #[test]
    fn old_settings_keep_system_language_and_visible_columns() {
        let settings: Settings =
            serde_json::from_str(r#"{"theme":"Light"}"#).expect("old settings");
        assert_eq!(settings.language, Language::System);
        assert!(settings.show_modified && settings.show_share);
        assert_eq!((settings.font_scale, settings.threads), (0.8, 0));
    }

    #[test]
    fn invalid_portable_settings_are_not_silently_accepted() {
        let fixture = tempfile::tempdir().expect("fixture");
        let path = fixture.path().join("settings.json");
        std::fs::write(&path, b"broken JSON").expect("settings");
        assert!(load_settings(&path).is_err());
    }

    #[test]
    fn portable_settings_limit_reads_and_default_new_files() {
        let fixture = tempfile::tempdir().expect("fixture");
        let path = fixture.path().join("settings.json");
        assert_eq!(
            load_settings(&path).expect("new settings").language,
            Language::System
        );
        let file = std::fs::File::create(&path).expect("settings");
        file.set_len(1024 * 1024 + 1).expect("oversized settings");
        assert!(load_settings(&path).is_err());
    }

    fn contrast(first: Color32, second: Color32) -> f32 {
        let luminance = |color: Color32| {
            let linear = |channel: u8| {
                let channel = f32::from(channel) / 255.0;
                if channel <= 0.04045 {
                    channel / 12.92
                } else {
                    ((channel + 0.055) / 1.055).powf(2.4)
                }
            };
            linear(color.r()) * 0.2126 + linear(color.g()) * 0.7152 + linear(color.b()) * 0.0722
        };
        let first = luminance(first);
        let second = luminance(second);
        (first.max(second) + 0.05) / (first.min(second) + 0.05)
    }

    #[test]
    fn muted_text_is_readable_in_light_and_dark_themes() {
        let context = egui::Context::default();
        for (theme, background) in [
            (egui::Theme::Light, Color32::from_rgb(246, 247, 251)),
            (egui::Theme::Dark, Color32::from_rgb(17, 21, 31)),
        ] {
            context.set_theme(theme);
            let _ = context.run(egui::RawInput::default(), |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    assert!(contrast(muted(ui), background) >= 4.5);
                });
            });
        }
    }

    #[test]
    fn primary_button_has_readable_white_text() {
        assert!(contrast(Color32::WHITE, PRIMARY_BUTTON) >= 4.5);
    }

    #[test]
    fn sidebar_version_follows_the_package_version() {
        let context = egui::Context::default();
        let creation = eframe::CreationContext::_new_kittest(context.clone());
        let mut app = UniDirStatApp::new(&creation, Vec::new(), None, ScanOptions::default(), None);
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(1700.0, 900.0),
                )),
                ..Default::default()
            },
            |context| app.sidebar(context),
        );
        let expected = format!("UniDirStat {} | Rust", env!("CARGO_PKG_VERSION"));
        assert!(output.shapes.iter().any(|shape| {
            matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == expected)
        }));
    }

    #[test]
    fn control_surfaces_follow_the_theme_with_readable_text_and_focus() {
        let context = egui::Context::default();
        let creation = eframe::CreationContext::_new_kittest(context.clone());
        let mut app = UniDirStatApp::new(&creation, Vec::new(), None, ScanOptions::default(), None);
        for theme in [Theme::Dark, Theme::Light, Theme::Dark] {
            app.settings.theme = theme;
            app.apply_theme(&context);
            let visuals = context.style().visuals.clone();
            let widgets = &visuals.widgets;
            if theme == Theme::Dark {
                assert!(widgets.inactive.weak_bg_fill.r() < 40);
                assert!(widgets.inactive.weak_bg_fill.b() > widgets.inactive.weak_bg_fill.r());
            }
            for widget in [
                widgets.inactive,
                widgets.hovered,
                widgets.active,
                widgets.open,
            ] {
                assert!(contrast(widget.text_color(), widget.weak_bg_fill) >= 4.5);
            }
            assert!(contrast(visuals.text_color(), visuals.panel_fill) >= 4.5);
            assert!(contrast(visuals.selection.stroke.color, visuals.selection.bg_fill) >= 4.5);
            assert!(contrast(widgets.active.bg_stroke.color, widgets.active.weak_bg_fill) >= 3.0);
            assert!(contrast(widgets.inactive.bg_stroke.color, visuals.extreme_bg_color) >= 3.0);
        }
    }

    #[test]
    fn semantic_status_text_has_readable_contrast_on_both_surfaces() {
        for (dark, surfaces) in [
            (false, [Color32::WHITE, Color32::from_rgb(246, 247, 251)]),
            (
                true,
                [Color32::from_rgb(25, 30, 43), Color32::from_rgb(17, 21, 31)],
            ),
        ] {
            for surface in surfaces {
                for foreground in [
                    ERROR_TEXT[usize::from(dark)],
                    WARNING_TEXT[usize::from(dark)],
                ] {
                    assert!(contrast(foreground, surface) >= 4.5);
                }
            }
        }
    }

    #[test]
    fn interface_uses_regular_weight_and_readable_default_metrics() {
        let fonts = interface_fonts();
        assert_eq!(
            fonts.families[&egui::FontFamily::Proportional][0],
            "Ubuntu-Regular"
        );
        const {
            assert!(BODY_SIZE >= 16.0 && HELP_SIZE >= 14.0);
            assert!(CONTROL_HEIGHT >= 40.0 && ROW_HEIGHT >= 36.0);
        }
    }

    #[test]
    fn map_tile_labels_have_readable_contrast_in_both_themes() {
        for color in crate::file_colors::FILE_PALETTE {
            for factor in [0.81, 0.93, 1.0] {
                let background = color.gamma_multiply(factor);
                assert!(contrast(tile_text_color(background), background) >= 4.5);
            }
        }
    }

    #[test]
    fn duplicate_rows_preserve_groups_and_saturate_reclaimable_bytes() {
        let report = DuplicateReport {
            groups: vec![
                analysis::DuplicateGroup {
                    size: 4,
                    hash: "first".into(),
                    nodes: vec![3, 8],
                },
                analysis::DuplicateGroup {
                    size: u64::MAX,
                    hash: "second".into(),
                    nodes: vec![1, 2, 7],
                },
            ],
            ..Default::default()
        };

        assert_eq!(
            duplicate_summary(&report),
            (
                vec![
                    DuplicateRow::Group(0),
                    DuplicateRow::Hash(0),
                    DuplicateRow::File(3),
                    DuplicateRow::File(8),
                    DuplicateRow::Group(1),
                    DuplicateRow::Hash(1),
                    DuplicateRow::File(1),
                    DuplicateRow::File(2),
                    DuplicateRow::File(7)
                ],
                u64::MAX,
            )
        );
    }

    #[test]
    fn duplicate_range_selection_skips_group_and_hash_headers() {
        let rows = [
            DuplicateRow::Group(0),
            DuplicateRow::Hash(0),
            DuplicateRow::File(7),
            DuplicateRow::File(3),
            DuplicateRow::Group(1),
            DuplicateRow::File(8),
        ];
        let order = duplicate_file_order(&rows);
        assert_eq!(order, [(7, 0), (3, 0), (8, 0)]);
        let mut selection = Selection::default();
        selection.click(7, &order, false, false);
        selection.click(8, &order, false, true);
        assert_eq!(selection.len(), 3);
    }

    #[test]
    fn storage_buckets_handle_age_boundaries_unknown_dates_and_future_dates() {
        let fixture = tempfile::tempdir().expect("fixture");
        let timestamp = 200 * 86_400;
        for (name, size) in [
            ("recent", 1),
            ("thirty", 2),
            ("old", 4),
            ("unknown", 8),
            ("future", 16),
        ] {
            std::fs::write(fixture.path().join(name), vec![0; size]).expect("file");
        }
        let mut snapshot = scanner::scan(
            &[fixture.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .expect("scan");
        for node in &mut snapshot.nodes {
            node.modified = match node.path.file_name().and_then(|name| name.to_str()) {
                Some("recent") => Some(timestamp - 29 * 86_400),
                Some("thirty") => Some(timestamp - 30 * 86_400),
                Some("old") => Some(timestamp - 180 * 86_400),
                Some("future") => Some(timestamp + 86_400),
                _ => None,
            };
        }

        assert_eq!(
            storage_buckets(&snapshot, timestamp),
            ([17, 2, 4, 8], [2, 1, 1, 1])
        );
    }

    #[test]
    fn scan_progress_distinguishes_logical_and_allocated_bytes() {
        let control = ScanControl::default();
        control.bytes.store(1024 * 1024, Ordering::Relaxed);
        control.allocated_bytes.store(4096, Ordering::Relaxed);
        let progress = job_progress(&control, 1.0, true);
        assert!(progress.contains("1.00 "), "{progress}");
        assert!(progress.contains("4.00 "), "{progress}");
        control.unknown_allocations.store(1, Ordering::Relaxed);
        let progress = job_progress(&control, 1.0, true);
        assert!(progress.contains(tr("Unavailable")), "{progress}");
        assert!(!progress.contains("4.00 "), "{progress}");
        assert!(!job_progress(&control, 1.0, false).contains(tr("Unavailable")));
    }

    #[test]
    fn bytes_are_formatted_without_overflow() {
        assert_eq!(format_bytes(1024), format!("1.00 {}", tr("KiB")));
        assert_eq!(format_bytes(u64::MAX), format!("16.00 {}", tr("EiB")));
    }

    #[test]
    fn theme_defaults_to_system_and_explicit_choice_round_trips() {
        assert!(Settings::default().theme == Theme::System);
        let settings = Settings {
            theme: Theme::Light,
            ..Settings::default()
        };
        let restored: Settings =
            serde_json::from_str(&serde_json::to_string(&settings).expect("settings"))
                .expect("roundtrip");
        assert!(restored.theme == Theme::Light);
    }
}
#[test]
fn navigating_back_to_the_same_file_view_keeps_rows() {
    assert!(!page_needs_rows(Page::Overview, false));
    assert!(!page_needs_rows(Page::Files, false));
    assert!(page_needs_rows(Page::Overview, true));
    assert!(!page_needs_rows(Page::Largest, true));
    assert!(page_needs_rows(Page::Largest, false));
    assert!(page_needs_rows(Page::Files, true));
}

#[test]
fn file_row_background_accepts_secondary_clicks() {
    let context = egui::Context::default();
    let mut position = egui::Pos2::ZERO;
    let mut opened = false;
    for pressed in [None, Some(true), Some(false)] {
        let events = pressed.map_or_else(Vec::new, |pressed| {
            vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Secondary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        });
        let _ = context.run(
            egui::RawInput {
                events,
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    let row = egui::Frame::new().show(ui, |ui| ui.label("file"));
                    position = row.response.rect.center();
                    let response = row.response.interact(Sense::click());
                    assert!(response.sense.senses_click());
                    response.context_menu(|ui| {
                        opened = true;
                        ui.label("context");
                    });
                });
            },
        );
    }
    assert!(
        opened,
        "right click over the file label must open its row menu"
    );
}

#[test]
fn new_preferences_require_access_review_and_allow_reduced_motion() {
    let mut value = serde_json::to_value(Settings::default()).expect("preferences");
    assert_eq!(value["mac_access_reviewed"], false);
    assert_eq!(value["reduced_motion"], false);
    value["mac_access_reviewed"] = true.into();
    value["reduced_motion"] = true.into();
    let restored: Settings = serde_json::from_value(value).expect("preferences");
    let value = serde_json::to_value(restored).expect("preferences");
    assert_eq!(value["mac_access_reviewed"], true);
    assert_eq!(value["reduced_motion"], true);
}
