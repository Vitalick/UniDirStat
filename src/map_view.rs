use eframe::egui::{Pos2, Rect, Vec2};

#[derive(Clone, Copy)]
pub(crate) struct MapViewport {
    pub scale: f32,
    pub offset: Vec2,
    target_scale: f32,
    target_offset: Vec2,
}

impl Default for MapViewport {
    fn default() -> Self {
        Self {
            scale: 1.0,
            offset: Vec2::ZERO,
            target_scale: 1.0,
            target_offset: Vec2::ZERO,
        }
    }
}

impl MapViewport {
    pub fn zoom_at(&mut self, anchor: Vec2, factor: f32, size: Vec2) {
        if !factor.is_finite() || factor <= 0.0 || !anchor.is_finite() {
            return;
        }
        let scale = (self.target_scale * factor).clamp(1.0, 64.0);
        self.target_offset = anchor - (anchor - self.target_offset) * (scale / self.target_scale);
        self.target_scale = scale;
        self.clamp(size);
    }

    pub fn reset(&mut self) {
        self.target_scale = 1.0;
        self.target_offset = Vec2::ZERO;
    }

    pub fn zoom_to(&mut self, rect: Rect, size: Vec2) {
        if !rect.is_finite() || rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        self.target_scale = (size.x / rect.width())
            .min(size.y / rect.height())
            .clamp(1.0, 64.0);
        self.target_offset = size / 2.0 - rect.center().to_vec2() * self.target_scale;
        self.clamp(size);
    }

    pub fn pan(&mut self, delta: Vec2, size: Vec2) {
        self.target_offset += delta;
        self.clamp(size);
    }

    fn clamp(&mut self, size: Vec2) {
        self.target_offset = self
            .target_offset
            .clamp(size * (1.0 - self.target_scale), Vec2::ZERO);
    }

    pub fn tick(&mut self, size: Vec2, seconds: f32, reduced_motion: bool) -> bool {
        self.clamp(size);
        let blend = if reduced_motion {
            1.0
        } else {
            1.0 - (-seconds.clamp(0.001, 0.1) * 18.0).exp()
        };
        self.scale += (self.target_scale - self.scale) * blend;
        self.offset += (self.target_offset - self.offset) * blend;
        let moving = (self.target_scale - self.scale).abs() > 0.001
            || (self.target_offset - self.offset).length() > 0.1;
        if !moving {
            self.scale = self.target_scale;
            self.offset = self.target_offset;
        }
        moving
    }

    pub fn rect(&self, rect: Rect, origin: Pos2) -> Rect {
        Rect::from_min_size(
            origin + self.offset + rect.min.to_vec2() * self.scale,
            rect.size() * self.scale,
        )
    }

    pub fn detail(&self) -> u32 {
        2_u32
            .pow(self.target_scale.log2().floor().max(0.0) as u32)
            .min(64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_zoom_keeps_the_pointer_over_the_same_content() {
        let mut view = MapViewport::default();
        let size = Vec2::new(800.0, 400.0);
        let pointer = Vec2::new(240.0, 120.0);
        view.zoom_at(pointer, 2.0, size);
        view.tick(size, 0.016, true);
        assert_eq!(view.scale, 2.0);
        assert_eq!(pointer * view.scale + view.offset, pointer);
    }

    #[test]
    fn double_click_target_animates_then_fills_the_view() {
        let mut view = MapViewport::default();
        let size = Vec2::new(800.0, 400.0);
        view.zoom_to(
            Rect::from_min_size(Pos2::new(200.0, 100.0), size / 2.0),
            size,
        );
        assert!(view.tick(size, 0.016, false));
        assert!(view.scale > 1.0 && view.scale < 2.0);
        view.tick(size, 0.016, true);
        assert_eq!(view.scale, 2.0);
        assert_eq!(view.offset, Vec2::new(-400.0, -200.0));
    }

    #[test]
    fn zoom_pan_and_reset_stay_inside_finite_bounds() {
        let mut view = MapViewport::default();
        let size = Vec2::new(800.0, 400.0);
        view.zoom_at(size / 2.0, 1_000.0, size);
        view.pan(Vec2::splat(-1_000_000.0), size);
        view.tick(size, 0.016, true);
        assert_eq!(view.scale, 64.0);
        assert_eq!(view.offset, size * -63.0);
        view.reset();
        view.tick(size, 0.016, true);
        assert_eq!(view.scale, 1.0);
        assert_eq!(view.offset, Vec2::ZERO);
    }
}
