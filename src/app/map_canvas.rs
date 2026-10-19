use super::workspace::{info_button, paint_label};
use super::*;
use crate::file_colors::FILE_PALETTE;

const MAP_LABEL_SIZE: f32 = 14.0;
const MAP_LABEL_HEIGHT: f32 = MAP_LABEL_SIZE * 1.5;

fn readable_map_region(rect: egui::Rect) -> bool {
    rect.width() >= 4.0 && rect.height() >= 4.0 && rect.area() >= 24.0
}

fn tile_shape(rect: egui::Rect) -> (egui::Rect, f32) {
    (rect, 0.0)
}

fn paint_map_label(ui: &egui::Ui, rect: egui::Rect, text: &str, color: Color32) {
    if rect.width() >= MAP_LABEL_SIZE * 3.0 && rect.height() >= MAP_LABEL_HEIGHT {
        paint_label(ui, rect, text, color, MAP_LABEL_SIZE);
    }
}

fn paint_map_focus(ui: &egui::Ui, rect: egui::Rect) {
    ui.painter().rect_stroke(
        rect.shrink(1.0),
        0.0,
        Stroke::new(4.0_f32, Color32::WHITE),
        StrokeKind::Inside,
    );
    ui.painter().rect_stroke(
        rect.shrink(2.0),
        0.0,
        Stroke::new(2.0_f32, Color32::BLACK),
        StrokeKind::Inside,
    );
}

fn base_rect(rect: unidirstat::layout::Rect, data: &MapData, size: Vec2) -> egui::Rect {
    let ratio = size / Vec2::new(data.width.max(1) as f32, data.height.max(1) as f32);
    egui::Rect::from_min_size(
        egui::Pos2::ZERO + Vec2::new(rect.x, rect.y) * ratio,
        Vec2::new(rect.width, rect.height) * ratio,
    )
}

fn selected_region(selection: &Selection, snapshot: &Snapshot, mut index: usize) -> bool {
    loop {
        if selection.contains(index) {
            return true;
        }
        let Some(parent) = snapshot.nodes[index].parent else {
            return false;
        };
        index = parent;
    }
}

fn map_resolution(view: MapView, size: Vec2, detail: u32) -> (u32, u32) {
    let vertical_detail = if view == MapView::Flame { 1 } else { detail };
    (
        (size.x * detail as f32).round().max(1.0) as u32,
        (size.y * vertical_detail as f32).round().max(1.0) as u32,
    )
}

fn sunburst_keyboard_target(
    response: &egui::Response,
    sectors: &[unidirstat::layout::Sector],
    selected: Option<usize>,
    hovered: Option<usize>,
) -> Option<(usize, bool)> {
    if !response.has_focus() || sectors.is_empty() {
        return None;
    }
    let cursor_id = response.id.with("sector-cursor");
    let saved = response
        .ctx
        .data_mut(|data| data.get_temp::<(usize, bool)>(cursor_id));
    let current = if response.gained_focus() {
        selected
    } else {
        saved.map(|(index, _)| index).or(selected)
    };
    let mut position = sectors
        .iter()
        .position(|sector| Some(sector.node) == current)
        .or_else(|| {
            sectors
                .iter()
                .position(|sector| Some(sector.node) == selected)
        })
        .unwrap_or(0);
    let (previous, next) = response.ctx.input_mut(|input| {
        (
            input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft)
                || input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
            input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight)
                || input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
        )
    });
    if previous || next {
        response
            .ctx
            .memory_mut(|memory| memory.move_focus(egui::FocusDirection::None));
    }
    let keyboard_click = response.clicked() && !response.clicked_by(egui::PointerButton::Primary);
    let mut keyboard = saved.is_none_or(|(_, keyboard)| keyboard)
        || response.gained_focus()
        || keyboard_click
        || previous
        || next;
    if previous {
        position = position.checked_sub(1).unwrap_or(sectors.len() - 1);
    } else if next {
        position = (position + 1) % sectors.len();
    } else if !keyboard_click
        && (response.clicked_by(egui::PointerButton::Primary)
            || response.secondary_clicked()
            || response
                .ctx
                .input(|input| input.pointer.delta() != Vec2::ZERO))
        && let Some(hovered) = hovered
        && let Some(hovered_position) = sectors.iter().position(|sector| sector.node == hovered)
    {
        position = hovered_position;
        keyboard = false;
    }
    let node = sectors[position].node;
    response
        .ctx
        .data_mut(|data| data.insert_temp(cursor_id, (node, keyboard)));
    Some((node, keyboard))
}

impl UniDirStatApp {
    pub(super) fn map_canvas(&mut self, ui: &mut egui::Ui, snapshot: &Arc<Snapshot>, height: f32) {
        let frame = card(ui).inner_margin(12);
        frame.show(ui, |ui| {
            let bottom = ui.cursor().top() + (height - 24.0).max(1.0);
            ui.horizontal_wrapped(|ui| {
                let heading = ui.strong(tr("Storage map"));
                let help = tr("Wheel zooms at the pointer; drag to pan. Double-click a folder to focus it. Cmd/Ctrl toggles selection. Use Up or Reset to return.");
                if self.settings.map_view == MapView::Sunburst {
                    info_button(ui, &format!("{help}\n{}", tr("Use arrow keys to choose a segment, then Enter or Space to select it.")));
                } else {
                    info_button(ui, help);
                }
                egui::ComboBox::from_id_salt("map_view").width(130.0).selected_text(match self.settings.map_view { MapView::Treemap => tr("Treemap"), MapView::Flame => tr("Flame graph"), MapView::Sunburst => tr("Sunburst") }).show_ui(ui, |ui| {
                    for (view, label) in [(MapView::Treemap, tr("Treemap")), (MapView::Flame, tr("Flame graph")), (MapView::Sunburst, tr("Sunburst"))] {
                        if ui.selectable_value(&mut self.settings.map_view, view, label).changed() { self.map_viewport = MapViewport::default(); self.map_focus = None; }
                    }
                }).response.labelled_by(heading.id);
                if ui.add_enabled(self.scope.is_some(), egui::Button::new(tr("Zoom out"))).clicked() {
                    self.scope = self.scope.and_then(|index| snapshot.nodes[index].parent);
                    self.map_viewport = MapViewport::default(); self.map_focus = None;
                }
                if ui.button(tr("Reset")).clicked() {
                    self.scope = None; self.map_focus = None; self.map_viewport.reset();
                    self.map_worker.clear();
                }
                if ui.add_enabled(self.selection.primary().is_some(), egui::Button::new(tr("Zoom to selection"))).clicked() {
                    self.map_focus = self.selection.primary().and_then(|index| if snapshot.nodes[index].kind == EntryKind::Directory { Some(index) } else { snapshot.nodes[index].parent });
                }
                ui.label(format!("{:.0}%", self.map_viewport.scale * 100.0));
                let count = trf("{count} visible entries", &[("count", self.rows.len().to_string())]);
                let count_width = ui.fonts_mut(|fonts| fonts.layout_no_wrap(count.clone(), FontId::proportional(BODY_SIZE), ui.visuals().text_color()).size().x);
                let remaining = ui.available_size_before_wrap().x;
                ui.allocate_ui_with_layout(Vec2::new(remaining.max(count_width), CONTROL_HEIGHT), Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(count).size(BODY_SIZE));
                });
            });
            ui.add(egui::Label::new(self.scope.map_or_else(|| tr("All scanned locations").to_owned(), |index| display_path(&snapshot.nodes[index].path))).truncate());
            if let Some(error) = self.map_worker.error() {
                ui.colored_label(ui.visuals().error_fg_color, trf("Error: {details}", &[("details", error.into())]));
                ui.label(tr("Reset the map to try again."));
            }
            let (bounds, _) = ui.allocate_exact_size(Vec2::new(ui.available_width().max(1.0), (bottom - ui.cursor().top()).max(1.0)), Sense::click_and_drag().difference(Sense::focusable_noninteractive()));
            let input = ui.input(|input| (input.pointer.hover_pos(), input.smooth_scroll_delta.y, input.zoom_delta(), input.pointer.delta(), input.pointer.primary_down() || input.pointer.middle_down(), input.pointer.press_origin(), input.pointer.is_decidedly_dragging(), input.stable_dt));
            if let Some(pointer) = input.0.filter(|pointer| bounds.contains(*pointer)) {
                let factor = ((input.1 * 0.003).exp() * input.2).clamp(0.5, 2.0);
                if (factor - 1.0).abs() > 0.0001 {
                    self.map_focus = None;
                    self.map_viewport.zoom_at(pointer - bounds.min, factor, bounds.size());
                    ui.ctx().input_mut(|input| { input.smooth_scroll_delta = Vec2::ZERO; input.raw_scroll_delta = Vec2::ZERO; });
                }
            }
            if input.4 && input.6 && input.5.is_some_and(|origin| bounds.contains(origin)) {
                self.map_focus = None; self.map_viewport.pan(input.3, bounds.size());
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            }
            let detail = self.map_viewport.detail();
            let (width, height) = map_resolution(self.settings.map_view, bounds.size(), detail);
            let key = MapKey { scope: self.scope, allocated: self.settings.allocated, width, height, depth_limit: self.settings.map_depth, kind: match self.settings.map_view { MapView::Treemap => MapKind::Treemap, MapView::Flame => MapKind::Flame, MapView::Sunburst => MapKind::Sunburst } };
            self.map_worker.request(key, Arc::clone(snapshot), ui.ctx());
            let data = self.map_worker.data().map(Arc::clone);
            if let (Some(index), Some(data)) = (self.map_focus, data.as_ref()) {
                if let Some(tile) = data.frame(index).or_else(|| data.tiles.iter().find(|tile| tile.node == index).copied()) {
                    self.map_viewport.zoom_to(base_rect(tile.rect, data, bounds.size()), bounds.size());
                } else {
                    self.map_viewport.zoom_at(bounds.size() / 2.0, if self.map_viewport.scale < 1.01 { 2.0 } else { 1.0 }, bounds.size());
                }
            }
            let moving = self.map_viewport.tick(bounds.size(), input.7, self.settings.reduced_motion);
            if moving { ui.ctx().request_repaint(); }
            let painter = ui.painter().with_clip_rect(bounds.intersect(ui.clip_rect()));
            painter.rect_filled(bounds, 5.0, ui.visuals().extreme_bg_color);
            if let Some(data) = data.as_ref() {
                if self.settings.map_view == MapView::Sunburst { self.sunburst_canvas(ui, snapshot, bounds, data); }
                else {
                    let raster = self.map_worker.raster(data, snapshot, crate::map_job::RasterView {
                        size: bounds.size(), scale: self.map_viewport.scale, offset: self.map_viewport.offset,
                        pixels_per_point: ui.ctx().pixels_per_point(),
                        max_texture_side: ui.input(|input| input.max_texture_side),
                    }, ui.ctx());
                    let direct = raster.is_none() && data.frames.len() + data.aggregates.len() + data.tiles.len() <= 4096;
                    let mut focused = None;
                    let frame_indices = raster.as_ref().map_or(&[][..], |raster| raster.visible.frames.as_slice());
                    for tile in frame_indices.iter().map(|&index| &data.frames[index]).chain(data.frames[..if direct { data.frames.len() } else { 0 }].iter()) {
                        let rect = self.map_viewport.rect(base_rect(tile.rect, data, bounds.size()), bounds.min);
                        if !bounds.intersects(rect) || !readable_map_region(rect.intersect(bounds)) { continue; }
                        painter.rect_filled(rect, 2.0, ui.visuals().widgets.inactive.bg_fill);
                        painter.rect_stroke(rect, 2.0, Stroke::new(0.6_f32, muted(ui).gamma_multiply(0.5)), StrokeKind::Inside);
                        let response = ui.interact(rect.intersect(bounds), ui.id().with(("directory-region", tile.node)), Sense::click());
                        focused = self.map_item(ui, response, tile.node, snapshot, base_rect(tile.rect, data, bounds.size()), bounds.size()).or(focused);
                    }
                    if let Some(raster) = &raster {
                        painter.image(raster.texture, raster.rect.translate(bounds.min.to_vec2()), egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), Color32::WHITE);
                    }
                    let aggregate_indices = raster.as_ref().map_or(&[][..], |raster| raster.visible.aggregates.as_slice());
                    for tile in aggregate_indices.iter().map(|&index| &data.aggregates[index]).chain(data.aggregates[..if direct { data.aggregates.len() } else { 0 }].iter()) {
                        let base = base_rect(tile.rect, data, bounds.size());
                        let rect = self.map_viewport.rect(base, bounds.min);
                        if !bounds.intersects(rect) { continue; }
                        let fill = FILE_PALETTE[tile.color.min(FILE_PALETTE.len() - 1)];
                        if raster.is_none() { painter.rect_filled(rect, 0.0, fill); }
                        if !readable_map_region(rect.intersect(bounds)) { continue; }
                        if tile.owner.is_some_and(|index| selected_region(&self.selection, snapshot, index)) {
                            painter.rect_stroke(rect.shrink(1.0), 0.0, Stroke::new(2.0_f32, ui.visuals().text_color()), StrokeKind::Inside);
                        }
                        let size = u64::try_from(tile.bytes).map_or_else(|_| format!("{} {}", tile.bytes, tr("B")), format_bytes);
                        let summary = if let Some(index) = tile.owner {
                            trf("{folder}/ · {count} files · {size}", &[("folder", snapshot.nodes[index].name().trim_end_matches(std::path::is_separator).to_owned()), ("count", tile.count.to_string()), ("size", size)])
                        } else {
                            trf("{count} small files · {size}", &[("count", tile.count.to_string()), ("size", size)])
                        };
                        let mut extension_names = tile.extensions.iter().take(8).map(String::as_str).collect::<Vec<_>>().join(", ");
                        if tile.extensions.len() > 8 {
                            extension_names = trf("{extensions}, and {count} more", &[("extensions", extension_names), ("count", (tile.extensions.len() - 8).to_string())]);
                        }
                        let extensions = trf("File types: {extensions}", &[("extensions", extension_names)]);
                        let owner = tile.owner.map(|index| trf("Containing folder: {path}", &[("path", display_path(&snapshot.nodes[index].path))]));
                        if self.settings.labels { paint_map_label(ui, rect.intersect(bounds).shrink(3.0), &summary, tile_text_color(fill)); }
                        let response = ui.interact(rect.intersect(bounds), ui.id().with(("aggregate", tile.owner, tile.color, tile.rect.x.to_bits(), tile.rect.y.to_bits())), Sense::click());
                        if response.clicked() && let Some(index) = tile.owner {
                            let modifiers = click_modifiers(&response.ctx);
                            self.select_aggregate_owner(index, snapshot, modifiers.command || modifiers.ctrl);
                        }
                        if response.clicked() && tile.owner.is_none() {
                            self.map_viewport.zoom_at(rect.center() - bounds.min, 2.0, bounds.size());
                            ui.ctx().request_repaint();
                        }
                        if response.double_clicked() && let Some(index) = tile.owner {
                            self.map_viewport.zoom_to(base, bounds.size());
                            self.map_focus = Some(index);
                            ui.ctx().request_repaint();
                        }
                        let action = if tile.owner.is_some() { tr("Select containing folder") } else { tr("Zoom to selection") };
                        response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, response.enabled(), tile.owner.is_some_and(|index| self.selection.contains(index)), format!("{summary}. {extensions}. {}. {action}", owner.as_deref().unwrap_or_default())));
                        if response.has_focus() { focused = Some(response.rect); }
                        response.context_menu(|ui| {
                            if let Some(index) = tile.owner {
                                if ui.button(tr("Select containing folder")).clicked() {
                                    self.select_aggregate_owner(index, snapshot, false);
                                    ui.close();
                                }
                                if ui.button(tr("Zoom into folder")).clicked() {
                                    self.map_focus = Some(index);
                                    ui.close();
                                }
                            }
                        });
                        response.on_hover_ui(|ui| {
                            ui.label(&summary);
                            ui.label(&extensions);
                            if let Some(owner) = &owner { ui.label(owner); }
                            ui.label(action);
                        });
                    }
                    let tile_indices = raster.as_ref().map_or(&[][..], |raster| raster.visible.tiles.as_slice());
                    for tile in tile_indices.iter().map(|&index| &data.tiles[index]).chain(data.tiles[..if direct { data.tiles.len() } else { 0 }].iter()) {
                        let base = base_rect(tile.rect, data, bounds.size());
                        let rect = self.map_viewport.rect(base, bounds.min);
                        if !bounds.intersects(rect) { continue; }
                        let readable = readable_map_region(rect.intersect(bounds));
                        if !readable && raster.is_some() { continue; }
                        let node = &snapshot.nodes[tile.node];
                        let color = self.file_colors.color(&node.extension());
                        let fill = Color32::from_rgb(color.r(), color.g(), color.b());
                        if !readable {
                            painter.rect_filled(rect, 0.0, fill);
                            continue;
                        }
                        let (tile_rect, radius) = tile_shape(rect);
                        let response = ui.interact(tile_rect.intersect(bounds), ui.id().with(("tile", tile.node)), Sense::click());
                        if raster.is_none() { painter.rect_filled(tile_rect, radius, fill); }
                        if selected_region(&self.selection, snapshot, tile.node) { painter.rect_stroke(tile_rect.shrink(1.0), radius, Stroke::new(2.0_f32, ui.visuals().text_color()), StrokeKind::Inside); }
                        if self.settings.labels && tile_rect.height() >= MAP_LABEL_HEIGHT + 2.0 && tile_rect.width() >= MAP_LABEL_SIZE * 3.0 + 8.0 {
                            let label = tile_rect.intersect(bounds).shrink2(Vec2::new(4.0, 1.0));
                            paint_map_label(ui, egui::Rect::from_min_size(label.min, Vec2::new(label.width(), MAP_LABEL_HEIGHT)), &node.name(), tile_text_color(fill));
                            if label.height() >= MAP_LABEL_HEIGHT * 2.0 + 1.0 { paint_map_label(ui, label.translate(Vec2::new(0.0, MAP_LABEL_HEIGHT + 1.0)).with_max_y(label.bottom()), &format_bytes(node.size(self.settings.allocated)), tile_text_color(fill)); }
                        }
                        focused = self.map_item(ui, response, tile.node, snapshot, base, bounds.size()).or(focused);
                    }
                    for index in self.selection.indices() {
                        if let Some(tile) = data.frame(index) {
                            let rect = self.map_viewport.rect(base_rect(tile.rect, data, bounds.size()), bounds.min);
                            if !readable_map_region(rect.intersect(bounds)) { continue; }
                            painter.rect_stroke(rect.shrink(1.0), 3.0, Stroke::new(3.0_f32, ACCENT), StrokeKind::Inside);
                        }
                    }
                    if let Some(rect) = focused { paint_map_focus(ui, rect); }
                }
            }
            if self.map_worker.pending() || self.map_worker.raster_pending() {
                let (done, total) = self.map_worker.progress();
                painter.rect_filled(egui::Rect::from_min_size(bounds.left_top(), Vec2::new(bounds.width().min(420.0), 30.0)), 4.0, ui.visuals().window_fill);
                let message = if self.map_worker.pending() { trf("Preparing map: {done} / {total}", &[("done", done.to_string()), ("total", total.to_string())]) } else { tr("Preparing map").into() };
                painter.text(bounds.left_top() + Vec2::splat(8.0), egui::Align2::LEFT_TOP, message, FontId::proportional(HELP_SIZE), ui.visuals().text_color());
                ui.ctx().request_repaint_after(Duration::from_millis(100));
            }
            if !moving && data.is_some() && let Some(index) = self.map_focus.take() {
                self.scope = Some(index); self.map_viewport = MapViewport::default(); ui.ctx().request_repaint();
            }
        });
    }

    fn select_aggregate_owner(&mut self, index: usize, snapshot: &Snapshot, toggle: bool) {
        self.selection.click(index, &[(index, 0)], toggle, false);
        self.selected = self.selection.primary();
        let mut parent = snapshot.nodes[index].parent;
        while let Some(ancestor) = parent {
            self.rebuild |= self.expanded.insert(ancestor);
            parent = snapshot.nodes[ancestor].parent;
        }
        self.reveal_row = Some(index);
    }

    fn map_item(
        &mut self,
        ui: &mut egui::Ui,
        response: egui::Response,
        index: usize,
        snapshot: &Snapshot,
        base: egui::Rect,
        size: Vec2,
    ) -> Option<egui::Rect> {
        self.item_response(response.clone(), index, snapshot, SelectionView::Single);
        if response.double_clicked() {
            self.map_viewport.zoom_to(base, size);
            if snapshot.nodes[index].kind == EntryKind::Directory && self.scope != Some(index) {
                self.map_focus = Some(index);
            }
            ui.ctx().request_repaint();
        }
        let focused = response.has_focus().then_some(response.rect);
        response.on_hover_ui(|ui| {
            ui.label(display_path(&snapshot.nodes[index].path));
            ui.label(format_bytes(
                snapshot.nodes[index].size(self.settings.allocated),
            ));
        });
        focused
    }

    fn sunburst_canvas(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &Snapshot,
        bounds: egui::Rect,
        data: &MapData,
    ) {
        let size = bounds.size();
        let center = egui::Pos2::ZERO + size / 2.0;
        let radius = size.x.min(size.y) / 2.0 - 5.0;
        let Some(summary) = self.map_worker.summary() else {
            return;
        };
        let depth = summary
            .depth
            .min(11)
            .min(self.settings.map_depth.saturating_sub(1));
        let band = radius / (depth + 2) as f32;
        let pointer = ui.input(|input| input.pointer.hover_pos());
        let painter = ui
            .painter()
            .with_clip_rect(bounds.intersect(ui.clip_rect()));
        let mut hovered = None;
        for sector in &data.sectors {
            if sector.depth > depth {
                continue;
            }
            let inner = band * (sector.depth + 1) as f32;
            let outer = inner + band - 1.0;
            let span = sector.end - sector.start;
            if span * outer * self.map_viewport.scale < 1.0 {
                continue;
            }
            let steps =
                ((span * outer * self.map_viewport.scale / 6.0).ceil() as usize).clamp(2, 160);
            let fill = self
                .file_colors
                .color(&snapshot.nodes[sector.node].extension());
            let mut mesh = egui::Mesh::default();
            for step in 0..=steps {
                let angle =
                    sector.start + span * step as f32 / steps as f32 - std::f32::consts::FRAC_PI_2;
                for distance in [inner, outer] {
                    let point = center + Vec2::angled(angle) * distance;
                    mesh.colored_vertex(
                        bounds.min
                            + self.map_viewport.offset
                            + point.to_vec2() * self.map_viewport.scale,
                        fill,
                    );
                }
                if step > 0 {
                    let vertex = (step * 2) as u32;
                    mesh.add_triangle(vertex - 2, vertex - 1, vertex);
                    mesh.add_triangle(vertex - 1, vertex + 1, vertex);
                }
            }
            painter.add(egui::Shape::mesh(mesh));
            let middle = sector.start + span / 2.0 - std::f32::consts::FRAC_PI_2;
            let point = center + Vec2::angled(middle) * ((inner + outer) / 2.0);
            let label_center =
                bounds.min + self.map_viewport.offset + point.to_vec2() * self.map_viewport.scale;
            if self.settings.labels
                && span * inner * self.map_viewport.scale > 50.0
                && band * self.map_viewport.scale >= MAP_LABEL_HEIGHT
            {
                let width = (span * inner * self.map_viewport.scale * 0.7).min(140.0);
                paint_map_label(
                    ui,
                    egui::Rect::from_center_size(label_center, Vec2::new(width, MAP_LABEL_HEIGHT))
                        .intersect(bounds),
                    &snapshot.nodes[sector.node].name(),
                    tile_text_color(fill),
                );
            }
            if selected_region(&self.selection, snapshot, sector.node) {
                painter.circle_stroke(
                    label_center,
                    3.0,
                    Stroke::new(2.0_f32, ui.visuals().text_color()),
                );
            }
            if let Some(pointer) = pointer.filter(|pointer| bounds.contains(*pointer)) {
                let relative = (pointer - bounds.min - self.map_viewport.offset)
                    / self.map_viewport.scale
                    - center.to_vec2();
                let angle = (relative.angle() + std::f32::consts::FRAC_PI_2)
                    .rem_euclid(std::f32::consts::TAU);
                if relative.length() >= inner
                    && relative.length() <= outer
                    && angle >= sector.start
                    && angle <= sector.end
                {
                    hovered = Some((sector.node, point));
                }
            }
        }
        let sense = if data.sectors.is_empty() {
            Sense::hover()
        } else {
            Sense::click()
        };
        let response = ui.interact(bounds, ui.id().with("sunburst-hit"), sense);
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                response.enabled(),
                tr("Storage map"),
            )
        });
        let keyboard = sunburst_keyboard_target(
            &response,
            &data.sectors,
            self.selection.primary(),
            hovered.map(|(index, _)| index),
        );
        let keyboard_target = keyboard.and_then(|(index, _)| {
            data.sectors
                .iter()
                .find(|sector| sector.node == index)
                .map(|sector| {
                    let middle = (sector.start + sector.end) / 2.0 - std::f32::consts::FRAC_PI_2;
                    (
                        index,
                        center + Vec2::angled(middle) * (band * (sector.depth as f32 + 1.5) - 0.5),
                    )
                })
        });
        if let Some((_, point)) =
            keyboard_target.filter(|_| keyboard.is_some_and(|(_, active)| active))
        {
            let point =
                bounds.min + self.map_viewport.offset + point.to_vec2() * self.map_viewport.scale;
            painter.circle_stroke(point, 6.0, Stroke::new(4.0_f32, Color32::WHITE));
            painter.circle_stroke(point, 6.0, Stroke::new(2.0_f32, Color32::BLACK));
        }
        let target = if response.context_menu_opened() {
            self.selection.primary().map(|index| (index, center))
        } else if keyboard.is_some_and(|(_, active)| active) {
            keyboard_target
        } else {
            hovered.or(keyboard_target)
        };
        if let Some((index, point)) = target {
            let focus = egui::Rect::from_center_size(point, size / 2.0);
            if let Some(rect) = self.map_item(ui, response, index, snapshot, focus, size) {
                paint_map_focus(ui, rect);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_region_actions_require_both_a_usable_span_and_area() {
        for (size, readable) in [
            (Vec2::new(4.0, 6.0), true),
            (Vec2::new(4.0, 5.0), false),
            (Vec2::new(3.0, 16.0), false),
            (Vec2::new(8.0, 4.0), true),
        ] {
            assert_eq!(
                readable_map_region(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                readable
            );
        }
    }

    #[test]
    fn tiny_directory_colors_paint_without_creating_widgets_until_zoomed() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::write(fixture.path().join("file.txt"), b"x").unwrap();
        let mut snapshot = scanner::scan(
            &[fixture.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        let template = snapshot.nodes[1].clone();
        let count = 100_000_usize;
        snapshot.nodes = vec![Node {
            kind: EntryKind::Directory,
            parent: None,
            children: (0..count).map(|index| index * 2 + 1).collect(),
            logical_size: count as u64,
            allocated_size: Some(count as u64),
            ..template.clone()
        }];
        for index in 0..count {
            let folder = index * 2 + 1;
            snapshot.nodes.push(Node {
                path: PathBuf::from(format!("root/folder-{index}")),
                kind: EntryKind::Directory,
                parent: Some(0),
                children: vec![folder + 1],
                logical_size: 1,
                allocated_size: Some(1),
                ..template.clone()
            });
            snapshot.nodes.push(Node {
                path: PathBuf::from(format!("root/folder-{index}/file.txt")),
                parent: Some(folder),
                logical_size: 1,
                allocated_size: Some(1),
                ..template.clone()
            });
        }
        let snapshot = Arc::new(snapshot);
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut app = UniDirStatApp::new(
            &eframe::CreationContext::_new_kittest(context.clone()),
            Vec::new(),
            None,
            ScanOptions::default(),
            None,
        );
        app.file_colors = Arc::new(FileColors::from_extensions(&[ExtensionStats {
            extension: "txt".into(),
            bytes: count as u64,
            files: count as u64,
        }]));
        app.map_worker.set_colors(Arc::clone(&app.file_colors));
        app.settings.reduced_motion = true;
        app.map_worker.request(
            MapKey {
                scope: None,
                allocated: false,
                width: 1000,
                height: 500,
                kind: MapKind::Treemap,
                depth_limit: 6,
            },
            Arc::clone(&snapshot),
            &context,
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.map_worker.pending() {
            assert!(Instant::now() < deadline);
            app.map_worker.poll();
            std::thread::yield_now();
        }
        let mut colored = Vec::new();
        let mut painted = egui::Rect::NOTHING;
        let mut node_count = 0;
        for _ in 0..1000 {
            app.map_worker.poll();
            let output = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(800.0, 500.0),
                    )),
                    ..Default::default()
                },
                |context| {
                    egui::CentralPanel::default()
                        .show(context, |ui| app.map_canvas(ui, &snapshot, 320.0));
                },
            );
            node_count = output.platform_output.accesskit_update.unwrap().nodes.len();
            painted = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect)
                        if rect.fill == context.style().visuals.extreme_bg_color =>
                    {
                        Some(rect.rect)
                    }
                    _ => None,
                })
                .max_by(|left, right| left.area().total_cmp(&right.area()))
                .unwrap();
            colored = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect) if rect.fill == FILE_PALETTE[0] => Some(rect.rect),
                    _ => None,
                })
                .collect();
            if colored.is_empty() {
                colored = output
                    .shapes
                    .iter()
                    .filter_map(|shape| match &shape.shape {
                        egui::Shape::Mesh(mesh)
                            if mesh.texture_id != egui::TextureId::default() =>
                        {
                            Some(mesh.vertices.iter().fold(
                                egui::Rect::NOTHING,
                                |mut rect, vertex| {
                                    rect.extend_with(vertex.pos);
                                    rect
                                },
                            ))
                        }
                        _ => None,
                    })
                    .collect();
            }
            if !colored.is_empty() && !app.map_worker.pending() && !app.map_worker.raster_pending()
            {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(
            node_count < 200,
            "tiny regions registered {node_count} accessibility nodes"
        );
        assert!(colored.len() == count || colored.len() == 1);
        app.map_viewport
            .zoom_at(painted.size() / 2.0, 64.0, painted.size());
        let output = loop {
            app.map_worker.poll();
            let output = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(800.0, 500.0),
                    )),
                    ..Default::default()
                },
                |context| {
                    egui::CentralPanel::default()
                        .show(context, |ui| app.map_canvas(ui, &snapshot, 320.0));
                },
            );
            if !app.map_worker.pending() && !app.map_worker.raster_pending() {
                break output;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        };
        let labels: Vec<_> = output
            .platform_output
            .accesskit_update
            .unwrap()
            .nodes
            .into_iter()
            .filter_map(|(_, node)| node.label().map(str::to_owned))
            .collect();
        assert!(
            labels.iter().any(|label| label.contains("root/folder-")),
            "scale={}, labels={labels:?}",
            app.map_viewport.scale
        );
    }

    #[test]
    fn wide_map_toolbar_keeps_its_entry_count_on_the_zoom_row_at_the_right_edge() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::write(fixture.path().join("file"), b"data").unwrap();
        let snapshot = Arc::new(
            scanner::scan(
                &[fixture.path().into()],
                &ScanOptions::default(),
                &ScanControl::default(),
            )
            .unwrap(),
        );
        let context = egui::Context::default();
        let mut app = UniDirStatApp::new(
            &eframe::CreationContext::_new_kittest(context.clone()),
            Vec::new(),
            None,
            ScanOptions::default(),
            None,
        );
        app.rows = vec![(0, 0), (1, 1)];
        let count = trf("{count} visible entries", &[("count", "2".into())]);
        let mut count_rect = None;
        let mut zoom_rect = None;
        let mut right_edge = 0.0;
        for _ in 0..3 {
            let output = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(1800.0, 600.0),
                    )),
                    ..Default::default()
                },
                |context| {
                    egui::CentralPanel::default().show(context, |ui| {
                        right_edge = ui.max_rect().right() - 12.0;
                        app.map_canvas(ui, &snapshot, 400.0);
                    });
                },
            );
            for shape in output.shapes {
                if let egui::Shape::Text(text) = shape.shape {
                    if text.galley.job.text == count {
                        count_rect = Some(text.visual_bounding_rect());
                    }
                    if text.galley.job.text == "100%" {
                        zoom_rect = Some(text.visual_bounding_rect());
                    }
                }
            }
        }
        let count_rect = count_rect.expect("entry count");
        let zoom_rect = zoom_rect.expect("zoom amount");
        assert!(
            (count_rect.center().y - zoom_rect.center().y).abs() < 8.0,
            "{count_rect:?}, {zoom_rect:?}"
        );
        assert!(
            (count_rect.right() - right_edge).abs() < 4.0,
            "{count_rect:?}, {right_edge}"
        );
    }

    #[test]
    fn aggregate_owner_selection_reveals_the_folder_and_updates_details() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::create_dir(fixture.path().join("folder")).unwrap();
        let snapshot = scanner::scan(
            &[fixture.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        let folder = snapshot
            .nodes
            .iter()
            .position(|node| node.name() == "folder")
            .unwrap();
        let context = egui::Context::default();
        let mut app = UniDirStatApp::new(
            &eframe::CreationContext::_new_kittest(context),
            Vec::new(),
            None,
            ScanOptions::default(),
            None,
        );
        app.expanded.clear();
        app.select_aggregate_owner(folder, &snapshot, false);
        assert_eq!(app.selected, Some(folder));
        assert_eq!(app.reveal_row, Some(folder));
        assert!(
            app.expanded
                .contains(&snapshot.nodes[folder].parent.unwrap())
        );
        app.select_aggregate_owner(folder, &snapshot, true);
        assert_eq!(app.selected, None);
    }

    #[test]
    fn sunburst_keyboard_roves_visible_sectors_and_activates_the_focused_name() {
        let context = egui::Context::default();
        let sectors: Vec<_> = [3, 7, 9]
            .into_iter()
            .map(|node| unidirstat::layout::Sector {
                node,
                start: 0.0,
                end: 1.0,
                depth: 0,
            })
            .collect();
        let mut target = None;
        let mut activated = None;
        for (key, expected) in [
            (None, 7),
            (Some(egui::Key::ArrowRight), 9),
            (Some(egui::Key::ArrowRight), 3),
            (Some(egui::Key::ArrowLeft), 9),
            (Some(egui::Key::ArrowUp), 7),
            (Some(egui::Key::ArrowDown), 9),
            (Some(egui::Key::Enter), 9),
            (Some(egui::Key::Space), 9),
        ] {
            let _ = context.run(
                egui::RawInput {
                    events: key
                        .map(|key| egui::Event::Key {
                            key,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: egui::Modifiers::NONE,
                        })
                        .into_iter()
                        .collect(),
                    ..Default::default()
                },
                |context| {
                    egui::CentralPanel::default().show(context, |ui| {
                        let response = ui.interact(
                            egui::Rect::from_min_size(egui::pos2(10.0, 20.0), Vec2::splat(100.0)),
                            ui.id().with("sunburst"),
                            Sense::click(),
                        );
                        if key.is_none() {
                            response.request_focus();
                        }
                        ui.interact(
                            egui::Rect::from_min_size(egui::pos2(150.0, 20.0), Vec2::splat(100.0)),
                            ui.id().with("neighbor"),
                            Sense::click(),
                        );
                        target = sunburst_keyboard_target(&response, &sectors, Some(7), None)
                            .map(|(index, _)| index);
                        if response.clicked() {
                            activated = target;
                        }
                    });
                },
            );
            assert_eq!(target, Some(expected));
        }
        assert_eq!(activated, Some(9));
    }

    #[test]
    fn sunburst_keyboard_falls_back_to_the_first_visible_sector() {
        let context = egui::Context::default();
        let sectors = [unidirstat::layout::Sector {
            node: 4,
            start: 0.0,
            end: 1.0,
            depth: 0,
        }];
        let mut target = None;
        let _ = context.run(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                let response = ui.interact(
                    egui::Rect::from_min_size(egui::pos2(10.0, 20.0), Vec2::splat(100.0)),
                    ui.id().with("sunburst"),
                    Sense::click(),
                );
                response.request_focus();
                target = sunburst_keyboard_target(&response, &sectors, Some(99), None)
                    .map(|(index, _)| index);
            });
        });
        assert_eq!(target, Some(4));
    }

    #[test]
    fn small_map_tiles_keep_their_area_instead_of_becoming_gapped_dots() {
        let rect = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), Vec2::new(8.0, 6.0));
        let (painted, radius) = tile_shape(rect);

        assert_eq!(painted, rect);
        assert_eq!(radius, 0.0);
        let (large, radius) = tile_shape(egui::Rect::from_min_size(
            egui::pos2(10.0, 20.0),
            Vec2::new(80.0, 60.0),
        ));
        assert_eq!(large.min, egui::pos2(10.0, 20.0));
        assert_eq!(large.size(), Vec2::new(80.0, 60.0));
        assert_eq!(radius, 0.0);
    }

    #[test]
    fn map_labels_skip_boxes_that_only_fit_an_ellipsis() {
        let context = egui::Context::default();
        let output = context.run(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                paint_map_label(
                    ui,
                    egui::Rect::from_min_size(egui::pos2(10.0, 20.0), Vec2::new(14.0, 22.0)),
                    "Very long file name.txt",
                    Color32::BLACK,
                );
            });
        });

        assert!(
            output
                .shapes
                .iter()
                .all(|shape| { !matches!(shape.shape, egui::Shape::Text(_)) })
        );
    }

    #[test]
    fn map_labels_keep_readable_text_size_when_the_name_is_truncated() {
        let context = egui::Context::default();
        let output = context.run(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                paint_map_label(
                    ui,
                    egui::Rect::from_min_size(egui::pos2(10.0, 20.0), Vec2::new(90.0, 24.0)),
                    "Very long file name.txt",
                    Color32::BLACK,
                );
            });
        });
        let labels: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| {
                if let egui::Shape::Text(text) = &shape.shape {
                    Some(text)
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(labels.len(), 1);
        assert!(
            labels[0]
                .galley
                .job
                .sections
                .iter()
                .all(|section| section.format.font_id.size >= 14.0)
        );
        assert!(
            labels[0]
                .galley
                .rows
                .iter()
                .flat_map(|row| &row.glyphs)
                .filter(|glyph| glyph.chr != '…')
                .count()
                >= 2
        );
    }

    #[test]
    fn flame_refinement_never_changes_vertical_geometry() {
        assert_eq!(
            map_resolution(MapView::Flame, Vec2::new(800.0, 240.0), 4),
            (3200, 240)
        );
        assert_eq!(
            map_resolution(MapView::Treemap, Vec2::new(800.0, 240.0), 4),
            (3200, 960)
        );
    }

    #[test]
    fn folder_selection_highlights_descendants_but_not_other_branches() {
        let fixture = tempfile::tempdir().expect("fixture");
        let folder = fixture.path().join("inside");
        std::fs::create_dir(&folder).expect("directory");
        std::fs::write(folder.join("child"), b"inside").expect("file");
        std::fs::write(fixture.path().join("other"), b"outside").expect("file");
        let snapshot = scanner::scan(
            &[fixture.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .expect("scan");
        let find = |name: &str| {
            snapshot
                .nodes
                .iter()
                .position(|node| node.name() == name)
                .expect("node")
        };
        let mut selection = Selection::default();
        selection.click(find("inside"), &[], false, false);
        assert!(selected_region(&selection, &snapshot, find("child")));
        assert!(!selected_region(&selection, &snapshot, find("other")));
    }
}
