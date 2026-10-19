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
        .circle_stroke(response.rect.center(), 8.0, Stroke::new(1.2_f32, muted(ui)));
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

pub(super) fn triangle(
    painter: &egui::Painter,
    center: egui::Pos2,
    direction: Vec2,
    color: Color32,
) {
    let perpendicular = Vec2::new(-direction.y, direction.x);
    let points = vec![
        center - direction * 2.0 + perpendicular * 4.0,
        center - direction * 2.0 - perpendicular * 4.0,
        center + direction * 3.0,
    ];
    painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
}

pub(super) fn file_icon(
    painter: &egui::Painter,
    rect: egui::Rect,
    directory: bool,
    color: Color32,
) {
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
            Stroke::new(1.5_f32, color),
            StrokeKind::Inside,
        );
        painter.line_segment(
            [
                rect.left_center() + Vec2::new(5.0, 0.0),
                rect.right_center() - Vec2::new(4.0, 0.0),
            ],
            Stroke::new(1.0_f32, color),
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

pub(super) fn focus_outline(ui: &egui::Ui, response: &egui::Response) {
    if response.has_focus() {
        ui.painter().rect_stroke(
            response.rect.shrink(1.0),
            3.0,
            Stroke::new(2.0_f32, ui.visuals().text_color()),
            StrokeKind::Inside,
        );
    }
}

fn summary_row(ui: &mut egui::Ui, label: &str, value: &str, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width().max(1.0), BODY_SIZE + 12.0),
        Sense::click(),
    );
    if response.clicked() {
        response.request_focus();
    }
    let visuals = ui.style().interact_selectable(&response, selected);
    if selected
        || response.hovered()
        || response.has_focus()
        || response.is_pointer_button_down_on()
    {
        ui.painter().rect_filled(rect, 3.0, visuals.weak_bg_fill);
    }
    let font = FontId::monospace(BODY_SIZE);
    let (value_galley, reserved_width) = ui.fonts_mut(|fonts| {
        let value_galley =
            fonts.layout_no_wrap(value.to_owned(), font.clone(), visuals.text_color());
        let reserved_width = fonts
            .layout_no_wrap(format_bytes(1024 * 1024 - 1), font, visuals.text_color())
            .size()
            .x
            .max(value_galley.size().x)
            + 2.0;
        (value_galley, reserved_width)
    });
    let content = rect.shrink2(Vec2::new(6.0, 3.0));
    let value_left = (content.right() - reserved_width).max(content.left());
    let label_rect = egui::Rect::from_min_max(
        content.min,
        egui::pos2((value_left - 8.0).max(content.left()), content.bottom()),
    );
    paint_label(ui, label_rect, label, visuals.text_color(), BODY_SIZE);
    ui.painter()
        .with_clip_rect(
            egui::Rect::from_min_max(egui::pos2(value_left, rect.top()), rect.max)
                .intersect(ui.clip_rect()),
        )
        .galley(
            egui::pos2(
                content.right() - value_galley.size().x,
                content.center().y - value_galley.size().y / 2.0,
            ),
            value_galley,
            visuals.text_color(),
        );
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::Button,
            response.enabled(),
            selected,
            format!("{label} | {value}"),
        )
    });
    response
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
                .default_width(260.0)
                .width_range(220.0..=420.0)
                .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(8, 0)))
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
        ui.scope(|ui| {
            ui.spacing_mut().interact_size.y = BODY_SIZE + 12.0;
            ui.spacing_mut().button_padding = Vec2::new(6.0, 3.0);
            ui.spacing_mut().item_spacing = Vec2::new(6.0, 3.0);
            self.scope_summary(ui, snapshot);
            if let Some(index) = self.selected {
                ui.add_space(8.0);
                ui.separator();
                self.inspector_contents(ui, snapshot, index);
            }
        });
    }

    fn scope_summary(&mut self, ui: &mut egui::Ui, snapshot: &Arc<Snapshot>) {
        let size = self.scope.map_or_else(
            || snapshot.total_size(self.settings.allocated),
            |index| snapshot.nodes[index].size(self.settings.allocated),
        );
        ui.horizontal(|ui| {
            ui.strong(tr("Overview"));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(format_bytes(size)).monospace().strong());
            });
        });
        let path = self.scope.map_or_else(
            || tr("All scanned locations").to_owned(),
            |index| display_path(&snapshot.nodes[index].path),
        );
        ui.horizontal(|ui| {
            let response = ui.add_sized(
                [
                    (ui.available_width() - BODY_SIZE - 12.0 - ui.spacing().item_spacing.x)
                        .max(1.0),
                    BODY_SIZE + 12.0,
                ],
                egui::Label::new(&path)
                    .truncate()
                    .sense(Sense::focusable_noninteractive()),
            );
            focus_outline(ui, &response);
            if response.has_focus() {
                response.show_tooltip_text(&path);
            }
            response.on_hover_text(&path);
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
            let label = extension_label(&stats.extension);
            let response = ui
                .push_id(("summary-extension", &stats.extension), |ui| {
                    summary_row(
                        ui,
                        label,
                        &format_bytes(stats.bytes),
                        self.search_query.extension.as_ref() == Some(&stats.extension),
                    )
                })
                .inner;
            let bar = egui::Rect::from_min_max(
                response.rect.left_bottom() + Vec2::new(6.0, -4.0),
                response.rect.right_bottom() - Vec2::new(6.0, 1.0),
            );
            ui.painter()
                .rect_filled(bar, 1.0, ui.visuals().extreme_bg_color);
            ui.painter().rect_filled(
                egui::Rect::from_min_size(
                    bar.min,
                    Vec2::new(
                        bar.width() * (stats.bytes as f32 / size.max(1) as f32).clamp(0.0, 1.0),
                        bar.height(),
                    ),
                ),
                1.0,
                self.file_colors.color(&stats.extension),
            );
            focus_outline(ui, &response);
            if response.has_focus() {
                response.show_tooltip_text(label);
            }
            if response.on_hover_text(label).clicked() {
                self.search_query.extension = Some(stats.extension.clone());
                self.search_query.scope = self.scope;
                self.page = Page::Files;
                self.compact_map = false;
                self.summary_open = false;
                self.rebuild = true;
            }
        }
        ui.add_space(8.0);
        ui.strong(tr("Largest files"));
        let largest = if self.settings.allocated {
            &summary.largest_allocated
        } else {
            &summary.largest_logical
        };
        for &index in largest {
            let node = &snapshot.nodes[index];
            let response = ui
                .push_id(("summary-file", index), |ui| {
                    summary_row(
                        ui,
                        &node.name(),
                        &format_bytes(node.size(self.settings.allocated)),
                        self.selection.contains(index),
                    )
                })
                .inner;
            focus_outline(ui, &response);
            self.item_response(response.clone(), index, snapshot, SelectionView::Overview);
            if response.has_focus() {
                response.show_tooltip_text(display_path(&node.path));
            }
            response.on_hover_text(display_path(&node.path));
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
        if !compact && self.page != Page::Overview {
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
        if self.job_kind == Some(JobKind::Scan) {
            self.live_files(ui);
            return;
        }
        self.file_table_contents(ui, Some(snapshot));
    }

    pub(super) fn file_table_contents(&mut self, ui: &mut egui::Ui, snapshot: Option<&Snapshot>) {
        ui.scope_builder(egui::UiBuilder::new().id("file-browser-table"), |ui| {
        let visible = [
            true,
            true,
            self.settings.show_modified,
            self.settings.show_share,
            true,
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
            self.column_widths[4],
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
                    (tr("Scan time"), None),
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
                    } else if column == 4 {
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::WidgetType::Label, true, label)
                        });
                        response.clone().on_hover_text(tr(
                            "Folder time includes its descendants. Parallel times are not added together.",
                        ));
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
                        if snapshot.is_some() { self.rebuild = true; }
                        else { self.live_expansion_changed(); }
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
                    let allocated = self.settings.allocated;
                    let rows = if let Some(snapshot) = snapshot {
                        range.clone().map(|row| {
                            let (index, depth) = self.rows[row];
                            live_scan::FileRow::new(snapshot, index, depth, allocated, None)
                        }).collect::<Vec<_>>()
                    } else {
                        self.control.with_live_view(|view| {
                            range.clone().filter_map(|row| {
                                let &(index, depth) = self.rows.get(row)?;
                                view.snapshot.nodes.get(index)?;
                                Some(live_scan::FileRow::new(view.snapshot, index, depth, allocated, Some(&view)))
                            }).collect::<Vec<_>>()
                        }).unwrap_or_else(|| std::mem::take(&mut self.live_tree.visible))
                    };
                    for (position, node) in rows.iter().enumerate() {
                        let index = node.index;
                        let depth = node.depth;
                        let row = range.start + position;
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
                                        1.0_f32,
                                        ui.visuals().widgets.noninteractive.bg_stroke.color,
                                    ),
                                );
                            }
                        }
                        let disclosure = egui::Rect::from_min_size(
                            rect.min + Vec2::new(indent + 3.0, (ROW_HEIGHT - 24.0) / 2.0),
                            Vec2::splat(24.0),
                        );
                        if node.directory && node.expandable {
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
                                if snapshot.is_some() { self.rebuild = true; }
                                else { self.live_expansion_changed(); }
                            }
                        }
                        let status_rect = egui::Rect::from_min_size(disclosure.right_top() + Vec2::new(2.0, 3.0), Vec2::splat(18.0));
                        ui.scope_builder(egui::UiBuilder::new().max_rect(status_rect), |ui| {
                            if node.limited {
                                live_scan::paint_scan_status(ui, unidirstat::scanner::DirectoryScanStatus::Cancelled, false, true);
                            } else {
                                live_scan::paint_scan_status(ui, node.status, snapshot.is_none() && self.control.is_paused(), self.settings.reduced_motion);
                            }
                        });
                        let icon = egui::Rect::from_min_size(
                            disclosure.right_top() + Vec2::new(23.0, 2.0),
                            Vec2::splat(16.0),
                        );
                        file_icon(
                            &painter,
                            icon,
                            node.directory,
                            self.file_colors.color(&node.extension),
                        );
                        let label_rect = egui::Rect::from_min_max(
                            egui::pos2(icon.right() + 7.0, rect.top()),
                            egui::pos2(name_rect.right() - 8.0, rect.bottom()),
                        );
                        let label = if self.page == Page::Largest || has_filters(&self.search_query)
                        {
                            &node.path
                        } else {
                            &node.name
                        };
                        paint_label(ui, label_rect, label, ui.visuals().text_color(), BODY_SIZE);
                        let mut left = rect.left() + name_width;
                        for column in 1..widths.len() {
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
                                        format_bytes(node.size),
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
                                3 => {
                                    let share = node.share;
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
                                _ => {
                                    let duration = live_scan::duration_label(node.duration);
                                    ui.put(
                                        cell.shrink2(Vec2::new(8.0, 0.0)),
                                        egui::Label::new(
                                            RichText::new(duration)
                                                .monospace()
                                                .size(HELP_SIZE)
                                                .color(muted(ui)),
                                        )
                                        .truncate(),
                                    )
                                    .on_hover_text(tr(
                                        "Folder time includes its descendants. Parallel times are not added together.",
                                    ));
                                }
                            }
                            left += widths[column];
                        }
                        self.tree_key(&response, node, snapshot.is_none(), ui.id());
                        if let Some(snapshot) = snapshot {
                            self.item_response(response.clone(), index, snapshot, SelectionView::Files);
                        } else if response.clicked() {
                            response.request_focus();
                            let modifiers = ui.input(|input| input.modifiers);
                            self.selection.click(index, &self.rows, modifiers.command || modifiers.ctrl, modifiers.shift);
                            self.selected = self.selection.primary();
                        }
                        response.on_hover_ui(|ui| {
                            ui.label(&node.path);
                            if node.limited { ui.label(tr("Depth limit reached. Contents were not scanned; sizes are incomplete.")); }
                            ui.label(trf("{parent}% of parent | {total}% of scan", &[
                                ("parent", format!("{:.1}", node.share * 100.0)),
                                ("total", format!("{:.1}", node.total_share * 100.0)),
                            ]));
                        });
                    }
                    if snapshot.is_none() { self.live_tree.visible = rows; }
                });
            });
        });
    }

    fn tree_key(
        &mut self,
        response: &egui::Response,
        node: &live_scan::FileRow,
        live: bool,
        parent_id: egui::Id,
    ) {
        let index = node.index;
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
                if live {
                    self.live_expansion_changed();
                } else {
                    self.rebuild = true;
                }
                None
            } else {
                node.parent
            }
        } else if input.3 {
            if node.directory && !self.expanded.contains(&index) {
                self.expanded.insert(index);
                if live {
                    self.live_expansion_changed();
                } else {
                    self.rebuild = true;
                }
                None
            } else {
                self.rows
                    .get(position + 1)
                    .filter(|row| row.1 == node.depth + 1)
                    .map(|row| row.0)
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
    fn idle_summary_rows_do_not_paint_button_boxes() {
        let context = egui::Context::default();
        let mut row = egui::Rect::NOTHING;
        let output = context.run(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                row = summary_row(ui, "zip", "1.25 GiB", false).rect;
            });
        });
        assert!(
            !output.shapes.iter().any(|shape| {
                matches!(&shape.shape, egui::Shape::Rect(rect) if rect.rect == row)
            })
        );
    }

    fn summary_fixture(
        context: &egui::Context,
    ) -> (tempfile::TempDir, UniDirStatApp, Arc<Snapshot>) {
        let fixture = tempfile::tempdir().expect("fixture");
        for index in 0..8 {
            let name = format!(
                "archive-name-that-cannot-fit-in-a-summary-{index}.extraordinarily-long-extension-{index}"
            );
            std::fs::write(
                fixture.path().join(name),
                vec![0; if index == 0 { 8192 } else { 1024 }],
            )
            .expect("file");
        }
        let snapshot = Arc::new(
            scanner::scan(
                &[fixture.path().into()],
                &ScanOptions::default(),
                &ScanControl::default(),
            )
            .expect("scan"),
        );
        let mut app = UniDirStatApp::new(
            &eframe::CreationContext::_new_kittest(context.clone()),
            Vec::new(),
            None,
            ScanOptions::default(),
            None,
        );
        app.analysis_complete = true;
        app.map_worker
            .request_summary(None, Arc::clone(&snapshot), context);
        let deadline = Instant::now() + Duration::from_secs(2);
        while app.map_worker.summary_pending() && Instant::now() < deadline {
            app.map_worker.poll();
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(app.map_worker.summary().is_some());
        context.set_zoom_factor(1.0);
        (fixture, app, snapshot)
    }

    #[test]
    fn scanning_table_never_interprets_new_rows_against_an_old_snapshot() {
        let context = egui::Context::default();
        let (fixture, mut app, old_snapshot) = summary_fixture(&context);
        for index in 0..20 {
            std::fs::write(fixture.path().join(format!("new-{index}")), b"new").unwrap();
        }
        app.control.enable_live_view();
        let new_snapshot = Arc::new(
            scanner::scan(
                &[fixture.path().into()],
                &ScanOptions::default(),
                &app.control,
            )
            .unwrap(),
        );
        app.rows = vec![(new_snapshot.nodes.len() - 1, 1)];
        app.control.set_completed_live_snapshot(new_snapshot);
        app.job_kind = Some(JobKind::Scan);
        let _ = context.run(Default::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| app.file_table(ui, &old_snapshot));
        });
    }

    #[test]
    fn live_tree_uses_the_completed_table_columns() {
        let context = egui::Context::default();
        let (fixture, mut app, _) = summary_fixture(&context);
        app.control.enable_live_view();
        let snapshot = scanner::scan(
            &[fixture.path().into()],
            &ScanOptions::default(),
            &app.control,
        )
        .unwrap();
        app.control.set_completed_live_snapshot(Arc::new(snapshot));
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(1400.0, 800.0),
                )),
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| app.live_files(ui));
            },
        );
        for label in ["Name", "Size", "Modified", "Share", "Scan time"] {
            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == tr(label))), "missing {label}");
        }
    }

    #[test]
    fn final_file_table_displays_recorded_scan_time() {
        let context = egui::Context::default();
        let (_fixture, mut app, mut snapshot) = summary_fixture(&context);
        let root = snapshot.roots[0];
        Arc::make_mut(&mut snapshot).nodes[root].scan_duration_ns = 1_250_000_000;
        app.rows = vec![(root, 0)];
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(1000.0, 350.0),
                )),
                ..egui::RawInput::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| app.file_table(ui, &snapshot));
            },
        );
        assert!(output.shapes.iter().any(|shape| {
            matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == "0:01.2")
        }));
    }

    #[test]
    fn summary_long_names_do_not_collide_with_sizes() {
        let context = egui::Context::default();
        let (_fixture, mut app, snapshot) = summary_fixture(&context);
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(220.0, 800.0),
                )),
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    app.workspace_summary(ui, &snapshot);
                });
            },
        );
        let text: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some((shape.clip_rect, text)),
                _ => None,
            })
            .collect();
        for label in [
            "extraordinarily-long-extension-0",
            "archive-name-that-cannot-fit-in-a-summary-0.extraordinarily-long-extension-0",
        ] {
            let (clip, name) = text
                .iter()
                .find(|(_, text)| text.galley.job.text == label)
                .expect("summary name");
            let name_rect = name.visual_bounding_rect().intersect(*clip);
            let (clip, bytes) = text
                .iter()
                .find(|(_, text)| {
                    text.galley.job.text == format_bytes(8192)
                        && (text.visual_bounding_rect().center().y - name_rect.center().y).abs()
                            < 3.0
                })
                .expect("size stays on the same row as its name");
            let bytes_rect = bytes.visual_bounding_rect();
            assert!(
                clip.contains_rect(bytes_rect),
                "size stays fully visible: {bytes_rect:?} within {clip:?}"
            );
            assert!(name_rect.right() + 4.0 <= bytes_rect.left());
        }
    }

    #[test]
    fn summary_statistics_fit_in_a_short_panel() {
        let context = egui::Context::default();
        let (_fixture, mut app, snapshot) = summary_fixture(&context);
        let mut height = 0.0;
        let _ = context.run(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                ui.set_width(244.0);
                height = ui
                    .scope(|ui| app.workspace_summary(ui, &snapshot))
                    .response
                    .rect
                    .height();
            });
        });
        assert!(height <= 540.0, "summary height: {height}");
    }

    #[test]
    fn file_controls_leave_visible_entry_count_to_the_map_toolbar() {
        let context = egui::Context::default();
        let (_fixture, mut app, _) = summary_fixture(&context);
        app.rows = vec![(0, 0)];
        app.rebuild = false;
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(800.0, 600.0),
                )),
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| app.file_controls(ui));
            },
        );
        let count = trf("{count} visible entries", &[("count", "1".into())]);
        assert!(!output.shapes.iter().any(|shape| {
            matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text == count)
        }));
    }

    #[test]
    fn files_page_keeps_the_entry_count_without_a_map() {
        let context = egui::Context::default();
        let (_fixture, mut app, _) = summary_fixture(&context);
        app.page = Page::Files;
        app.rows = vec![(0, 0)];
        app.rebuild = false;
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(800.0, 600.0),
                )),
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| app.file_controls(ui));
            },
        );
        let count = trf("{count} visible entries", &[("count", "1".into())]);
        assert!(output.shapes.iter().any(|shape| {
            matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text == count)
        }));
    }

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
