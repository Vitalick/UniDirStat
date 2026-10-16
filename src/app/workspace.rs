use super::*;

pub(super) fn paint_label(ui: &egui::Ui, rect: egui::Rect, text: &str, color: Color32, size: f32) {
    if rect.width() < 4.0 || rect.height() < 12.0 {
        return;
    }
    let galley = egui::WidgetText::from(RichText::new(text).size(size).color(color)).into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        rect.width(),
        egui::TextStyle::Body,
    );
    ui.painter()
        .with_clip_rect(rect.intersect(ui.clip_rect()))
        .galley(
            rect.left_center() - Vec2::new(0.0, galley.size().y / 2.0),
            galley,
            color,
        );
}

pub(super) fn language_icon(ui: &mut egui::Ui) {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::hover());
    ui.painter().text(
        rect.left_top(),
        egui::Align2::LEFT_TOP,
        "A",
        FontId::proportional(16.0),
        ui.visuals().text_color(),
    );
    ui.painter().text(
        rect.right_bottom(),
        egui::Align2::RIGHT_BOTTOM,
        "Я",
        FontId::proportional(14.0),
        ACCENT,
    );
    response.on_hover_text(tr("Language"));
}

pub(super) fn info_button(ui: &mut egui::Ui, text: &str) {
    let response = ui
        .menu_button("  ", |ui| {
            ui.set_max_width(360.0);
            ui.label(RichText::new(text).size(BODY_SIZE));
        })
        .response;
    ui.painter()
        .circle_stroke(response.rect.center(), 8.0, Stroke::new(1.2, muted(ui)));
    ui.painter().text(
        response.rect.center(),
        egui::Align2::CENTER_CENTER,
        "i",
        FontId::proportional(BODY_SIZE),
        muted(ui),
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tr("Information"))
    });
    response.on_hover_text(tr("Information"));
}

fn triangle(painter: &egui::Painter, center: egui::Pos2, direction: Vec2, color: Color32) {
    let perpendicular = Vec2::new(-direction.y, direction.x);
    let points = vec![
        center - direction * 2.0 + perpendicular * 4.0,
        center - direction * 2.0 - perpendicular * 4.0,
        center + direction * 3.0,
    ];
    painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
}

fn file_icon(painter: &egui::Painter, rect: egui::Rect, directory: bool, color: Color32) {
    if directory {
        painter.rect_filled(
            egui::Rect::from_min_size(rect.min, Vec2::new(8.0, 5.0)),
            2.0,
            PALETTE[3],
        );
        painter.rect_filled(rect.shrink2(Vec2::new(0.0, 2.0)), 2.0, PALETTE[3]);
    } else {
        painter.rect_stroke(
            rect.shrink2(Vec2::new(2.0, 0.0)),
            2.0,
            Stroke::new(1.5, color),
            StrokeKind::Inside,
        );
        painter.line_segment(
            [
                rect.left_center() + Vec2::new(5.0, 0.0),
                rect.right_center() - Vec2::new(4.0, 0.0),
            ],
            Stroke::new(1.0, color),
        );
    }
}

pub(super) fn parent_share(snapshot: &Snapshot, index: usize, allocated: bool) -> f32 {
    let denominator = snapshot.nodes[index].parent.map_or_else(
        || snapshot.total_size(allocated),
        |parent| snapshot.nodes[parent].size(allocated),
    );
    (snapshot.nodes[index].size(allocated) as f64 / denominator.max(1) as f64).clamp(0.0, 1.0)
        as f32
}

fn take_tree_keys(context: &egui::Context) -> (bool, bool, bool, bool, bool, bool, bool) {
    let keys = context.input_mut(|input| {
        let modifiers = input
            .events
            .iter()
            .find_map(|event| match event {
                egui::Event::Key {
                    key:
                        egui::Key::ArrowUp
                        | egui::Key::ArrowDown
                        | egui::Key::ArrowLeft
                        | egui::Key::ArrowRight
                        | egui::Key::Home
                        | egui::Key::End,
                    pressed: true,
                    modifiers,
                    ..
                } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or(input.modifiers);
        (
            input.consume_key(modifiers, egui::Key::ArrowUp),
            input.consume_key(modifiers, egui::Key::ArrowDown),
            input.consume_key(modifiers, egui::Key::ArrowLeft),
            input.consume_key(modifiers, egui::Key::ArrowRight),
            input.consume_key(modifiers, egui::Key::Home),
            input.consume_key(modifiers, egui::Key::End),
            modifiers.shift,
        )
    });
    if keys.0 || keys.1 || keys.2 || keys.3 || keys.4 || keys.5 {
        context.memory_mut(|memory| memory.move_focus(egui::FocusDirection::None));
    }
    keys
}

fn tree_panel_heights(available: f32) -> (f32, f32, f32) {
    let available = available.max(1.0);
    let minimum = (CONTROL_HEIGHT + ROW_HEIGHT * 3.0 + 18.0).min(available * 0.6);
    let maximum = (available - 180.0).max(minimum);
    (minimum, maximum, (available * 0.5).clamp(minimum, maximum))
}

fn focus_outline(ui: &egui::Ui, response: &egui::Response) {
    if response.has_focus() {
        ui.painter().rect_stroke(
            response.rect.shrink(1.0),
            3.0,
            Stroke::new(2.0, ui.visuals().text_color()),
            StrokeKind::Inside,
        );
    }
}

impl UniDirStatApp {
    pub(super) fn workspace(&mut self, ui: &mut egui::Ui, snapshot: &Arc<Snapshot>) {
        self.map_worker
            .request_summary(self.scope, Arc::clone(snapshot), ui.ctx());
        let narrow = ui.available_width() < 900.0;
        let very_short = ui.available_height() < 360.0;
        if !narrow {
            egui::SidePanel::right("workspace-summary")
                .resizable(true)
                .default_width(280.0)
                .width_range(220.0..=420.0)
                .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 0)))
                .show_inside(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("workspace-summary-scroll")
                        .show(ui, |ui| self.workspace_summary(ui, snapshot));
                });
        } else if !very_short && ui.button(tr("Statistics and details")).clicked() {
            self.summary_open = true;
        }
        let compact = ui.available_height() < 600.0;
        if very_short {
            ui.horizontal_wrapped(|ui| {
                egui::ComboBox::from_id_salt("workspace-content-view")
                    .width(112.0)
                    .selected_text(if self.compact_map {
                        tr("Storage map")
                    } else {
                        tr("All files")
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.compact_map, false, tr("All files"));
                        ui.selectable_value(&mut self.compact_map, true, tr("Storage map"));
                    })
                    .response
                    .widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::ComboBox,
                            true,
                            tr("Content view"),
                        )
                    });
                if narrow && ui.button(tr("Summary")).clicked() {
                    self.summary_open = true;
                }
                if !self.compact_map {
                    self.file_controls(ui);
                }
            });
            if self.compact_map {
                self.map(ui, snapshot, ui.available_height());
            } else {
                self.file_table(ui, snapshot);
            }
        } else if compact {
            ui.horizontal_wrapped(|ui| {
                ui.selectable_value(&mut self.compact_map, false, tr("All files"));
                ui.selectable_value(&mut self.compact_map, true, tr("Storage map"));
            });
        }
        if !very_short {
            if compact && self.compact_map {
                self.map(ui, snapshot, ui.available_height());
            } else {
                self.file_controls(ui);
                if compact {
                    self.file_table(ui, snapshot);
                } else {
                    let (minimum, maximum, height) = tree_panel_heights(ui.available_height());
                    egui::TopBottomPanel::top("workspace-tree")
                        .resizable(true)
                        .default_height(height)
                        .height_range(minimum..=maximum)
                        .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(0, 8)))
                        .show_inside(ui, |ui| self.file_table(ui, snapshot));
                    ui.add_space(8.0);
                    self.map(ui, snapshot, ui.available_height());
                }
            }
        }
        if self.summary_open {
            let context = ui.ctx().clone();
            let mut open = self.summary_open;
            egui::Window::new(RichText::new(tr("Statistics and details")).size(BODY_SIZE))
                .id(egui::Id::new("workspace-summary-window"))
                .open(&mut open)
                .constrain_to(context.content_rect())
                .max_size(context.content_rect().size() - Vec2::new(24.0, 80.0))
                .default_size([340.0, 440.0])
                .vscroll(true)
                .show(&context, |ui| self.workspace_summary(ui, snapshot));
            self.summary_open = open;
        }
    }

    fn workspace_summary(&mut self, ui: &mut egui::Ui, snapshot: &Arc<Snapshot>) {
        self.scope_summary(ui, snapshot);
        if let Some(index) = self.selected {
            ui.add_space(12.0);
            ui.separator();
            self.inspector_contents(ui, snapshot, index);
        }
    }

    fn scope_summary(&mut self, ui: &mut egui::Ui, snapshot: &Arc<Snapshot>) {
        ui.heading(tr("Overview"));
        let size = self.scope.map_or_else(
            || snapshot.total_size(self.settings.allocated),
            |index| snapshot.nodes[index].size(self.settings.allocated),
        );
        ui.label(
            RichText::new(format_bytes(size))
                .monospace()
                .size(25.0)
                .strong(),
        );
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::Label::new(self.scope.map_or_else(|| tr("All scanned locations").to_owned(), |index| snapshot.nodes[index].path.display().to_string())).wrap());
            info_button(ui, tr("Logical size can exceed disk capacity (sparse files, hard links). Allocated blocks may be shared; snapshots and filesystem metadata are not included."));
        });
        if !self.analysis_complete {
            ui.label(tr(
                "Analysis was cancelled. Refresh to calculate all statistics.",
            ));
        }
        if self.map_worker.summary_pending() {
            ui.horizontal(|ui| {
                if !self.settings.reduced_motion {
                    ui.spinner();
                }
                ui.label(tr("Calculating statistics"));
            });
        }
        if let Some(error) = self.map_worker.summary_error() {
            ui.colored_label(
                ui.visuals().error_fg_color,
                trf("Error: {details}", &[("details", error.to_owned())]),
            );
            ui.label(tr("Refresh to retry statistics."));
        }
        let Some(summary) = self.map_worker.summary().map(Arc::clone) else {
            return;
        };
        ui.separator();
        ui.strong(tr("File types"));
        let extensions = if self.settings.allocated {
            &summary.extensions_allocated
        } else {
            &summary.extensions_logical
        };
        for stats in extensions.iter().take(8) {
            ui.horizontal(|ui| {
                color_dot(ui, extension_color(&stats.extension));
                if ui.button(extension_label(&stats.extension)).clicked() {
                    self.search_query.extension = Some(stats.extension.clone());
                    self.search_query.scope = self.scope;
                    self.page = Page::Files;
                    self.compact_map = false;
                    self.summary_open = false;
                    self.rebuild = true;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.monospace(format_bytes(stats.bytes));
                });
            });
            ui.add(
                egui::ProgressBar::new(stats.bytes as f32 / size.max(1) as f32)
                    .desired_height(4.0)
                    .fill(extension_color(&stats.extension)),
            );
        }
        ui.add_space(12.0);
        ui.strong(tr("Largest files"));
        let largest = if self.settings.allocated {
            &summary.largest_allocated
        } else {
            &summary.largest_logical
        };
        for &index in largest {
            let node = &snapshot.nodes[index];
            let response = egui::Frame::new()
                .inner_margin(6)
                .fill(if self.selection.contains(index) {
                    ui.visuals().selection.bg_fill
                } else {
                    Color32::TRANSPARENT
                })
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.add(
                        egui::Label::new(node.name())
                            .truncate()
                            .sense(Sense::hover()),
                    );
                    ui.monospace(format_bytes(node.size(self.settings.allocated)));
                })
                .response;
            let response = clickable_row(response);
            focus_outline(ui, &response);
            self.item_response(response, index, snapshot, SelectionView::Overview);
        }
    }

    pub(super) fn file_controls(&mut self, ui: &mut egui::Ui) {
        let compact = ui.available_height() < 360.0;
        if compact {
            let response = ui
                .menu_button(
                    if self.filter_error.is_some() {
                        tr("Search error")
                    } else {
                        tr("Search")
                    },
                    |ui| {
                        let viewport = ui.ctx().content_rect().size();
                        ui.set_width((viewport.x - 40.0).clamp(160.0, 480.0));
                        egui::ScrollArea::vertical()
                            .max_height((viewport.y - 80.0).max(40.0))
                            .show(ui, |ui| {
                                self.filters(ui);
                                ui.label(trf(
                                    "{count} visible entries",
                                    &[("count", self.rows.len().to_string())],
                                ));
                                if self.search_receiver.is_some() {
                                    show_work_progress(ui, &self.search_control);
                                }
                            });
                    },
                )
                .response;
            response.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    true,
                    if self.filter_error.is_some() {
                        tr("Search error")
                    } else {
                        tr("Search and filters")
                    },
                )
            });
        } else {
            self.filters(ui);
        }
        self.rebuild_rows(ui.ctx());
        if self.search_receiver.is_some() {
            let cancelled = self.search_control.is_cancelled();
            let paused = self.search_control.is_paused();
            ui.horizontal_wrapped(|ui| {
                if !cancelled && !paused && !self.settings.reduced_motion {
                    ui.spinner();
                }
                ui.label(if cancelled {
                    tr("Cancelling...")
                } else if paused {
                    tr("Search paused")
                } else {
                    tr("Searching...")
                });
                if ui
                    .add_enabled(
                        !cancelled,
                        egui::Button::new(if paused { tr("Resume") } else { tr("Pause") }),
                    )
                    .clicked()
                {
                    if paused {
                        self.search_control.resume();
                    } else {
                        self.search_control.pause();
                    }
                }
                if ui
                    .add_enabled(!cancelled, egui::Button::new(tr("Cancel")))
                    .clicked()
                {
                    self.search_control.cancel();
                }
            });
            if !compact {
                show_work_progress(ui, &self.search_control);
            }
        }
        if !compact {
            ui.label(trf(
                "{count} visible entries",
                &[("count", self.rows.len().to_string())],
            ));
        }
        if self.search_receiver.is_none() && self.rows.is_empty() && self.filter_error.is_none() {
            if has_filters(&self.search_query) {
                ui.horizontal_wrapped(|ui| {
                    ui.label(tr("No entries match these filters."));
                    if ui.button(tr("Clear filters")).clicked() {
                        self.search_query = SearchQuery::default();
                        self.rebuild = true;
                    }
                });
            } else {
                ui.label(tr("This location has no entries to display."));
            }
        }
    }

    pub(super) fn file_table(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        let visible = [
            true,
            true,
            self.settings.show_modified,
            self.settings.show_share,
        ];
        let remaining: f32 = self
            .column_widths
            .iter()
            .enumerate()
            .skip(1)
            .filter(|(index, _)| visible[*index])
            .map(|(_, width)| *width)
            .sum();
        let name_width = if self.column_widths[0] > 0.0 {
            self.column_widths[0]
        } else {
            (ui.available_width() - remaining).max(220.0)
        };
        let widths = [
            name_width,
            self.column_widths[1],
            self.column_widths[2],
            self.column_widths[3],
        ];
        let table_width = widths
            .iter()
            .enumerate()
            .filter(|(index, _)| visible[*index])
            .map(|(_, width)| *width)
            .sum::<f32>()
            .max(ui.available_width());
        egui::ScrollArea::horizontal()
            .id_salt("file-columns")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_min_width(table_width);
                ui.spacing_mut().item_spacing.y = 0.0;
                let (header, _) =
                    ui.allocate_exact_size(Vec2::new(table_width, CONTROL_HEIGHT), Sense::hover());
                let mut left = header.left();
                for (column, (label, sort)) in [
                    (tr("Name"), Some(SortBy::Name)),
                    (tr("Size"), Some(SortBy::Size)),
                    (tr("Modified"), Some(SortBy::Modified)),
                    (tr("Share"), None),
                ]
                .into_iter()
                .enumerate()
                {
                    if !visible[column] {
                        continue;
                    }
                    let rect = egui::Rect::from_min_size(
                        egui::pos2(left, header.top()),
                        Vec2::new(widths[column], header.height()),
                    );
                    let sorted = sort == Some(self.sort_by);
                    let position = if column == 1 {
                        rect.right_center() - Vec2::new(24.0, 0.0)
                    } else {
                        rect.left_center() + Vec2::new(8.0, 0.0)
                    };
                    ui.painter().text(
                        position,
                        if column == 1 {
                            egui::Align2::RIGHT_CENTER
                        } else {
                            egui::Align2::LEFT_CENTER
                        },
                        label,
                        FontId::proportional(BODY_SIZE),
                        muted(ui),
                    );
                    if sorted {
                        triangle(
                            ui.painter(),
                            rect.right_center() - Vec2::new(8.0, 0.0),
                            if self.descending { Vec2::Y } else { -Vec2::Y },
                            ACCENT,
                        );
                    }
                    let response = ui.interact(
                        rect.shrink2(Vec2::new(4.0, 0.0)),
                        ui.id().with(("column", column)),
                        if sort.is_some() {
                            Sense::click()
                        } else {
                            Sense::hover()
                        },
                    );
                    if sort.is_some() {
                        response.widget_info(|| {
                            egui::WidgetInfo::selected(
                                egui::WidgetType::Button,
                                true,
                                sorted,
                                label,
                            )
                        });
                        focus_outline(ui, &response);
                    }
                    if response.clicked()
                        && let Some(sort) = sort
                    {
                        if self.sort_by == sort {
                            self.descending = !self.descending;
                        } else {
                            self.sort_by = sort;
                            self.descending = sort != SortBy::Name;
                        }
                        self.rebuild = true;
                    }
                    let handle = egui::Rect::from_center_size(
                        rect.right_center(),
                        Vec2::new(7.0, rect.height()),
                    );
                    let resize = ui
                        .interact(
                            handle,
                            ui.id().with(("column-resize", column)),
                            Sense::click_and_drag(),
                        )
                        .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
                    let width_label = trf("{column} column width", &[("column", label.to_owned())]);
                    resize.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &width_label)
                    });
                    focus_outline(ui, &resize);
                    let keyboard_step = if resize.has_focus() {
                        ui.input_mut(|input| {
                            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft) {
                                -12.0
                            } else if input
                                .consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight)
                            {
                                12.0
                            } else {
                                0.0
                            }
                        })
                    } else {
                        0.0
                    };
                    let minimum = if column == 0 { 160.0 } else { 72.0 };
                    if resize.dragged() || keyboard_step != 0.0 {
                        if keyboard_step != 0.0 {
                            ui.ctx()
                                .memory_mut(|memory| memory.move_focus(egui::FocusDirection::None));
                        }
                        self.column_widths[column] =
                            (widths[column] + resize.drag_delta().x + keyboard_step)
                                .clamp(minimum, 1600.0);
                    }
                    response.union(resize).context_menu(|ui| {
                        let mut width = widths[column];
                        if ui
                            .add(
                                egui::Slider::new(&mut width, minimum..=1600.0)
                                    .text(&width_label)
                                    .integer(),
                            )
                            .changed()
                        {
                            self.column_widths[column] = width;
                        }
                        if ui.button(tr("Reset columns")).clicked() {
                            self.column_widths = DEFAULT_COLUMN_WIDTHS;
                            ui.close();
                        }
                    });
                    ui.painter().vline(
                        rect.right(),
                        rect.y_range(),
                        ui.visuals().widgets.noninteractive.bg_stroke,
                    );
                    left += widths[column];
                }
                ui.separator();
                let mut scroll = egui::ScrollArea::vertical()
                    .id_salt("file-list")
                    .min_scrolled_height(0.0)
                    .auto_shrink([false, false]);
                if self.search_receiver.is_none()
                    && let Some(index) = self.reveal_row.take()
                {
                    if let Some(row) = self.rows.iter().position(|&(node, _)| node == index) {
                        scroll = scroll.vertical_scroll_offset(row as f32 * ROW_HEIGHT);
                    } else if self.search_receiver.is_some() || self.rebuild {
                        self.reveal_row = Some(index);
                    }
                }
                scroll.show_rows(ui, ROW_HEIGHT, self.rows.len(), |ui, range| {
                    for row in range {
                        let (index, depth) = self.rows[row];
                        let node = &snapshot.nodes[index];
                        let row_id = ui.id().with(("tree-row", index));
                        let (rect, _) = ui.allocate_exact_size(
                            Vec2::new(table_width, ROW_HEIGHT),
                            Sense::hover(),
                        );
                        let response = ui.interact(rect, row_id, Sense::click());
                        if self.selection.contains(index) || response.hovered() {
                            ui.painter().rect_filled(
                                rect,
                                3.0,
                                if self.selection.contains(index) {
                                    ui.visuals().selection.bg_fill
                                } else {
                                    ui.visuals().widgets.hovered.weak_bg_fill
                                },
                            );
                        } else if row % 2 == 0 {
                            ui.painter()
                                .rect_filled(rect, 0.0, ui.visuals().faint_bg_color);
                        }
                        focus_outline(ui, &response);
                        let name_rect =
                            egui::Rect::from_min_size(rect.min, Vec2::new(name_width, ROW_HEIGHT));
                        let painter = ui
                            .painter()
                            .with_clip_rect(name_rect.intersect(ui.clip_rect()));
                        let indent = (depth as f32 * 18.0).min((name_width - 85.0).max(0.0));
                        for level in 0..depth.min(40) {
                            let x = rect.left() + 17.0 + level as f32 * 18.0;
                            if x < name_rect.right() - 60.0 {
                                painter.vline(
                                    x,
                                    rect.y_range(),
                                    Stroke::new(
                                        1.0,
                                        ui.visuals().widgets.noninteractive.bg_stroke.color,
                                    ),
                                );
                            }
                        }
                        let disclosure = egui::Rect::from_min_size(
                            rect.min + Vec2::new(indent + 3.0, (ROW_HEIGHT - 24.0) / 2.0),
                            Vec2::splat(24.0),
                        );
                        if node.kind == EntryKind::Directory && !node.children.is_empty() {
                            let expanded = self.expanded.contains(&index);
                            let arrow = ui.interact(
                                disclosure,
                                ui.id().with(("disclosure", index)),
                                Sense::click(),
                            );
                            focus_outline(ui, &arrow);
                            triangle(
                                &painter,
                                disclosure.center(),
                                if expanded { Vec2::Y } else { Vec2::X },
                                muted(ui),
                            );
                            arrow.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::WidgetType::Button,
                                    true,
                                    if expanded {
                                        tr("Collapse folder")
                                    } else {
                                        tr("Expand folder")
                                    },
                                )
                            });
                            if arrow.clicked() {
                                if !self.expanded.remove(&index) {
                                    self.expanded.insert(index);
                                }
                                self.rebuild = true;
                            }
                        }
                        let icon = egui::Rect::from_min_size(
                            disclosure.right_top() + Vec2::new(3.0, 2.0),
                            Vec2::splat(16.0),
                        );
                        file_icon(
                            &painter,
                            icon,
                            node.kind == EntryKind::Directory,
                            extension_color(&node.extension()),
                        );
                        let label_rect = egui::Rect::from_min_max(
                            egui::pos2(icon.right() + 7.0, rect.top()),
                            egui::pos2(name_rect.right() - 8.0, rect.bottom()),
                        );
                        let label = if self.page == Page::Largest || has_filters(&self.search_query)
                        {
                            node.path.display().to_string()
                        } else {
                            node.name()
                        };
                        paint_label(ui, label_rect, &label, ui.visuals().text_color(), BODY_SIZE);
                        let mut left = rect.left() + name_width;
                        for column in 1..4 {
                            if !visible[column] {
                                continue;
                            }
                            let cell = egui::Rect::from_min_size(
                                egui::pos2(left, rect.top()),
                                Vec2::new(widths[column], ROW_HEIGHT),
                            );
                            match column {
                                1 => {
                                    ui.painter().text(
                                        cell.right_center() - Vec2::new(12.0, 0.0),
                                        egui::Align2::RIGHT_CENTER,
                                        format_bytes(node.size(self.settings.allocated)),
                                        FontId::monospace(BODY_SIZE),
                                        ui.visuals().text_color(),
                                    );
                                }
                                2 => {
                                    paint_label(
                                        ui,
                                        cell.shrink2(Vec2::new(8.0, 0.0)),
                                        &age(node.modified),
                                        muted(ui),
                                        HELP_SIZE,
                                    );
                                }
                                _ => {
                                    let share =
                                        parent_share(snapshot, index, self.settings.allocated);
                                    let bar = egui::Rect::from_min_max(
                                        cell.left_center() + Vec2::new(8.0, -3.0),
                                        cell.right_center() - Vec2::new(62.0, -3.0),
                                    );
                                    ui.painter().rect_filled(
                                        bar,
                                        3.0,
                                        ui.visuals().extreme_bg_color,
                                    );
                                    ui.painter().rect_filled(
                                        egui::Rect::from_min_size(
                                            bar.min,
                                            Vec2::new(bar.width() * share, bar.height()),
                                        ),
                                        3.0,
                                        ACCENT,
                                    );
                                    ui.painter().text(
                                        cell.right_center() - Vec2::new(8.0, 0.0),
                                        egui::Align2::RIGHT_CENTER,
                                        format!("{:.1}%", share * 100.0),
                                        FontId::monospace(HELP_SIZE),
                                        muted(ui),
                                    );
                                }
                            }
                            left += widths[column];
                        }
                        self.tree_key(&response, index, snapshot, ui.id());
                        self.item_response(response.clone(), index, snapshot, SelectionView::Files);
                        response.on_hover_ui(|ui| {
                            ui.label(node.path.display().to_string());
                            ui.label(trf(
                                "{parent}% of parent | {total}% of scan",
                                &[
                                    (
                                        "parent",
                                        format!(
                                            "{:.1}",
                                            parent_share(snapshot, index, self.settings.allocated)
                                                * 100.0
                                        ),
                                    ),
                                    (
                                        "total",
                                        format!(
                                            "{:.1}",
                                            node.size(self.settings.allocated) as f64
                                                / snapshot
                                                    .total_size(self.settings.allocated)
                                                    .max(1)
                                                    as f64
                                                * 100.0
                                        ),
                                    ),
                                ],
                            ));
                        });
                    }
                });
            });
    }

    fn tree_key(
        &mut self,
        response: &egui::Response,
        index: usize,
        snapshot: &Snapshot,
        parent_id: egui::Id,
    ) {
        if response.clicked() {
            response.request_focus();
        }
        if !response.has_focus() {
            return;
        }
        let input = take_tree_keys(&response.ctx);
        if !(input.0 || input.1 || input.2 || input.3 || input.4 || input.5) {
            return;
        }
        let node = &snapshot.nodes[index];
        let position = self
            .rows
            .iter()
            .position(|&(node, _)| node == index)
            .unwrap_or(0);
        let target = if input.0 {
            self.rows.get(position.saturating_sub(1)).map(|row| row.0)
        } else if input.1 {
            self.rows.get(position + 1).map(|row| row.0)
        } else if input.4 {
            self.rows.first().map(|row| row.0)
        } else if input.5 {
            self.rows.last().map(|row| row.0)
        } else if input.2 {
            if self.expanded.remove(&index) {
                self.rebuild = true;
                None
            } else {
                node.parent
            }
        } else if input.3 {
            if node.kind == EntryKind::Directory && !self.expanded.contains(&index) {
                self.expanded.insert(index);
                self.rebuild = true;
                None
            } else {
                node.children.first().copied()
            }
        } else {
            None
        };
        if let Some(target) = target {
            self.selection.click(target, &self.rows, false, input.6);
            self.selected = self.selection.primary();
            self.reveal_row = Some(target);
            response
                .ctx
                .memory_mut(|memory| memory.request_focus(parent_id.with(("tree-row", target))));
            response.ctx.request_repaint();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_panel_leaves_map_space_in_a_short_workspace() {
        let (minimum, maximum, height) = tree_panel_heights(120.0);
        assert!(minimum <= height && height <= maximum && maximum < 120.0);
    }

    #[test]
    fn tree_panel_fits_three_rows_below_its_header() {
        let (minimum, _, height) = tree_panel_heights(600.0);
        assert!(minimum >= CONTROL_HEIGHT + ROW_HEIGHT * 3.0 && height >= minimum);
    }

    #[test]
    fn range_navigation_keeps_shift_if_released_in_the_same_frame() {
        let context = egui::Context::default();
        let mut range = false;
        let _ = context.run(
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::ArrowDown,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::SHIFT,
                }],
                ..Default::default()
            },
            |context| {
                range = take_tree_keys(context).6;
            },
        );
        assert!(range);
    }

    #[test]
    fn one_navigation_key_cannot_move_multiple_focused_rows_in_one_frame() {
        let context = egui::Context::default();
        let mut moved = [false; 2];
        let _ = context.run(
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::ArrowDown,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |context| {
                moved[0] = take_tree_keys(context).1;
                moved[1] = take_tree_keys(context).1;
            },
        );
        assert_eq!(moved, [true, false]);
    }

    #[test]
    fn consumed_tree_arrows_do_not_move_focus_to_a_neighbor() {
        let context = egui::Context::default();
        let mut consumed = 0;
        for pressed in [false, true, true] {
            let _ = context.run(
                egui::RawInput {
                    events: pressed
                        .then_some(egui::Event::Key {
                            key: egui::Key::ArrowRight,
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
                            ui.id().with("tree-control"),
                            Sense::click(),
                        );
                        if !pressed {
                            response.request_focus();
                        }
                        ui.interact(
                            egui::Rect::from_min_size(egui::pos2(150.0, 20.0), Vec2::splat(100.0)),
                            ui.id().with("neighbor"),
                            Sense::click(),
                        );
                        if response.has_focus() && take_tree_keys(context).3 {
                            consumed += 1;
                        }
                    });
                },
            );
        }
        assert_eq!(consumed, 2);
    }

    #[test]
    fn share_is_relative_to_the_immediate_parent_not_the_entire_scan() {
        let directory = tempfile::tempdir().expect("fixture");
        let nested = directory.path().join("nested");
        std::fs::create_dir(&nested).expect("directory");
        std::fs::write(nested.join("half"), [0; 10]).expect("file");
        std::fs::write(nested.join("other"), [0; 10]).expect("file");
        std::fs::write(directory.path().join("outside"), [0; 80]).expect("file");
        let snapshot = scanner::scan(
            &[directory.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .expect("scan");
        let index = snapshot
            .nodes
            .iter()
            .position(|node| node.name() == "half")
            .expect("file");
        assert_eq!(parent_share(&snapshot, index, false), 0.5);
    }
}
