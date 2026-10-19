use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    sync::{
        Arc, Weak,
        mpsc::{self, Receiver, TryRecvError},
    },
};

use crate::file_colors::{FILE_PALETTE, FileColors};
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
    pub depth_limit: usize,
}

#[derive(Default)]
pub struct MapData {
    pub width: u32,
    pub height: u32,
    pub tiles: Vec<layout::Tile>,
    pub sectors: Vec<layout::Sector>,
    pub frames: Vec<layout::Tile>,
    pub aggregates: Vec<layout::Aggregate>,
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
    colors: Arc<FileColors>,
    context: egui::Context,
}

struct MapTask {
    key: MapKey,
    control: Arc<LayoutControl>,
    receiver: Receiver<Option<MapData>>,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct RasterView {
    pub size: egui::Vec2,
    pub scale: f32,
    pub offset: egui::Vec2,
    pub pixels_per_point: f32,
    pub max_texture_side: usize,
}

struct Raster {
    data: Weak<MapData>,
    view: RasterView,
    texture: egui::TextureHandle,
    visible: Arc<VisibleRegions>,
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct VisibleRegions {
    pub frames: Vec<usize>,
    pub aggregates: Vec<usize>,
    pub tiles: Vec<usize>,
}

#[derive(Debug, PartialEq)]
pub(crate) struct RasterPaint {
    pub texture: egui::TextureId,
    pub rect: egui::Rect,
    pub visible: Arc<VisibleRegions>,
}

struct RasterPixels {
    image: egui::ColorImage,
    visible: VisibleRegions,
}

struct RasterTask {
    data: Weak<MapData>,
    view: RasterView,
    control: Arc<LayoutControl>,
    receiver: Receiver<Option<(egui::TextureHandle, Arc<VisibleRegions>)>>,
}

#[derive(Default)]
pub struct MapWorker {
    raster: Option<Raster>,
    raster_running: Option<RasterTask>,
    colors: Arc<FileColors>,
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
    pub(crate) fn raster(
        &mut self,
        data: &Arc<MapData>,
        snapshot: &Arc<Snapshot>,
        view: RasterView,
        context: &egui::Context,
    ) -> Option<RasterPaint> {
        if let Some(task) = &self.raster_running {
            let result = task.receiver.try_recv();
            if !matches!(result, Err(TryRecvError::Empty))
                && let Some(task) = self.raster_running.take()
                && !task.control.is_cancelled()
                && let Ok(Some((texture, visible))) = result
            {
                self.raster = Some(Raster {
                    data: task.data,
                    view: task.view,
                    texture,
                    visible,
                });
            }
        }
        let identity = Arc::downgrade(data);
        let ready = self
            .raster
            .as_ref()
            .is_some_and(|raster| raster.data.ptr_eq(&identity) && raster.view == view);
        if !ready {
            if let Some(task) = &self.raster_running {
                if !task.data.ptr_eq(&identity) || task.view != view {
                    task.control.cancel();
                }
            } else {
                let control = Arc::new(LayoutControl::default());
                let worker_control = Arc::clone(&control);
                let worker_data = Arc::clone(data);
                let snapshot = Arc::clone(snapshot);
                let colors = Arc::clone(&self.colors);
                let context = context.clone();
                let (sender, receiver) = mpsc::sync_channel(1);
                let spawned =
                    std::thread::Builder::new()
                        .name("map-raster".into())
                        .spawn(move || {
                            let texture = render_raster(
                                &worker_data,
                                &snapshot,
                                &colors,
                                view,
                                &worker_control,
                            )
                            .filter(|_| !worker_control.is_cancelled())
                            .map(|pixels| {
                                let texture = context.load_texture(
                                    "map-raster",
                                    pixels.image,
                                    egui::TextureOptions::NEAREST,
                                );
                                (texture, Arc::new(pixels.visible))
                            });
                            let _ = sender.send(texture);
                            context.request_repaint();
                        });
                match spawned {
                    Ok(_) => {
                        self.raster_running = Some(RasterTask {
                            data: identity.clone(),
                            view,
                            control,
                            receiver,
                        })
                    }
                    Err(error) => self.error = Some(error.to_string()),
                }
            }
        }
        self.raster
            .as_ref()
            .filter(|raster| raster.data.ptr_eq(&identity))
            .map(|raster| {
                let ratio = view.size / raster.view.size * (view.scale / raster.view.scale);
                let rect = egui::Rect::from_min_size(
                    egui::Pos2::ZERO + view.offset - raster.view.offset * ratio,
                    raster.view.size * ratio,
                );
                RasterPaint {
                    texture: raster.texture.id(),
                    rect,
                    visible: Arc::clone(&raster.visible),
                }
            })
    }

    pub(crate) fn raster_pending(&self) -> bool {
        self.raster_running.is_some()
    }

    pub(crate) fn set_colors(&mut self, colors: Arc<FileColors>) {
        if Arc::ptr_eq(&self.colors, &colors) {
            return;
        }
        self.colors = colors;
        self.clear_geometry();
    }

    pub fn request(&mut self, key: MapKey, snapshot: Arc<Snapshot>, context: &egui::Context) {
        self.request_summary(key.scope, Arc::clone(&snapshot), context);
        if self.requested == Some(key) {
            return;
        }
        self.requested = Some(key);
        if !self.ready_key.is_some_and(|ready| {
            ready.scope == key.scope
                && ready.allocated == key.allocated
                && ready.kind == key.kind
                && ready.depth_limit == key.depth_limit
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
            colors: Arc::clone(&self.colors),
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
                let data = compute(
                    &request.snapshot,
                    request.key,
                    &worker_control,
                    &request.colors,
                );
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

    fn clear_geometry(&mut self) {
        self.raster = None;
        if let Some(task) = &self.raster_running {
            task.control.cancel();
        }
        if let Some(task) = &self.running {
            task.control.cancel();
        }
        self.requested = None;
        self.ready = None;
        self.ready_key = None;
        self.queued = None;
        self.cache.clear();
        self.error = None;
    }

    pub fn clear(&mut self) {
        self.clear_geometry();
        self.snapshot = None;
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
        if let Some(task) = &self.raster_running {
            task.control.cancel();
        }
        if let Some(task) = &self.running {
            task.control.cancel();
        }
        if let Some(task) = &self.summary_running {
            task.control.cancel();
        }
    }
}

fn render_raster(
    data: &MapData,
    snapshot: &Snapshot,
    colors: &FileColors,
    view: RasterView,
    control: &LayoutControl,
) -> Option<RasterPixels> {
    // Bound the cache to the viewport, even when geometry is refined for deep zoom.
    let size = [
        (view.size.x * view.pixels_per_point)
            .ceil()
            .clamp(1.0, view.max_texture_side.clamp(1, 4096) as f32) as usize,
        (view.size.y * view.pixels_per_point)
            .ceil()
            .clamp(1.0, view.max_texture_side.clamp(1, 4096) as f32) as usize,
    ];
    let mut image = egui::ColorImage::filled(size, egui::Color32::TRANSPARENT);
    let mut visible = VisibleRegions::default();
    let pixels = egui::vec2(size[0] as f32, size[1] as f32) / view.size;
    let scale = view.size / egui::vec2(data.width.max(1) as f32, data.height.max(1) as f32)
        * view.scale
        * pixels;
    let offset = view.offset * pixels;
    let readable = |rect: layout::Rect| {
        let scale = scale / pixels;
        let rect = egui::Rect::from_min_size(
            egui::Pos2::ZERO + view.offset + egui::vec2(rect.x, rect.y) * scale,
            egui::vec2(rect.width, rect.height) * scale,
        )
        .intersect(egui::Rect::from_min_size(egui::Pos2::ZERO, view.size));
        rect.width() >= 4.0 && rect.height() >= 4.0 && rect.area() >= 24.0
    };
    let bounds = |rect: layout::Rect| {
        [
            (rect.x * scale.x + offset.x).round() as i32,
            (rect.y * scale.y + offset.y).round() as i32,
            ((rect.x + rect.width) * scale.x + offset.x).round() as i32,
            ((rect.y + rect.height) * scale.y + offset.y).round() as i32,
        ]
    };
    for (index, tile) in data.frames.iter().enumerate() {
        if control.is_cancelled() {
            return None;
        }
        if readable(tile.rect) {
            visible.frames.push(index);
        }
    }
    for (index, tile) in data.aggregates.iter().enumerate() {
        if control.is_cancelled() {
            return None;
        }
        if readable(tile.rect) {
            visible.aggregates.push(index);
        }
        raster_rect(
            &mut image,
            bounds(tile.rect),
            FILE_PALETTE[tile.color.min(FILE_PALETTE.len() - 1)],
            true,
        );
    }
    for (index, tile) in data.tiles.iter().enumerate() {
        if control.is_cancelled() {
            return None;
        }
        let rect = bounds(tile.rect);
        if rect[2] <= 0 || rect[3] <= 0 || rect[0] >= size[0] as i32 || rect[1] >= size[1] as i32 {
            continue;
        }
        if readable(tile.rect) {
            visible.tiles.push(index);
        }
        let color = colors.color(&snapshot.nodes[tile.node].extension());
        raster_rect(&mut image, rect, color, true);
    }
    Some(RasterPixels { image, visible })
}

fn raster_rect(image: &mut egui::ColorImage, rect: [i32; 4], fill: egui::Color32, emboss: bool) {
    let [width, height] = image.size;
    let [left, top, right, bottom] = rect;
    let x0 = left.clamp(0, width as i32) as usize;
    let x1 = right.clamp(0, width as i32) as usize;
    let y0 = top.clamp(0, height as i32) as usize;
    let y1 = bottom.clamp(0, height as i32) as usize;
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let edge = if emboss {
        match (right - left).min(bottom - top) {
            8.. => 3,
            5.. => 2,
            3.. => 1,
            _ => 0,
        }
    } else {
        0
    };
    let border = fill.lerp_to_gamma(egui::Color32::BLACK, 0.42);
    let light = [
        border,
        fill.lerp_to_gamma(egui::Color32::WHITE, 0.14),
        fill.lerp_to_gamma(egui::Color32::WHITE, 0.06),
    ];
    let dark = [
        border,
        fill.lerp_to_gamma(egui::Color32::BLACK, 0.18),
        fill.lerp_to_gamma(egui::Color32::BLACK, 0.08),
    ];
    for y in y0..y1 {
        let row = &mut image.pixels[y * width + x0..y * width + x1];
        let from_top = y as i32 - top;
        let from_bottom = bottom - y as i32 - 1;
        let color = if from_top < edge {
            light[from_top as usize]
        } else if from_bottom < edge {
            dark[from_bottom as usize]
        } else {
            fill
        };
        row.fill(color);
        for depth in 0..edge.min(from_top.min(from_bottom) + 1) {
            for (x, color) in [
                (left + depth, light[depth as usize]),
                (right - depth - 1, dark[depth as usize]),
            ] {
                if (x0 as i32..x1 as i32).contains(&x) {
                    row[x as usize - x0] = color;
                }
            }
        }
    }
}

fn compute(
    snapshot: &Snapshot,
    key: MapKey,
    control: &LayoutControl,
    colors: &FileColors,
) -> Option<MapData> {
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
            let geometry = layout::treemap_hierarchy_with_depth(
                snapshot,
                roots,
                key.allocated,
                bounds,
                control,
                key.depth_limit,
                |extension| colors.index(extension),
            )?;
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
            data.tiles = layout::flame_visible_with_depth(
                snapshot,
                roots,
                key.allocated,
                bounds,
                control,
                key.depth_limit,
            )?;
        }
        MapKind::Sunburst => {
            data.sectors = layout::sunburst_visible_with_depth(
                snapshot,
                roots,
                key.allocated,
                bounds,
                control,
                key.depth_limit,
            )?;
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

    #[test]
    fn embossed_neighbors_cover_the_canvas_without_gaps_or_gloss() {
        let fill = FILE_PALETTE[0];
        let mut image = egui::ColorImage::filled([24, 12], egui::Color32::TRANSPARENT);
        raster_rect(&mut image, [0, 0, 12, 12], fill, true);
        raster_rect(&mut image, [12, 0, 24, 12], fill, true);
        assert!(image.pixels.iter().all(|pixel| pixel.a() == 255));
        assert_eq!(image[(4, 4)], fill);
        assert_eq!(image[(8, 8)], fill);
        assert!(image[(11, 6)].r() < image[(10, 6)].r());
        assert!(image[(12, 6)].r() < fill.r());
        assert!(image[(1, 6)].r() > image[(2, 6)].r());
        assert!(image[(2, 6)].r() > fill.r());
        assert!(image[(10, 6)].r() < image[(9, 6)].r());
        assert!(image[(9, 6)].r() < fill.r());
    }

    #[test]
    fn clipped_emboss_does_not_add_edges_at_the_viewport_boundary() {
        let fill = FILE_PALETTE[0];
        let mut image = egui::ColorImage::filled([3, 3], egui::Color32::TRANSPARENT);
        raster_rect(&mut image, [-2, -2, 5, 5], fill, true);
        assert!(image.pixels.iter().all(|pixel| *pixel == fill));
        raster_rect(&mut image, [1, 1, 2, 2], FILE_PALETTE[1], true);
        assert_eq!(image[(1, 1)], FILE_PALETTE[1]);
    }

    #[test]
    fn tiny_tiles_keep_color_and_opaque_coverage() {
        for side in 1..=4 {
            let mut image = egui::ColorImage::filled([side, side], egui::Color32::TRANSPARENT);
            raster_rect(
                &mut image,
                [0, 0, side as i32, side as i32],
                FILE_PALETTE[0],
                true,
            );
            assert!(image.pixels.iter().all(|pixel| pixel.a() == 255));
            assert!(image.pixels.contains(&FILE_PALETTE[0]));
        }
    }

    #[test]
    fn raster_stays_viewport_sized_at_deep_zoom() {
        let data = MapData {
            width: 64_000,
            height: 32_000,
            ..MapData::default()
        };
        let image = render_raster(
            &data,
            &snapshot(),
            &FileColors::default(),
            RasterView {
                size: egui::vec2(1000.0, 500.0),
                scale: 64.0,
                offset: egui::vec2(-2000.0, -1000.0),
                pixels_per_point: 1.0,
                max_texture_side: 2048,
            },
            &LayoutControl::default(),
        )
        .unwrap()
        .image;
        assert_eq!(image.size, [1000, 500]);
        assert_eq!(
            image.pixels.len() * std::mem::size_of::<egui::Color32>(),
            2_000_000
        );
    }

    #[test]
    fn raster_caches_only_readable_viewport_indices_and_replaces_them_after_pan() {
        let data = MapData {
            width: 10_000,
            height: 1000,
            tiles: (0..100_000)
                .map(|index| layout::Tile {
                    node: 1,
                    rect: layout::Rect {
                        x: (index % 1000) as f32 * 10.0,
                        y: (index / 1000) as f32 * 10.0,
                        width: 10.0,
                        height: 10.0,
                    },
                })
                .collect(),
            ..MapData::default()
        };
        let view = RasterView {
            size: egui::vec2(100.0, 100.0),
            scale: 64.0,
            offset: egui::Vec2::ZERO,
            pixels_per_point: 1.0,
            max_texture_side: 2048,
        };
        let first = render_raster(
            &data,
            &snapshot(),
            &FileColors::default(),
            view,
            &LayoutControl::default(),
        )
        .unwrap();
        let panned = render_raster(
            &data,
            &snapshot(),
            &FileColors::default(),
            RasterView {
                offset: egui::vec2(-100.0, -100.0),
                ..view
            },
            &LayoutControl::default(),
        )
        .unwrap();
        assert!(!first.visible.tiles.is_empty());
        assert!(first.visible.tiles.len() < 100);
        assert!(panned.visible.tiles.len() < 100);
        assert_ne!(first.visible.tiles, panned.visible.tiles);
        assert_eq!(first.image.pixels.len(), 10_000);
        let cancelled = LayoutControl::default();
        cancelled.cancel();
        assert!(
            render_raster(&data, &snapshot(), &FileColors::default(), view, &cancelled).is_none()
        );
    }

    #[test]
    fn dense_same_color_files_render_as_distinct_gapless_blocks() {
        let mut snapshot = snapshot();
        let template = snapshot.nodes[1].clone();
        let count = 35_701;
        let nodes = &mut Arc::make_mut(&mut snapshot).nodes;
        *nodes = std::iter::once(Node {
            kind: EntryKind::Directory,
            children: (1..=count).collect(),
            logical_size: count as u64,
            ..template.clone()
        })
        .chain((1..=count).map(|_| Node {
            parent: Some(0),
            logical_size: 1,
            children: Vec::new(),
            ..template.clone()
        }))
        .collect();
        let start = Instant::now();
        let data = compute(
            &snapshot,
            MapKey {
                width: 1000,
                height: 500,
                ..key()
            },
            &LayoutControl::default(),
            &FileColors::default(),
        )
        .unwrap();
        let layout_time = start.elapsed();
        let start = Instant::now();
        let image = render_raster(
            &data,
            &snapshot,
            &FileColors::default(),
            RasterView {
                size: egui::vec2(1000.0, 500.0),
                scale: 1.0,
                offset: egui::Vec2::ZERO,
                pixels_per_point: 1.0,
                max_texture_side: 2048,
            },
            &LayoutControl::default(),
        )
        .unwrap()
        .image;
        eprintln!(
            "dense map: {} tiles, layout {layout_time:?}, raster {:?}, {} bytes",
            data.tiles.len(),
            start.elapsed(),
            image.pixels.len() * 4
        );
        assert_eq!(data.tiles.len(), count);
        assert!(image.pixels.iter().all(|pixel| pixel.a() == 255));
        assert!(
            image
                .pixels
                .windows(2)
                .filter(|pair| pair[0] != pair[1])
                .count()
                > count
        );
    }

    #[test]
    fn lower_ranked_extensions_keep_color_in_a_scoped_gapless_map() {
        let mut snapshot = snapshot();
        let nodes = &mut Arc::make_mut(&mut snapshot).nodes;
        nodes[1].path = PathBuf::from("root/a.type17").into();
        nodes[2].path = PathBuf::from("root/b.type18").into();
        let colors = FileColors::from_extensions(
            &(0..19)
                .map(|rank| ExtensionStats {
                    extension: format!("type{rank:02}"),
                    bytes: 100 - rank,
                    files: 1,
                })
                .collect::<Vec<_>>(),
        );
        let data = compute(&snapshot, key(), &LayoutControl::default(), &colors).unwrap();
        let image = render_raster(
            &data,
            &snapshot,
            &colors,
            RasterView {
                size: egui::vec2(100.0, 50.0),
                scale: 1.0,
                offset: egui::Vec2::ZERO,
                pixels_per_point: 1.0,
                max_texture_side: 2048,
            },
            &LayoutControl::default(),
        )
        .unwrap()
        .image;
        assert!(image.pixels.iter().all(|pixel| pixel.a() == 255));
        assert!(image.pixels.contains(&FILE_PALETTE[0]));
        assert!(image.pixels.contains(&FILE_PALETTE[1]));
    }

    #[test]
    fn treemap_depth_limit_keeps_a_colored_folder_region_and_can_reveal_its_files() {
        let mut snapshot = snapshot();
        let template = snapshot.nodes[1].clone();
        Arc::make_mut(&mut snapshot).nodes = (0_usize..10)
            .map(|index| Node {
                path: PathBuf::from(format!("root/{index}.txt")).into(),
                parent: index.checked_sub(1),
                children: if index < 9 { vec![index + 1] } else { vec![] },
                kind: if index < 9 {
                    EntryKind::Directory
                } else {
                    EntryKind::File
                },
                ..template.clone()
            })
            .collect();
        let colors = FileColors::from_extensions(&[ExtensionStats {
            extension: "txt".into(),
            bytes: 100,
            files: 1,
        }]);
        let data = compute(&snapshot, key(), &LayoutControl::default(), &colors).unwrap();
        assert!(data.tiles.is_empty());
        assert!(data.frame(6).is_none());
        assert_eq!(data.aggregates.len(), 1);
        let folder = &data.aggregates[0];
        assert_eq!((folder.owner, folder.color, folder.count), (Some(5), 0, 1));
        assert_eq!(folder.rect, data.frame(5).unwrap().rect);
        let image = render_raster(
            &data,
            &snapshot,
            &colors,
            RasterView {
                size: egui::vec2(120.0, 80.0),
                scale: 1.0,
                offset: egui::Vec2::ZERO,
                pixels_per_point: 1.0,
                max_texture_side: 2048,
            },
            &LayoutControl::default(),
        )
        .unwrap()
        .image;
        assert!(image.pixels.iter().all(|pixel| pixel.a() == 255));
        assert_eq!(image[(60, 40)], FILE_PALETTE[0]);
        assert!(image[(0, 40)].r() < FILE_PALETTE[0].r());
        let detail = compute(
            &snapshot,
            MapKey {
                scope: Some(5),
                ..key()
            },
            &LayoutControl::default(),
            &colors,
        )
        .unwrap();
        assert_eq!(detail.tiles.len(), 1);
        assert_eq!(detail.tiles[0].node, 9);
        for kind in [MapKind::Flame, MapKind::Sunburst] {
            let data = compute(
                &snapshot,
                MapKey {
                    width: 1000,
                    height: 1000,
                    kind,
                    ..key()
                },
                &LayoutControl::default(),
                &colors,
            )
            .unwrap();
            let nodes: Vec<_> = data
                .tiles
                .iter()
                .map(|tile| tile.node)
                .chain(data.sectors.iter().map(|sector| sector.node))
                .collect();
            assert_eq!(nodes, [0, 1, 2, 3, 4, 5]);
        }
    }

    #[test]
    fn changing_depth_hides_the_previous_map_and_rebuilds_its_geometry() {
        let context = egui::Context::default();
        let snapshot = snapshot();
        let mut worker = MapWorker::default();
        worker.request(key(), Arc::clone(&snapshot), &context);
        finish(&mut worker);
        assert_eq!(worker.data().unwrap().tiles.len(), 2);
        worker.request(
            MapKey {
                depth_limit: 1,
                ..key()
            },
            snapshot,
            &context,
        );
        assert!(worker.data().is_none());
        finish(&mut worker);
        assert_eq!(worker.data().unwrap().aggregates.len(), 1);
        assert!(worker.data().unwrap().tiles.is_empty());
    }

    #[test]
    fn unchanged_view_reuses_one_cached_texture_and_clear_releases_it() {
        let snapshot = snapshot();
        let data = Arc::new(
            compute(
                &snapshot,
                key(),
                &LayoutControl::default(),
                &FileColors::default(),
            )
            .unwrap(),
        );
        let context = egui::Context::default();
        let mut worker = MapWorker::default();
        let view = RasterView {
            size: egui::vec2(100.0, 50.0),
            scale: 1.0,
            offset: egui::Vec2::ZERO,
            pixels_per_point: 1.0,
            max_texture_side: 2048,
        };
        let deadline = Instant::now() + Duration::from_secs(3);
        let first = loop {
            if let Some(paint) = worker.raster(&data, &snapshot, view, &context) {
                break paint;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        };
        assert_eq!(worker.raster(&data, &snapshot, view, &context), Some(first));
        assert!(worker.raster_running.is_none());
        worker.clear();
        assert!(worker.raster.is_none());
    }

    fn snapshot() -> Arc<Snapshot> {
        Arc::new(Snapshot {
            version: 1,
            roots: vec![0],
            nodes: (0..3)
                .map(|index| Node {
                    path: PathBuf::from(format!("node-{index}")).into(),
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
                    scan_failed: false,
                    scan_warning: false,
                    depth_limited: false,
                    scan_duration_ns: 0,
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
            depth_limit: 6,
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
    fn map_colors_follow_global_logical_ranks_across_scope_and_metric_changes() {
        let mut snapshot = snapshot();
        let nodes = &mut Arc::make_mut(&mut snapshot).nodes;
        nodes[1].path = PathBuf::from("root/inside.a").into();
        nodes[2].path = PathBuf::from("root/outside.b").into();
        let context = egui::Context::default();
        let colors = Arc::new(FileColors::from_extensions(&[
            ExtensionStats {
                extension: "a".into(),
                bytes: 60,
                files: 1,
            },
            ExtensionStats {
                extension: "b".into(),
                bytes: 40,
                files: 1,
            },
        ]));
        let mut worker = MapWorker::default();
        worker.set_colors(Arc::clone(&colors));
        for (scope, allocated, expected) in [
            (None, false, vec![("a", 0), ("b", 1)]),
            (Some(2), false, vec![("b", 1)]),
            (None, true, vec![("a", 0), ("b", 1)]),
        ] {
            worker.request(
                MapKey {
                    scope,
                    allocated,
                    width: 1,
                    height: 1,
                    ..key()
                },
                Arc::clone(&snapshot),
                &context,
            );
            finish(&mut worker);
            let data = worker.data().unwrap();
            let groups = &data.aggregates;
            for (extension, color) in expected {
                assert!(
                    groups
                        .iter()
                        .any(|group| group.extensions == [extension] && group.color == color)
                        || data.tiles.iter().any(|tile| {
                            let tile_extension = snapshot.nodes[tile.node].extension();
                            tile_extension == extension && colors.index(&tile_extension) == color
                        }),
                    "{extension} should retain color {color}: {groups:?}"
                );
            }
            let ready = Arc::clone(worker.data().unwrap());
            worker.set_colors(Arc::clone(&colors));
            assert!(Arc::ptr_eq(&ready, worker.data().unwrap()));
        }
    }

    #[test]
    fn replacing_colors_invalidates_cached_and_finished_map_results_but_clear_preserves_colors() {
        let mut snapshot = snapshot();
        let nodes = &mut Arc::make_mut(&mut snapshot).nodes;
        nodes[1].path = PathBuf::from("root/inside.a").into();
        nodes[2].path = PathBuf::from("root/outside.b").into();
        let context = egui::Context::default();
        let mut worker = MapWorker::default();
        worker.request(
            MapKey {
                width: 1,
                height: 1,
                ..key()
            },
            Arc::clone(&snapshot),
            &context,
        );
        finish(&mut worker);
        finish_summary(&mut worker);
        let previous = Arc::clone(worker.data().unwrap());
        let summary = Arc::clone(worker.summary().unwrap());
        let colors = Arc::new(FileColors::from_extensions(&[
            ExtensionStats {
                extension: "b".into(),
                bytes: 60,
                files: 1,
            },
            ExtensionStats {
                extension: "a".into(),
                bytes: 40,
                files: 1,
            },
        ]));
        worker.set_colors(Arc::clone(&colors));
        assert!(worker.data().is_none());
        assert!(worker.cache.is_empty());
        assert!(Arc::ptr_eq(&summary, worker.summary().unwrap()));
        worker.clear();
        worker.request(
            MapKey {
                width: 1,
                height: 1,
                ..key()
            },
            snapshot,
            &context,
        );
        finish(&mut worker);
        assert!(!Arc::ptr_eq(&previous, worker.data().unwrap()));
        let groups = &worker.data().unwrap().aggregates;
        assert!(
            groups
                .iter()
                .any(|group| group.extensions == ["b"] && group.color == 0)
        );
    }

    #[test]
    fn a_finished_job_with_old_colors_cannot_fill_the_same_requested_key() {
        let mut snapshot = snapshot();
        let nodes = &mut Arc::make_mut(&mut snapshot).nodes;
        nodes[1].path = PathBuf::from("root/inside.a").into();
        nodes[2].path = PathBuf::from("root/outside.b").into();
        let context = egui::Context::default();
        let old_colors = FileColors::from_extensions(&[
            ExtensionStats {
                extension: "a".into(),
                bytes: 60,
                files: 1,
            },
            ExtensionStats {
                extension: "b".into(),
                bytes: 40,
                files: 1,
            },
        ]);
        let colors = Arc::new(FileColors::from_extensions(&[
            ExtensionStats {
                extension: "b".into(),
                bytes: 60,
                files: 1,
            },
            ExtensionStats {
                extension: "a".into(),
                bytes: 40,
                files: 1,
            },
        ]));
        let key = MapKey {
            width: 1,
            height: 1,
            ..key()
        };
        let control = Arc::new(LayoutControl::default());
        let (sender, receiver) = mpsc::sync_channel(1);
        sender
            .send(compute(
                &snapshot,
                key,
                &LayoutControl::default(),
                &old_colors,
            ))
            .unwrap();
        let mut worker = MapWorker::default();
        worker.snapshot = Some(Arc::downgrade(&snapshot));
        worker.requested = Some(key);
        worker.running = Some(MapTask {
            key,
            control: Arc::clone(&control),
            receiver,
        });
        worker.set_colors(colors);
        assert!(control.is_cancelled());
        worker.request(key, snapshot, &context);
        finish(&mut worker);
        let groups = &worker.data().unwrap().aggregates;
        assert!(
            groups
                .iter()
                .any(|group| group.extensions == ["b"] && group.color == 0)
        );
        assert!(
            groups
                .iter()
                .any(|group| group.extensions == ["a"] && group.color == 1)
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
        inside.path = PathBuf::from("root/folder/inside.txt").into();
        inside.parent = Some(1);
        nodes[1].kind = EntryKind::Directory;
        nodes[1].children = vec![3];
        nodes[2].path = PathBuf::from("root/outside.bin").into();
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

        let data = compute(
            &snapshot,
            key(),
            &LayoutControl::default(),
            &FileColors::default(),
        )
        .unwrap();

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
