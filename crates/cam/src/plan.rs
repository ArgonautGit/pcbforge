//! CAM-4 — pass planner.
//!
//! Splits an op's `params.passes` ablation passes into checkpointed
//! [`PassGroup`]s of `pp.group_size` passes each (the last group holds the
//! remainder), assigning each pass its hatch angle.
//!
//! # Conventions
//!
//! * **Angles.** Pass `k` (global, `0..params.passes`) uses hatch-angle set
//!   `k`, i.e. `opts.base_angle_deg + k * opts.fill_angle_step_deg` — the
//!   same formula as CAM-1's hatch sets (see
//!   [`crate::ablation::hatch_set_angle_deg`], which this module reuses).
//!   Callers drive `ablation_paths`'s `hatch_sets` argument from
//!   `params.passes`, so `PassSpec::pass_index` matches the
//!   `PathKind::Rubout(k)` tag of the geometry that pass traces.
//! * **Checkpoints.** Every group — including the last — ends at an operator
//!   checkpoint (`checkpoint == true`): the backlog's pass-group model has
//!   the operator inspect/measure after each emitted job file, and the final
//!   group's checkpoint gates the corrective-iteration loop
//!   (`pp.max_corrective_iters`).
//! * **Signature.** The task sketch is `plan(paths, params, pp)`; this
//!   implementation additionally takes `opts: &CamOpts` because the
//!   base/step hatch angles live there. `paths` is accepted (and currently
//!   only carried, not transformed): [`PassGroup`] embeds no geometry — job
//!   emission later pairs groups with the op's [`Paths`].
//!
//! # Panics
//!
//! `pp.group_size == 0` is a configuration error and panics with a clear
//! message (silently coercing to 1 would mask a bad `PassPlan`).

use pcb_core::{AblationParams, CamOpts, PassGroup, PassPlan, PassSpec, Paths};

use crate::ablation::hatch_set_angle_deg;

/// Split `params.passes` into checkpointed groups of `pp.group_size`.
///
/// Returns `params.passes / pp.group_size` full groups followed by one
/// remainder group if the division is inexact; `params.passes == 0` yields
/// an empty vec. Pass indices run `0..params.passes` monotonically across
/// groups, each carrying angle `opts.base_angle_deg + pass_index *
/// opts.fill_angle_step_deg`. Every group has `checkpoint == true` (see the
/// module docs).
///
/// # Panics
///
/// Panics if `pp.group_size == 0`.
pub fn plan(
    paths: &Paths,
    opts: &CamOpts,
    params: &AblationParams,
    pp: &PassPlan,
) -> Vec<PassGroup> {
    assert!(
        pp.group_size > 0,
        "PassPlan::group_size must be at least 1 (got 0)"
    );
    // Geometry is not transformed here; groups are paired with the op's
    // paths at job-emission time. Accepted per the task signature.
    let _ = paths;

    let specs: Vec<PassSpec> = (0..params.passes)
        .map(|k| PassSpec {
            pass_index: k,
            hatch_angle_deg: hatch_set_angle_deg(opts, k),
        })
        .collect();

    specs
        .chunks(pp.group_size as usize)
        .map(|chunk| PassGroup {
            passes: chunk.to_vec(),
            checkpoint: true,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(passes: u32) -> AblationParams {
        AblationParams {
            power_pct: 80.0,
            speed_mm_s: 800.0,
            frequency_khz: 45.0,
            pulse_ns: 200,
            passes,
        }
    }

    fn pass_plan(group_size: u32) -> PassPlan {
        PassPlan {
            group_size,
            max_corrective_iters: 3,
        }
    }

    /// base 0, step 17 — dyadic-friendly so angle math is exact in f64.
    fn opts() -> CamOpts {
        CamOpts {
            base_angle_deg: 0.0,
            fill_angle_step_deg: 17.0,
            ..CamOpts::default()
        }
    }

    fn group_sizes(groups: &[PassGroup]) -> Vec<usize> {
        groups.iter().map(|g| g.passes.len()).collect()
    }

    #[test]
    fn fourteen_passes_group_size_four() {
        let groups = plan(&Paths::default(), &opts(), &params(14), &pass_plan(4));
        assert_eq!(group_sizes(&groups), [4, 4, 4, 2]);

        // Pass indices run 0..14 monotonically across groups, and every
        // pass k carries exactly base + k * step (rotating monotonically).
        let flat: Vec<PassSpec> = groups.iter().flat_map(|g| g.passes.clone()).collect();
        assert_eq!(flat.len(), 14);
        for (i, spec) in flat.iter().enumerate() {
            assert_eq!(spec.pass_index, i as u32);
            assert_eq!(spec.hatch_angle_deg, i as f64 * 17.0, "pass {i}");
        }
        for w in flat.windows(2) {
            assert!(w[1].hatch_angle_deg > w[0].hatch_angle_deg);
        }
    }

    #[test]
    fn every_group_is_checkpointed_including_the_last() {
        let groups = plan(&Paths::default(), &opts(), &params(14), &pass_plan(4));
        assert!(groups.iter().all(|g| g.checkpoint));
    }

    #[test]
    fn exact_multiple_has_no_remainder_group() {
        let groups = plan(&Paths::default(), &opts(), &params(8), &pass_plan(4));
        assert_eq!(group_sizes(&groups), [4, 4]);
    }

    #[test]
    fn fewer_passes_than_group_size_yields_one_group() {
        let groups = plan(&Paths::default(), &opts(), &params(3), &pass_plan(4));
        assert_eq!(group_sizes(&groups), [3]);
    }

    #[test]
    fn zero_passes_yields_no_groups() {
        let groups = plan(&Paths::default(), &opts(), &params(0), &pass_plan(4));
        assert!(groups.is_empty());
    }

    #[test]
    fn nonzero_base_angle_is_honored() {
        let o = CamOpts {
            base_angle_deg: 11.25,
            fill_angle_step_deg: 6.5,
            ..CamOpts::default()
        };
        let groups = plan(&Paths::default(), &o, &params(5), &pass_plan(2));
        let flat: Vec<PassSpec> = groups.iter().flat_map(|g| g.passes.clone()).collect();
        for (i, spec) in flat.iter().enumerate() {
            assert_eq!(spec.hatch_angle_deg, 11.25 + i as f64 * 6.5, "pass {i}");
        }
    }

    #[test]
    #[should_panic(expected = "group_size must be at least 1")]
    fn zero_group_size_panics() {
        let _ = plan(&Paths::default(), &opts(), &params(4), &pass_plan(0));
    }
}
