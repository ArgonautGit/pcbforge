//! What the console writes into its diagnostic log ([`crate::diag`]).
//!
//! The sink is dumb; the records live here, next to the state they describe.
//! Every record is one line, `key=value`-ish, and every record belonging to one
//! fiducial check carries `check=N` — the check, the overlay it produced and
//! the export it fed are written by different code paths, frames (or a CLI
//! round-trip) apart, so `grep check=7` is what puts them next to each other.
//!
//! The rule that keeps this file usable: **nothing here may be called once per
//! UI frame**. Records come from operator actions and state changes only; the
//! one value that can move every frame (the overlay bbox) is guarded by
//! [`ConsoleApp::diag_overlay`]'s key + epsilon check.

use super::*;

/// How far the overlay's machine-mm bbox must move before it is worth another
/// record, mm. Below this it is a drag's sub-pixel jitter, not a new position.
const OVERLAY_EPS_MM: f64 = 0.05;

/// Short token for which camera→machine projection is active. The overlay and
/// the fiducial fit must be reading the same one; the variant is the first
/// thing to check when they disagree.
fn projection_token(p: &CameraProjection) -> &'static str {
    match p {
        CameraProjection::CommandedField { .. } => "commanded-field",
        CameraProjection::PhysicalLens { .. } => "physical-lens",
        CameraProjection::Homography { .. } => "homography",
    }
}

/// `x,y` pairs (or `-` for a point that has none), mm, in layout order.
fn fmt_points(pts: &[Option<(f64, f64)>]) -> String {
    pts.iter()
        .map(|p| match p {
            Some((x, y)) => format!("{x:.3},{y:.3}"),
            None => "-".into(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

impl ConsoleApp {
    /// Append one record. The single choke point, so a sink that has started
    /// failing is reported to the operator exactly once and then left alone.
    pub(super) fn diag(&mut self, text: &str) {
        self.runtime.diag.record(text);
        if self.runtime.diag.failed() && !self.runtime.diag_failure_reported {
            self.runtime.diag_failure_reported = true;
            let path = self.runtime.diag.path().display().to_string();
            self.runtime.log.push(LogLine {
                text: format!(
                    "diagnostics: couldn't write {path} — the console keeps running without a \
                     diagnostic log"
                ),
                err: true,
            });
        }
    }

    /// Record 1: the session header. Everything a later reader needs to know
    /// what this console was before the operator touched anything — including
    /// the calibration state, because a fiducial lock that reads clean against
    /// a stale lens fit is exactly the failure that leaves no trace on screen.
    pub(super) fn diag_startup(&mut self) {
        let profile = if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        };
        let source = match self.cam_source() {
            crate::camera::Source::Device(i) => format!("device {i}"),
            crate::camera::Source::File(p) if p.trim().is_empty() => "file (unset)".into(),
            crate::camera::Source::File(p) => format!("file {p}"),
        };
        // The pixel window the ① lens fit actually covers. A frame whose
        // fiducials sit outside it is being extrapolated, which reads as a
        // clean fit with a wrong answer.
        let bounds = match self
            .calibration
            .lens
            .as_ref()
            .and_then(|c| c.lens.calib_px_bounds)
        {
            Some([x0, y0, x1, y1]) => format!("{x0:.1},{y0:.1}..{x1:.1},{y1:.1}"),
            None => "none".into(),
        };
        let signature = match self.calibration.lens_frame_signature {
            Some(((w, h), o)) => format!("{w}x{h}/{}", o.token()),
            None => "none".into(),
        };
        let record = format!(
            "startup version={} profile={profile} db={} settings={} log={} camera_source={source} \
             field_accepted={} lens_frame_signature={signature} lens_calib_px_bounds={bounds}",
            env!("CARGO_PKG_VERSION"),
            self.db_path.display(),
            self.runtime.settings_path.display(),
            self.runtime.diag.path().display(),
            self.calibration.field_accepted,
        );
        self.diag(&record);
        // Tell the operator where it is, once, in the panel they already watch —
        // otherwise the file is only findable by knowing this code.
        let path = self.runtime.diag.path().display().to_string();
        self.runtime.log.push(LogLine {
            text: format!("diagnostics → {path} (previous session: {path}.prev)"),
            err: false,
        });
    }

    /// Record 5: mirror every failure line from the in-app Log into the file.
    ///
    /// Done by index rather than at the ~50 `log.push` sites: the console's
    /// error reporting is spread across every module, and a sweep that can
    /// never be forgotten is worth more than a call at each site.
    pub(super) fn diag_mirror_errors(&mut self) {
        // A sink that has already failed must not be fed the error line its own
        // failure produced — that is the one way this could loop.
        if self.runtime.diag.failed() {
            self.runtime.diag_mirrored = self.runtime.log.len();
            return;
        }
        // `pump_verb` trims the log from the front and adjusts the cursor with
        // it; clamp anyway so no future trim can slice out of bounds.
        let from = self.runtime.diag_mirrored.min(self.runtime.log.len());
        let pending: Vec<String> = self.runtime.log[from..]
            .iter()
            .filter(|l| l.err)
            .map(|l| format!("error {}", l.text))
            .collect();
        self.runtime.diag_mirrored = self.runtime.log.len();
        for line in pending {
            self.diag(&line);
        }
    }

    /// Record 2a: the inputs of one fiducial check. Opens a new `check=N`,
    /// which every record that follows from this check carries.
    pub(super) fn diag_check_begin(
        &mut self,
        layout: &str,
        dims: (u32, u32),
        projection: Result<&CameraProjection, &str>,
        detected: &[Option<(f64, f64)>],
    ) -> u64 {
        self.runtime.diag_check_seq += 1;
        let seq = self.runtime.diag_check_seq;
        let projection = match projection {
            Ok(p) => projection_token(p).to_string(),
            Err(e) => format!("unavailable ({e})"),
        };
        let record = format!(
            "fid-check check={seq} layout=\"{layout}\" frame={}x{} projection={projection} \
             side={:?} detected_machine_mm=[{}]",
            dims.0,
            dims.1,
            self.job.side,
            fmt_points(detected)
        );
        self.diag(&record);
        seq
    }

    /// Record 2b: the fit the check produced, before any gate has judged it.
    /// `b0` is the layout centroid — the point a fresh check lands the design
    /// on — so it is the machine-mm number the export bbox is compared against.
    pub(super) fn diag_check_fit(
        &mut self,
        b0: (f64, f64),
        pose: &crate::fiducial::BoardPose,
        detected_centroid: Option<(f64, f64)>,
    ) {
        let seq = self.runtime.diag_check_seq;
        let centroid = match detected_centroid {
            Some((x, y)) => format!("{x:.3},{y:.3}"),
            None => "none".into(),
        };
        let record = format!(
            "fid-check check={seq} layout_centroid_mm={:.3},{:.3} detected_centroid_mm={centroid} \
             fit scale={:.6} rot_deg={:+.4} tx={:.3} ty={:.3} flipped={} rms_mm={:.4} used={}",
            b0.0,
            b0.1,
            pose.scale,
            pose.rot_deg,
            pose.tx_mm,
            pose.ty_mm,
            pose.flipped,
            pose.rms_mm,
            pose.used,
        );
        self.diag(&record);
    }

    /// Record 2c: how the check ended — the gate that refused it, or `applied`
    /// plus the placement it wrote.
    pub(super) fn diag_check_outcome(&mut self, outcome: &str) {
        let seq = self.runtime.diag_check_seq;
        let record = format!(
            "fid-check check={seq} outcome={outcome} placement tx={:.3} ty={:.3} rot_deg={:+.4} \
             scale={:.6} pivot={:.3},{:.3} auto_pose={}",
            self.placement.tx_mm,
            self.placement.ty_mm,
            self.placement.rot_deg,
            self.placement.scale,
            self.placement.pivot.0,
            self.placement.pivot.1,
            self.placement.auto_pose,
        );
        self.diag(&record);
    }

    /// Record 2d: a canvas gesture on the fiducial frame began.
    ///
    /// Two records per completed gesture — this and
    /// [`diag_drag_stopped`](Self::diag_drag_stopped) — which is why this pair
    /// does not break the module's no-per-frame rule: a press and a release are
    /// operator actions, however many frames the drag between them spans.
    ///
    /// Carries `check=N` like the rest of the fiducial family: a drag that
    /// moved the job after check 7 is the thing a reader of check 7 needs to
    /// see, and without it the only trace was the placement affine itself.
    pub(super) fn diag_drag_started(&mut self, origin: &DragOrigin) {
        let seq = self.runtime.diag_check_seq;
        let record = format!(
            "fid-drag check={seq} phase=started target={} marker={} modifiers={} move_job_armed={} \
             start_px={:.1},{:.1} place tx={:.3} ty={:.3} rot_deg={:+.4}",
            origin.target,
            match origin.marker {
                Some(i) => i.to_string(),
                None => "-".into(),
            },
            origin.modifiers,
            origin.armed,
            origin.start_px.0,
            origin.start_px.1,
            origin.start_place.0,
            origin.start_place.1,
            origin.start_place.2,
        );
        self.diag(&record);
    }

    /// Record 2e: the same gesture released, with what it did to the placement.
    ///
    /// `place_delta_mm` is zero for every gesture that was not a job move —
    /// which is the point: a long drag across the design with `target=none`
    /// (unarmed) reads as an attempted pan that correctly moved nothing.
    pub(super) fn diag_drag_stopped(&mut self, origin: &DragOrigin, end_px: Option<(f64, f64)>) {
        let seq = self.runtime.diag_check_seq;
        let end = end_px.unwrap_or(origin.start_px);
        let (dx, dy) = (
            self.placement.tx_mm - origin.start_place.0,
            self.placement.ty_mm - origin.start_place.1,
        );
        let record = format!(
            "fid-drag check={seq} phase=stopped target={} modifiers={} move_job_armed={} \
             start_px={:.1},{:.1} end_px={end_x:.1},{end_y:.1} px_delta={:.1},{:.1} \
             place_delta_mm={dx:.3},{dy:.3} moved_mm={:.3} rot_delta_deg={:+.4}",
            origin.target,
            origin.modifiers,
            origin.armed,
            origin.start_px.0,
            origin.start_px.1,
            end.0 - origin.start_px.0,
            end.1 - origin.start_px.1,
            dx.hypot(dy),
            self.placement.rot_deg - origin.start_place.2,
            end_x = end.0,
            end_y = end.1,
        );
        self.diag(&record);
    }

    /// Record 3: where the drawn design actually sits, in machine mm.
    ///
    /// Two numbers, because they fail differently. The **affine** bbox is the
    /// design pushed through `Placement::affine()` — the common prefix of the
    /// overlay and the export, so comparing it with the exported file's bbox
    /// isolates the export leg. The **screen** centre is the drawn outline's
    /// bounding box read back through the projection, so comparing it with the
    /// affine centre isolates the projection leg. Between them, and the check's
    /// detected centroid, a reader can say which side is wrong.
    ///
    /// Called from the per-frame overlay, so it does nothing at all unless the
    /// placement changed AND the resulting bbox moved by `OVERLAY_EPS_MM`.
    pub(super) fn diag_overlay(
        &mut self,
        screen_bbox_center_px: Option<(f64, f64)>,
        dims: (u32, u32),
    ) {
        if self.placement.job.is_empty() {
            return;
        }
        let key = [
            self.placement.tx_mm,
            self.placement.ty_mm,
            self.placement.rot_deg,
            self.placement.scale,
            self.placement.pivot.0,
            self.placement.pivot.1,
            self.placement.job.len() as f64,
        ];
        // An unchanged placement cannot have moved the design, so this skips the
        // vertex sweep below as well as the record. It only saves the sweep
        // while the placement is STATIC — a moving one changes the key every
        // frame, and then the sweep costs about what the outline projection
        // beside it already does.
        if self.runtime.diag_overlay_key == Some(key) {
            return;
        }
        self.runtime.diag_overlay_key = Some(key);
        let a = self.placement().affine();
        let nm = NM_PER_MM as f64;
        let mut bbox: Option<[f64; 4]> = None;
        for poly in &self.placement.job {
            for ring in std::iter::once(&poly.outer).chain(poly.holes.iter()) {
                for p in ring {
                    let (gx, gy) = (p.x as f64 / nm, p.y as f64 / nm);
                    let (x, y) = (a[0] * gx + a[1] * gy + a[2], a[3] * gx + a[4] * gy + a[5]);
                    bbox = Some(match bbox {
                        None => [x, y, x, y],
                        Some([x0, y0, x1, y1]) => [x0.min(x), y0.min(y), x1.max(x), y1.max(y)],
                    });
                }
            }
        }
        let Some(b) = bbox else { return };
        let moved = match self.runtime.diag_overlay_bbox {
            None => true,
            Some(prev) => b
                .iter()
                .zip(prev.iter())
                .any(|(n, p)| (n - p).abs() > OVERLAY_EPS_MM),
        };
        if !moved {
            return;
        }
        self.runtime.diag_overlay_bbox = Some(b);
        let drawn = match screen_bbox_center_px
            .and_then(|px| self.place_projection(dims.0, dims.1).ok()?.from_px(px))
        {
            Some((x, y)) => format!("{x:.3},{y:.3}"),
            None => "none".into(),
        };
        let seq = self.runtime.diag_check_seq;
        let record = format!(
            "overlay check={seq} affine_bbox_mm={:.3},{:.3}..{:.3},{:.3} \
             affine_center_mm={:.3},{:.3} drawn_center_mm={drawn} affine=[{}]",
            b[0],
            b[1],
            b[2],
            b[3],
            (b[0] + b[2]) / 2.0,
            (b[1] + b[3]) / 2.0,
            a.iter()
                .map(|v| format!("{v:.6}"))
                .collect::<Vec<_>>()
                .join(","),
        );
        self.diag(&record);
    }

    /// Record 4a: an export, at the moment it is handed off.
    ///
    /// The placement snapshot, the affine it produced, the exact
    /// `correspondences()` string and the full argv — because the whole
    /// question is whether the placement the overlay drew is the placement the
    /// CLI was told about, and `correspondences()` is where the two paths part.
    pub(super) fn diag_export(
        &mut self,
        kind: &str,
        argv: &[String],
        out: &std::path::Path,
        field_warped: bool,
    ) {
        let seq = self.runtime.diag_check_seq;
        let placement = self.placement();
        let a = placement.affine();
        let record = format!(
            "export check={seq} kind={kind} out={} field_warped={field_warped} \
             placement tx={:.3} ty={:.3} rot_deg={:+.4} scale={:.6} pivot={:.3},{:.3} \
             affine=[{}] correspondences=\"{}\" argv=[{}]",
            out.display(),
            placement.tx_mm,
            placement.ty_mm,
            placement.rot_deg,
            placement.scale,
            placement.pivot_mm.0,
            placement.pivot_mm.1,
            a.iter()
                .map(|v| format!("{v:.6}"))
                .collect::<Vec<_>>()
                .join(","),
            placement.correspondences(),
            argv.join(" "),
        );
        self.diag(&record);
    }

    /// Arm the post-export readback ([`Self::diag_export_readback`]) for a verb
    /// that has actually been started. Only then: a refused click must not
    /// measure a file some earlier click wrote.
    pub(super) fn diag_arm_readback(
        &mut self,
        kind: &'static str,
        path: PathBuf,
        field_warped: bool,
    ) {
        self.runtime.diag_readback = Some(DiagReadback {
            path,
            kind,
            check: self.runtime.diag_check_seq,
            field_warped,
        });
    }

    /// Record 4b: what the export actually wrote — the geometry bbox of the
    /// `.lbrn2` on disk, in mm.
    ///
    /// Those coordinates are **commanded** mm when `field_warped` (the field map
    /// pre-distorts physical→commanded before writing) and **physical** mm
    /// otherwise. The distinction is not cosmetic: comparing a commanded bbox
    /// against a physical overlay bbox is itself a way to be wrong, so the
    /// record names which one it is rather than leaving the reader to guess.
    pub(super) fn diag_export_readback(&mut self, readback: &DiagReadback) {
        let record = match std::fs::read_to_string(&readback.path) {
            Ok(doc) => {
                let verts = crate::diag::lbrn2_verts(&doc);
                if verts.is_empty() {
                    format!(
                        "export-readback check={} kind={} out={} verts=0 (no geometry parsed)",
                        readback.check,
                        readback.kind,
                        readback.path.display()
                    )
                } else {
                    let (x0, y0, x1, y1) = crate::diag::verts_bbox(&verts);
                    format!(
                        "export-readback check={} kind={} out={} units={} verts={} \
                         bbox_mm={x0:.3},{y0:.3}..{x1:.3},{y1:.3} center_mm={:.3},{:.3}",
                        readback.check,
                        readback.kind,
                        readback.path.display(),
                        if readback.field_warped {
                            "commanded-mm"
                        } else {
                            "physical-mm"
                        },
                        verts.len(),
                        (x0 + x1) / 2.0,
                        (y0 + y1) / 2.0,
                    )
                }
            }
            Err(e) => format!(
                "export-readback check={} kind={} out={} unreadable: {e}",
                readback.check,
                readback.kind,
                readback.path.display()
            ),
        };
        self.diag(&record);
    }
}
