use super::*;

/// Bidirectional projection used by camera overlays and drag-to-place. The
/// nonlinear variants keep camera-lens and laser-field curvature intact; the
/// homography/uniform variants preserve the existing pre-calibration fallback.
#[derive(Debug, Clone)]
pub(super) enum CameraProjection {
    /// Commanded machine mm ↔ camera px. Used for the work area and for an
    /// uncorrected placement, whose coordinates are sent straight to the laser.
    CommandedNonlinear {
        lens: vision::LensMap,
        field: vision::FieldMap,
    },
    /// Desired physical mm ↔ camera px. Used when field correction is armed;
    /// the emit path applies physical→commanded exactly once afterward.
    PhysicalLens {
        lens: vision::LensMap,
    },
    Homography {
        mm_to_px: vision::Homography,
        px_to_mm: vision::Homography,
    },
    Uniform {
        px_per_mm: f64,
        frame_h: f64,
    },
}

impl CameraProjection {
    pub(super) fn to_px(&self, mm: (f64, f64)) -> Option<(f64, f64)> {
        let p = match self {
            Self::CommandedNonlinear { lens, field } => {
                crate::calib::commanded_to_camera_px(lens, field, mm)?
            }
            Self::PhysicalLens { lens } => crate::calib::physical_to_camera_px(lens, mm)?,
            Self::Homography { mm_to_px, .. } => {
                let p = mm_to_px.apply(nalgebra::Point2::new(mm.0, mm.1));
                (p.x, p.y)
            }
            Self::Uniform { px_per_mm, frame_h } => (mm.0 * px_per_mm, frame_h - mm.1 * px_per_mm),
        };
        finite_pair(p)
    }

    pub(super) fn from_px(&self, px: (f64, f64)) -> Option<(f64, f64)> {
        let p = match self {
            Self::CommandedNonlinear { lens, field } => {
                crate::calib::camera_px_to_commanded(lens, field, px)?
            }
            Self::PhysicalLens { lens } => crate::calib::camera_px_to_physical(lens, px)?,
            Self::Homography { px_to_mm, .. } => {
                let p = px_to_mm.apply(nalgebra::Point2::new(px.0, px.1));
                (p.x, p.y)
            }
            Self::Uniform { px_per_mm, frame_h } => {
                (px.0 / px_per_mm, (frame_h - px.1) / px_per_mm)
            }
        };
        finite_pair(p)
    }

    pub(super) fn label(&self) -> &'static str {
        match self {
            Self::CommandedNonlinear { .. } => "commanded mm (nonlinear lens + field)",
            Self::PhysicalLens { .. } => "physical mm (field-corrected)",
            Self::Homography { .. } => "machine mm (approximate homography)",
            Self::Uniform { .. } => "design frame (uncalibrated)",
        }
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

    pub(super) fn place_homography(&self) -> Option<vision::Homography> {
        // Approximate/fallback maps only. A quality-gated lens + field fit is
        // selected by `place_projection` instead so it never gets flattened
        // back into FieldCal::to_px.
        match &self.calibration.anchor {
            Some(c) => c.px_to_mm.try_inverse(),
            None => self.fiducials.homography.clone(),
        }
    }

    pub(super) fn nonlinear_maps_for_frame(
        &self,
        dimensions: (u32, u32),
    ) -> Result<Option<(vision::LensMap, vision::FieldMap)>, String> {
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
        if !crate::calib::composed_projection_is_finite(&lens.lens, &field.field) {
            return Err(
                "nonlinear camera/field calibration contains non-finite coefficients".into(),
            );
        }
        Ok(Some((lens.lens.clone(), field.field.clone())))
    }

    pub(super) fn camera_projection(
        &self,
        dimensions: (u32, u32),
    ) -> Result<Option<CameraProjection>, String> {
        if let Some((lens, field)) = self.nonlinear_maps_for_frame(dimensions)? {
            return Ok(Some(CameraProjection::CommandedNonlinear { lens, field }));
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
        if let Some((lens, field)) = self.nonlinear_maps_for_frame((width, height))? {
            return Ok(if self.placement.field_correct {
                CameraProjection::PhysicalLens { lens }
            } else {
                CameraProjection::CommandedNonlinear { lens, field }
            });
        }
        if self.placement.field_correct {
            return Err(
                "field correction is unavailable because the latest ③ fit was rejected or is stale"
                    .into(),
            );
        }
        if let Some(mm_to_px) = self.place_homography() {
            let px_to_mm = mm_to_px
                .try_inverse()
                .ok_or("placement homography is singular")?;
            return Ok(CameraProjection::Homography { mm_to_px, px_to_mm });
        }
        if !self.placement.px_per_mm.is_finite() || self.placement.px_per_mm <= 0.0 {
            return Err("px per mm must be finite and positive".into());
        }
        Ok(CameraProjection::Uniform {
            px_per_mm: self.placement.px_per_mm,
            frame_h: height as f64,
        })
    }
}
