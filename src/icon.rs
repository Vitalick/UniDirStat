pub fn window_icon() -> Result<eframe::egui::IconData, String> {
    eframe::icon_data::from_png_bytes(include_bytes!("../assets/unidirstat.png"))
        .map_err(|error| format!("Cannot decode the application icon: {error}"))
}

pub fn paint(painter: &eframe::egui::Painter, bounds: eframe::egui::Rect) {
    use eframe::egui::{self, Color32, ColorImage, Id, TextureHandle, TextureOptions};

    let identifier = Id::new("unidirstat-application-icon");
    let texture = painter
        .ctx()
        .data(|data| data.get_temp::<TextureHandle>(identifier))
        .or_else(|| {
            let icon = window_icon().ok()?;
            let texture = painter.ctx().load_texture(
                "UniDirStat",
                ColorImage::from(&icon),
                TextureOptions::LINEAR,
            );
            painter
                .ctx()
                .data_mut(|data| data.insert_temp(identifier, texture.clone()));
            Some(texture)
        });
    if let Some(texture) = texture {
        painter.image(
            texture.id(),
            bounds,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn window_icon_has_four_opaque_tiles_and_transparent_gaps() {
        let icon = super::window_icon().expect("embedded icon decodes");
        assert_eq!((icon.width, icon.height), (256, 256));
        for (horizontal, vertical) in [(64, 64), (192, 64), (64, 192), (192, 192)] {
            let offset = (vertical * 256 + horizontal) * 4;
            assert_eq!(&icon.rgba[offset..offset + 4], &[119, 131, 255, 255]);
        }
        for (horizontal, vertical) in [(0, 0), (128, 64), (64, 128), (128, 128)] {
            assert_eq!(icon.rgba[(vertical * 256 + horizontal) * 4 + 3], 0);
        }
    }

    #[test]
    fn painting_icon_uploads_its_texture_once() {
        use eframe::egui::{self, Id, TextureHandle};

        let context = egui::Context::default();
        let draw = |context: &egui::Context| {
            egui::CentralPanel::default().show(context, |ui| {
                super::paint(
                    ui.painter(),
                    egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::splat(24.0)),
                );
            });
        };
        let first_frame = context.run(egui::RawInput::default(), draw);
        assert!(
            first_frame
                .textures_delta
                .set
                .iter()
                .any(|(_, delta)| delta.image.size() == [256, 256])
        );
        let identifier = Id::new("unidirstat-application-icon");
        let texture = context
            .data(|data| data.get_temp::<TextureHandle>(identifier))
            .expect("cached icon");
        let second_frame = context.run(egui::RawInput::default(), draw);
        assert!(
            second_frame
                .textures_delta
                .set
                .iter()
                .all(|(texture_id, _)| *texture_id != texture.id())
        );
    }
}
