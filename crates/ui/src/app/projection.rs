use super::*;

/// Bidirectional projection used by camera overlays and drag-to-place.
/// Production geometry is always expressed in physical millimeters; the
/// homography remains a diagnostic-only fallback for camera overlays.
// Short-lived per-frame value, never stored in bulk — the variant size gap
// (polynomial maps vs two 3×3 homographies) has no practical cost.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub(super) enum CameraProjection {
    /// Commanded machine mm ↔ camera px. Machine-space overlays use the
    /// complete command→physical→camera chain so their curves show where
    /// the field optics actually land each commanded point.
    CommandedField {
        lens: vision::LensMap,
        frame: calib::Rigid2,
        field: vision::FieldMap,
        planes: PlaneShift,
    },
    /// Desired physical mm ↔ camera px. Production export applies
    /// physical→commanded field warping exactly once afterward.
    PhysicalLens {
        lens: vision::LensMap,
        frame: calib::Rigid2,
        planes: PlaneShift,
    },
    Homography {
        mm_to_px: vision::Homography,
        px_to_mm: vision::Homography,
    },
}

/// The height compensation applied between the ① lens map and everything
/// downstream of it.
///
/// Which plane each map lives on, since the whole correction is bookkeeping
/// about exactly that:
///
/// * `lens.px_to_mm` reads **on the ① calibration plane** — where the printed
///   paper grid lay. That plane is this struct's zero, whatever height the
///   operator recorded for the paper.
/// * `frame` (paper→machine) and `field` (physical→commanded) were both built
///   from camera readings of the ③ burned grid, which lay `field_mm` above the
///   paper. They are therefore keyed on ①-plane readings *of features at that
///   height*, not on true positions.
/// * The operator marks a surface `mark_mm` above the paper.
///
/// So a pixel is read on the ① plane, restated from the mark height into the
/// ③ height's reading convention, and only then handed to `frame`/`field`.
/// Both fields are DIFFERENCES against the ① paper height — the operator
/// enters three heights above the bed surface and only their differences ever
/// reach here. Positive is UPWARD, toward the camera. With the shipped
/// defaults (all three equal, hence `0.0`/`0.0`) the restatement is the
/// identity and every conversion is bit-for-bit what it was before this
/// existed.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct PlaneShift {
    /// `None` when the lens fit carries no usable perspective — the feature
    /// then applies nothing rather than guessing a tilt.
    pub(super) tilt: Option<calib::CameraTilt>,
    pub(super) mark_mm: f64,
    pub(super) field_mm: f64,
}

impl PlaneShift {
    /// Are we actually going to move anything? False with no tilt model, and
    /// false when the two planes coincide (including the default 0/0).
    pub(super) fn active(&self) -> bool {
        self.tilt.is_some() && (self.mark_mm - self.field_mm).abs() > 0.0
    }

    /// ①-plane reading of a point on the MARK surface → the reading the
    /// `frame`/`field` calibration is keyed on.
    fn to_cal(&self, paper: (f64, f64)) -> Option<(f64, f64)> {
        match &self.tilt {
            Some(t) => t.restate(paper, self.mark_mm, self.field_mm),
            None => Some(paper),
        }
    }

    /// The inverse, for the drawing direction.
    fn from_cal(&self, paper: (f64, f64)) -> Option<(f64, f64)> {
        match &self.tilt {
            Some(t) => t.restate(paper, self.field_mm, self.mark_mm),
            None => Some(paper),
        }
    }

    /// The displacement this shift applies at the centre of the region the ①
    /// fit covers, in paper mm — what the console quotes so a wrong derivation
    /// shows up as a number instead of a silent offset.
    pub(super) fn shift_at_center(&self) -> Option<(f64, f64)> {
        let t = self.tilt.as_ref()?;
        let moved = self.to_cal(t.center_mm)?;
        Some((moved.0 - t.center_mm.0, moved.1 - t.center_mm.1))
    }
}

impl CameraProjection {
    pub(super) fn to_px(&self, mm: (f64, f64)) -> Option<(f64, f64)> {
        let p = match self {
            Self::CommandedField {
                lens,
                frame,
                field,
                planes,
            } => {
                let physical = calib::commanded_to_physical(field, mm)?;
                let paper = planes.from_cal(finite_pair(frame.inverse_apply(physical))?)?;
                calib::paper_to_camera_px(lens, paper)?
            }
            Self::PhysicalLens {
                lens,
                frame,
                planes,
            } => {
                let paper = planes.from_cal(finite_pair(frame.inverse_apply(mm))?)?;
                calib::paper_to_camera_px(lens, paper)?
            }
            Self::Homography { mm_to_px, .. } => {
                let p = mm_to_px.apply(nalgebra::Point2::new(mm.0, mm.1));
                (p.x, p.y)
            }
        };
        finite_pair(p)
    }

    // The natural inverse of `to_px`; the `from_` prefix reads as direction,
    // not conversion-constructor.
    #[allow(clippy::wrong_self_convention)]
    pub(super) fn from_px(&self, px: (f64, f64)) -> Option<(f64, f64)> {
        let p = match self {
            Self::CommandedField {
                lens,
                frame,
                field,
                planes,
            } => {
                let paper = planes.to_cal(calib::camera_px_to_paper(lens, px)?)?;
                let physical = finite_pair(frame.apply(paper))?;
                finite_pair(field.to_commanded.apply(physical.0, physical.1))?
            }
            Self::PhysicalLens {
                lens,
                frame,
                planes,
            } => {
                let paper = planes.to_cal(calib::camera_px_to_paper(lens, px)?)?;
                finite_pair(frame.apply(paper))?
            }
            Self::Homography { px_to_mm, .. } => {
                let p = px_to_mm.apply(nalgebra::Point2::new(px.0, px.1));
                (p.x, p.y)
            }
        };
        finite_pair(p)
    }
}

pub(super) fn finite_pair(p: (f64, f64)) -> Option<(f64, f64)> {
    (p.0.is_finite() && p.1.is_finite()).then_some(p)
}

impl ConsoleApp {
    pub(super) fn initial_center_mm(&self, w_px: f64, h_px: f64) -> Result<(f64, f64), String> {
        let projection = self.place_projection(w_px as u32, h_px as u32)?;
        projection
            .from_px((w_px / 2.0, h_px / 2.0))
            .ok_or_else(|| "active camera projection returned a non-finite center".into())
    }

    pub(super) fn sync_auto_field_center(&mut self) {
        if self.camera.field_center_auto {
            let center = self.camera.field_mm / 2.0;
            self.camera.field_cx_mm = center;
            self.camera.field_cy_mm = center;
        }
    }

    #[cfg(test)]
    pub(super) fn place_homography(&self) -> Option<vision::Homography> {
        match &self.calibration.anchor {
            Some(calibration) => calibration.px_to_mm.try_inverse(),
            None => self.fiducials.homography.clone(),
        }
    }

    pub(super) fn nonlinear_maps_for_frame(
        &self,
        dimensions: (u32, u32),
    ) -> Result<Option<(vision::LensMap, calib::Rigid2, vision::FieldMap)>, String> {
        if !self.calibration.field_accepted {
            return Ok(None);
        }
        let lens = self
            .calibration
            .lens
            .as_ref()
            .ok_or("accepted laser field has no camera-lens calibration")?;
        let field = self
            .calibration
            .field
            .as_ref()
            .ok_or("accepted laser field calibration is missing")?;
        let signature = (dimensions, self.camera.orientation);
        if self.calibration.lens_frame_signature != Some(signature) {
            return Err(format!(
                "nonlinear calibration is for {:?}, current frame is {:?}; match resolution/crop/orientation and do not move the camera",
                self.calibration.lens_frame_signature, signature
            ));
        }
        if !calib::composed_projection_is_finite(&lens.lens, &field.field)
            || !field.paper_to_machine.is_finite()
        {
            return Err(
                "nonlinear camera/field calibration contains non-finite coefficients".into(),
            );
        }
        Ok(Some((
            lens.lens.clone(),
            field.paper_to_machine,
            field.field.clone(),
        )))
    }

    /// The camera's view-ray geometry above the ① calibration plane, derived
    /// from the stored lens fit. `None` when there is no lens map or the fit
    /// carries no usable perspective terms.
    ///
    /// Derived on demand rather than cached: it is one small homography fit
    /// per projection construction (a handful per frame), and caching it would
    /// mean invalidating a copy at every site that swaps the lens map.
    pub(super) fn camera_tilt(&self) -> Option<calib::CameraTilt> {
        calib::camera_tilt_from_lens(&self.calibration.lens.as_ref()?.lens)
    }

    /// The configured height compensation, with the tilt model resolved. The
    /// operator's three heights are all above the bed surface; the ① paper
    /// height is what the lens map reads on, so it is the origin everything
    /// else is stated against here.
    pub(super) fn plane_shift(&self) -> PlaneShift {
        let paper = self.calibration.paper_height_mm;
        PlaneShift {
            tilt: self.camera_tilt(),
            mark_mm: self.fiducials.surface_height_mm - paper,
            field_mm: self.calibration.laser_height_mm - paper,
        }
    }

    /// One line describing the height compensation actually in force, and
    /// whether it is doing anything. Reported beside the surface height on the
    /// fiducial-check tab and in `debug_summary` so the derived geometry is
    /// checkable rather than trusted: a tilt or standoff that does not match
    /// the bench is visible here before it quietly moves a burn.
    pub(super) fn height_comp_status(&self) -> (String, bool) {
        let planes = self.plane_shift();
        let Some(tilt) = planes.tilt else {
            return (
                "no tilt model in the lens fit — height compensation inactive".into(),
                false,
            );
        };
        // Report the foreshortened axis in MACHINE bearing when the burned-grid
        // frame is known; the paper's pose in the view carries no meaning.
        let bearing = match self.calibration.field.as_ref().map(|f| f.paper_to_machine) {
            Some(r) => tilt.bearing_deg(move |d| {
                let x = if r.flip_x { -d.0 } else { d.0 };
                (r.cos * x - r.sin * d.1, r.sin * x + r.cos * d.1)
            }),
            None => tilt.bearing_deg(|d| d),
        };
        let frame = if self.calibration.field.is_some() {
            "machine"
        } else {
            "paper"
        };
        let shift = planes.shift_at_center().unwrap_or((0.0, 0.0));
        (
            format!(
                "tilt {:.1}° @ {:.0} mm · foreshortened {bearing:+.0}° ({frame}) · \
                 vs the ① paper: surface {:+.2} mm, ③ grid {:+.2} mm \
                 → {:.3} mm at field centre",
                tilt.tilt_rad.to_degrees(),
                tilt.working_distance_mm,
                planes.mark_mm,
                planes.field_mm,
                shift.0.hypot(shift.1),
            ),
            planes.active(),
        )
    }

    pub(super) fn camera_projection(
        &self,
        dimensions: (u32, u32),
    ) -> Result<Option<CameraProjection>, String> {
        if let Some((lens, frame, field)) = self.nonlinear_maps_for_frame(dimensions)? {
            return Ok(Some(CameraProjection::CommandedField {
                lens,
                frame,
                field,
                planes: self.plane_shift(),
            }));
        }
        let Some(px_to_mm) = self.calibration.anchor.as_ref().map(|c| c.px_to_mm.clone()) else {
            return Ok(None);
        };
        let mm_to_px = px_to_mm
            .try_inverse()
            .ok_or("laser-anchor homography is singular")?;
        Ok(Some(CameraProjection::Homography { mm_to_px, px_to_mm }))
    }

    pub(super) fn place_projection(
        &self,
        width: u32,
        height: u32,
    ) -> Result<CameraProjection, String> {
        if let Some((lens, frame, _field)) = self.nonlinear_maps_for_frame((width, height))? {
            return Ok(CameraProjection::PhysicalLens {
                lens,
                frame,
                planes: self.plane_shift(),
            });
        }
        // Without an accepted nonlinear calibration, fall back to the saved
        // laser-anchor homography so the operator can still see the frame and
        // rough-place the job (labelled approximate). This is a viewing aid
        // only — "Etch here" independently refuses to export until ① Camera
        // lens + ③ Laser field are accepted, so no unwarped geometry can ship.
        let Some(px_to_mm) = self.calibration.anchor.as_ref().map(|c| c.px_to_mm.clone()) else {
            return Err(
                "Place needs a projection: accept step 1 (Camera lens) + step 3 (Laser field) (or at least step 2, Laser anchor, for an approximate preview)"
                    .into(),
            );
        };
        let mm_to_px = px_to_mm
            .try_inverse()
            .ok_or("laser-anchor homography is singular")?;
        Ok(CameraProjection::Homography { mm_to_px, px_to_mm })
    }
}
