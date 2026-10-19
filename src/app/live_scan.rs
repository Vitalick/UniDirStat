use super::*;
use unidirstat::scanner::DirectoryScanStatus;

#[derive(Default)]
pub(super) struct LiveTree {
    rows: Vec<(usize, usize)>,
    pub(super) expanded: HashSet<usize>,
    nodes: usize,
    finalized: bool,
    refreshed: Option<Instant>,
    walk: Vec<WalkFrame>,
    dirty: bool,
    visible: Vec<LiveRow>,
}

struct WalkFrame {
    index: Option<usize>,
    depth: usize,
    next_child: usize,
}

impl LiveTree {
    fn start_rows(&mut self, snapshot: &Snapshot, finalized: bool) {
        self.rows.clear();
        self.visible.clear();
        self.walk.clear();
        self.walk.push(WalkFrame {
            index: None,
            depth: 0,
            next_child: 0,
        });
        self.nodes = snapshot.nodes.len();
        self.finalized = finalized;
        self.dirty = false;
        self.refreshed = Some(Instant::now());
    }

    fn advance_rows(&mut self, snapshot: &Snapshot, limit: usize) {
        if self.walk.is_empty() {
            return;
        }
        let started = Instant::now();
        for _ in 0..limit {
            let Some(frame) = self.walk.last_mut() else {
                break;
            };
            let children = frame.index.map_or(snapshot.roots.as_slice(), |index| {
                snapshot.nodes[index].children.as_slice()
            });
            let Some(&index) = children.get(frame.next_child) else {
                self.walk.pop();
                continue;
            };
            frame.next_child += 1;
            let depth = frame.depth;
            self.rows.push((index, depth));
            if self.expanded.contains(&index) {
                self.walk.push(WalkFrame {
                    index: Some(index),
                    depth: depth + 1,
                    next_child: 0,
                });
            }
            if started.elapsed() >= Duration::from_millis(2) {
                break;
            }
        }
        if self.walk.is_empty() {
            self.refreshed = Some(Instant::now());
        }
    }
}

struct LiveRow {
    index: usize,
    depth: usize,
    name: String,
    size: u64,
    directory: bool,
    status: DirectoryScanStatus,
    duration: Option<Duration>,
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

fn paint_scan_status(
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
        DirectoryScanStatus::Failed => {
            ui.painter().text(
                center,
                egui::Align2::CENTER_CENTER,
                "!",
                FontId::proportional(BODY_SIZE),
                ERROR_TEXT[dark],
            );
            tr("Scan issues")
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
        let tree = &mut self.live_tree;
        let allocated = self.settings.allocated;
        control.with_live_view(|view| {
            let due = tree
                .refreshed
                .is_none_or(|last| last.elapsed() >= Duration::from_millis(400));
            if tree.dirty
                || tree.finalized != view.is_finalized()
                || (tree.walk.is_empty() && tree.nodes != view.snapshot.nodes.len() && due)
            {
                if tree.refreshed.is_none() {
                    tree.expanded.extend(&view.snapshot.roots);
                }
                tree.start_rows(view.snapshot, view.is_finalized());
            }
            tree.advance_rows(view.snapshot, 4096);
        });
        ui.label(tr("Scan results update as folders are discovered."));
        ui.horizontal(|ui| {
            ui.strong(tr("Name"));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_sized([112.0, 24.0], egui::Label::new(tr("Scan time")));
                ui.add_sized([120.0, 24.0], egui::Label::new(tr("Size")));
            });
        });
        ui.separator();
        if tree.rows.is_empty() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(tr("Discovering folders"));
            });
        }
        egui::ScrollArea::both().id_salt("live-files").auto_shrink([false, false]).show_rows(ui, ROW_HEIGHT, tree.rows.len(), |ui, range| {
            let updated = control.with_live_view(|view| {
                range.clone().filter_map(|row| {
                    let (index, depth) = *tree.rows.get(row)?;
                    let node = view.snapshot.nodes.get(index)?;
                    Some(LiveRow {
                        index, depth, name: node.name(), size: node.size(allocated),
                        directory: node.kind == EntryKind::Directory,
                        status: view.directory_status(index).unwrap_or(if node.error.is_some() { DirectoryScanStatus::Failed } else { DirectoryScanStatus::Complete }),
                        duration: view.scan_duration(index),
                    })
                }).collect::<Vec<_>>()
            });
            if let Some(rows) = updated { tree.visible = rows; }
            for row in &tree.visible {
                ui.push_id(row.index, |ui| {
                ui.allocate_ui_with_layout(Vec2::new(ui.available_width().max(480.0), ROW_HEIGHT), Layout::left_to_right(Align::Center), |ui| {
                    ui.add_space((row.depth as f32 * 18.0).min(360.0));
                    if row.directory {
                        let expanded = tree.expanded.contains(&row.index);
                        let label = if expanded { tr("Collapse folder") } else { tr("Expand folder") };
                        let (rect, response) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
                        workspace::triangle(ui.painter(), rect.center(), if expanded { Vec2::Y } else { Vec2::X }, muted(ui));
                        workspace::focus_outline(ui, &response);
                        let response = response.on_hover_text(label);
                        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
                        if response.clicked() {
                            if !tree.expanded.remove(&row.index) { tree.expanded.insert(row.index); }
                            tree.dirty = true;
                        }
                    } else { ui.add_space(24.0); }
                    paint_scan_status(ui, row.status, control.is_paused(), self.settings.reduced_motion);
                    let (icon_rect, _) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
                    workspace::file_icon(ui.painter(), icon_rect, row.directory, muted(ui));
                    let width = (ui.available_width() - 248.0).max(100.0);
                    let (name_rect, name_response) = ui.allocate_exact_size(Vec2::new(width, ROW_HEIGHT), Sense::hover());
                    workspace::paint_label(ui, name_rect, &row.name, ui.visuals().text_color(), BODY_SIZE);
                    name_response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &row.name));
                    name_response.on_hover_text(&row.name);
                    ui.add_sized([120.0, ROW_HEIGHT], egui::Label::new(format_bytes(row.size)).halign(Align::Max));
                    ui.add_sized([112.0, ROW_HEIGHT], egui::Label::new(duration_label(row.duration)).halign(Align::Max)).on_hover_text(tr("Folder time includes its descendants. Parallel times are not added together."));
                });
                });
            }
        });
        ui.ctx().request_repaint_after(Duration::from_millis(200));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn live_row_walk_is_bounded_and_resumes_without_copying_a_wide_sibling_list() {
        let temporary = tempfile::tempdir().unwrap();
        for index in 0..12 {
            std::fs::write(temporary.path().join(format!("file-{index}")), [7; 10]).unwrap();
        }
        let snapshot = scanner::scan(
            &[temporary.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        let mut tree = LiveTree::default();
        tree.expanded.insert(snapshot.roots[0]);
        tree.start_rows(&snapshot, false);
        tree.advance_rows(&snapshot, 2);
        assert_eq!(tree.rows.len(), 2);
        assert!(tree.walk.len() <= 2);
        while !tree.walk.is_empty() {
            tree.advance_rows(&snapshot, 2);
        }
        assert_eq!(tree.rows.len(), snapshot.nodes.len());
        assert_eq!(
            tree.rows
                .iter()
                .map(|&(index, _)| index)
                .collect::<HashSet<_>>()
                .len(),
            snapshot.nodes.len()
        );
        tree.expanded.clear();
        tree.start_rows(&snapshot, true);
        tree.advance_rows(&snapshot, 2);
        assert_eq!(tree.rows, vec![(snapshot.roots[0], 0)]);
    }

    #[test]
    fn timing_distinguishes_unavailable_submillisecond_and_long_scans() {
        assert_eq!(duration_label(None), tr("Unavailable"));
        assert_eq!(duration_label(Some(Duration::from_nanos(10))), tr("<1 ms"));
        assert_eq!(duration_label(Some(Duration::from_millis(1250))), "0:01.2");
        assert_eq!(duration_label(Some(Duration::from_secs(125))), "2:05.0");
    }
}
