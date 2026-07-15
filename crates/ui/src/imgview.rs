//! A reusable zoom/pan wrapper for any image panel.
//!
//! The console shows several images the operator works on — the calibration
//! frame, the fiducial frame, the placement composite, the camera, the job
//! preview. This gives all of them the same navigation without disturbing their
//! existing interactions:
//!
//! * **Ctrl + drag** pans, **Ctrl + mouse wheel** zooms about the cursor.
//! * **Ctrl + double-click** resets to fit.
//! * Plain (no-Ctrl) click/drag is left untouched for the panel — placing and
//!   dragging markers still work exactly as before.
//!
//! A panel calls [`show`] with its texture and a persistent [`ImageView`], then
//! uses the returned [`ImageXform`] to map native-pixel ↔ screen for its
//! overlays and hit-testing (replacing the ad-hoc `rect`-based maths each panel
//! used to inline).

use egui::{Color32, Pos2, Rect, Response, Sense, TextureHandle, Ui, Vec2, pos2, vec2};

/// Persistent per-panel view: `zoom` is a multiple of the fit-to-panel scale
/// (`1.0` = the whole image fits), `pan` offsets the image centre from the
/// panel centre in screen pixels.
#[derive(Clone, Copy, Debug)]
pub struct ImageView {
    pub zoom: f32,
    pub pan: Vec2,
}

impl Default for ImageView {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan: Vec2::ZERO,
        }
    }
}

const MIN_ZOOM: f32 = 1.0;
const MAX_ZOOM: f32 = 24.0;

/// The transform for a given view, from the panel geometry and fit scale.
fn xform_of(view: &ImageView, panel: Rect, fit: f32, tw: f32, th: f32) -> ImageXform {
    let scale = fit * view.zoom;
    let size = vec2(tw, th) * scale;
    let img_min = panel.center() + view.pan - size * 0.5;
    ImageXform {
        panel,
        img_min,
        scale,
    }
}

/// Return the view that results from zooming by `factor` about `hover`, keeping
/// the image point under the cursor fixed. Zoom is clamped to `[MIN, MAX]`.
fn zoom_about(
    view: &ImageView,
    hover: Pos2,
    panel: Rect,
    fit: f32,
    tw: f32,
    th: f32,
    factor: f32,
) -> ImageView {
    let old = xform_of(view, panel, fit, tw, th);
    let (nat_x, nat_y) = old.to_native(hover);
    let new_zoom = (view.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
    let scale_new = fit * new_zoom;
    let size_new = vec2(tw, th) * scale_new;
    // Solve img_min so `hover` maps to the same native pixel, then back out pan.
    let pan = vec2(
        hover.x - panel.center().x + size_new.x * 0.5 - nat_x as f32 * scale_new,
        hover.y - panel.center().y + size_new.y * 0.5 - nat_y as f32 * scale_new,
    );
    ImageView {
        zoom: new_zoom,
        pan,
    }
}

/// The current mapping between native image pixels and screen coordinates, plus
/// the (clipped) panel rect the image was drawn in.
#[derive(Clone, Copy)]
pub struct ImageXform {
    /// The on-screen panel rect (the clip region for overlays).
    pub panel: Rect,
    /// Screen position of native pixel `(0, 0)`.
    pub img_min: Pos2,
    /// Screen pixels per native pixel.
    pub scale: f32,
}

impl ImageXform {
    /// Native image pixel → screen position.
    pub fn to_screen(self, px: f64, py: f64) -> Pos2 {
        pos2(
            self.img_min.x + px as f32 * self.scale,
            self.img_min.y + py as f32 * self.scale,
        )
    }

    /// Screen position → native image pixel.
    pub fn to_native(self, p: Pos2) -> (f64, f64) {
        (
            ((p.x - self.img_min.x) / self.scale) as f64,
            ((p.y - self.img_min.y) / self.scale) as f64,
        )
    }
}

/// Draw `tex` with the current `view`, handling Ctrl+drag pan / Ctrl+wheel zoom
/// / Ctrl+double-click reset. Returns the coordinate transform and the widget
/// response; the caller keeps its plain click/drag behaviour by gating on
/// [`is_navigating`] (i.e. acting only when Ctrl is *not* held).
pub fn show(ui: &mut Ui, tex: &TextureHandle, view: &mut ImageView) -> (ImageXform, Response) {
    let (tw, th) = (tex.size()[0] as f32, tex.size()[1] as f32);

    // Panel sized to the image aspect, fit within the available box (matching
    // the old `shrink_to_fit`).
    let avail = ui.available_size();
    let mut disp_w = avail.x.max(16.0);
    let mut disp_h = disp_w * th / tw;
    if avail.y.is_finite() && avail.y > 16.0 && disp_h > avail.y {
        disp_h = avail.y;
        disp_w = disp_h * tw / th;
    }
    let (panel, response) = ui.allocate_exact_size(vec2(disp_w, disp_h), Sense::click_and_drag());
    let fit = disp_w / tw; // == disp_h / th

    let ctrl = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);

    // Zoom about the cursor. egui folds Ctrl+wheel into `zoom_delta`; a raw
    // Ctrl+scroll is the fallback for platforms that don't.
    if response.hovered() {
        let (zdelta, scroll_y) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta.y));
        let factor = if (zdelta - 1.0).abs() > 1e-3 {
            zdelta
        } else if ctrl && scroll_y.abs() > 0.0 {
            (scroll_y * 0.005).exp()
        } else {
            1.0
        };
        if (factor - 1.0).abs() > 1e-6
            && let Some(hover) = response.hover_pos()
        {
            *view = zoom_about(view, hover, panel, fit, tw, th, factor);
        }
    }

    if ctrl && response.dragged() {
        view.pan += response.drag_delta();
    }
    if ctrl && response.double_clicked() {
        *view = ImageView::default();
    }
    // At fit, snap back to centred so the image can't drift off with no benefit.
    if (view.zoom - 1.0).abs() < 1e-4 {
        view.pan = Vec2::ZERO;
    }

    let xf = xform_of(view, panel, fit, tw, th);
    let painter = ui.painter_at(panel);
    painter.image(
        tex.id(),
        Rect::from_min_size(xf.img_min, vec2(tw, th) * xf.scale),
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
        Color32::WHITE,
    );

    (xf, response)
}

/// Whether the operator is currently driving pan/zoom (Ctrl held). Panels gate
/// their marker click/drag on `!is_navigating(ui)` so navigation never places
/// or moves a marker.
pub fn is_navigating(ui: &Ui) -> bool {
    ui.input(|i| i.modifiers.ctrl || i.modifiers.command)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 200×100-native image fit into a 400×200 panel (fit scale 2.0).
    fn setup() -> (Rect, f32, f32, f32) {
        let panel = Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 200.0));
        let (tw, th) = (200.0, 100.0);
        let fit = 400.0 / tw; // 2.0
        (panel, fit, tw, th)
    }

    #[test]
    fn xform_round_trips_native_and_screen() {
        let (panel, fit, tw, th) = setup();
        let view = ImageView::default();
        let xf = xform_of(&view, panel, fit, tw, th);
        // At fit, native (0,0) is the panel's top-left, native (tw,th) the
        // bottom-right.
        let tl = xf.to_screen(0.0, 0.0);
        assert!((tl.x - 0.0).abs() < 1e-3 && (tl.y - 0.0).abs() < 1e-3);
        let br = xf.to_screen(tw as f64, th as f64);
        assert!((br.x - 400.0).abs() < 1e-3 && (br.y - 200.0).abs() < 1e-3);
        // Round trip an arbitrary point.
        let (nx, ny) = xf.to_native(pos2(123.0, 77.0));
        let back = xf.to_screen(nx, ny);
        assert!((back.x - 123.0).abs() < 1e-3 && (back.y - 77.0).abs() < 1e-3);
    }

    #[test]
    fn zoom_keeps_the_point_under_the_cursor_fixed() {
        let (panel, fit, tw, th) = setup();
        let view = ImageView::default();
        let hover = pos2(310.0, 60.0);
        // The native pixel under the cursor before zooming.
        let before = xform_of(&view, panel, fit, tw, th).to_native(hover);
        // Zoom in 3×.
        let zoomed = zoom_about(&view, hover, panel, fit, tw, th, 3.0);
        assert!((zoomed.zoom - 3.0).abs() < 1e-4);
        let after = xform_of(&zoomed, panel, fit, tw, th).to_native(hover);
        // Same native pixel stays under the cursor.
        assert!(
            (after.0 - before.0).abs() < 0.05 && (after.1 - before.1).abs() < 0.05,
            "cursor-anchored zoom drifted: {before:?} → {after:?}"
        );
    }

    #[test]
    fn zoom_clamps_to_the_limits() {
        let (panel, fit, tw, th) = setup();
        let hover = panel.center();
        // Way past the max.
        let mut v = ImageView::default();
        for _ in 0..20 {
            v = zoom_about(&v, hover, panel, fit, tw, th, 2.0);
        }
        assert!(
            (v.zoom - MAX_ZOOM).abs() < 1e-3,
            "clamped to max: {}",
            v.zoom
        );
        // And never below the fit minimum.
        for _ in 0..20 {
            v = zoom_about(&v, hover, panel, fit, tw, th, 0.5);
        }
        assert!(
            (v.zoom - MIN_ZOOM).abs() < 1e-3,
            "clamped to min: {}",
            v.zoom
        );
    }
}
