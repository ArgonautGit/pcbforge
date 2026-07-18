//! The stage engine: resolve a board by its pallet tag, run the current
//! stage's executor, advance, and persist — one stage per [`step`] call.
//!
//! `pcbforge next` is a single [`step`]: it opens the DB, reads the pallet tag
//! via a [`PalletSource`], finds (or creates) that pallet's board, runs the
//! executor for the board's current stage, advances the board to the stage's
//! successor, and writes runlog rows. Because every call re-derives its state
//! from the DB, "separate process invocations" are just repeated `step` calls
//! against a freshly opened [`Db`] — state survives restarts.
//!
//! Executors are trait objects ([`StageExecutor`]) so the real Laser emission
//! (ORC-3/DRV-6) and the real clearance loop (ORC-3) can land later without
//! touching the engine. The three shipped here are bring-up stubs whose only
//! side effects are runlog rows, so the walk is fully test-observable.

use std::env;

use crate::db::{Board, Db};
use crate::stages::{StageDef, StageGraph, StageKind};

/// Environment variable naming the pallet's AprilTag ID for the stub source.
pub const ENV_PALLET_TAG: &str = "PCBFORGE_PALLET_TAG";

/// Environment variable marking the board on the bed as double-sided, read by
/// the bring-up [`FlipExecutor`] (`1`/`true` → the flip stage branches into the
/// bottom-side flow). The real signal becomes a board/design attribute once the
/// scheduler binds real designs.
pub const ENV_DOUBLE_SIDED: &str = "PCBFORGE_DOUBLE_SIDED";

/// The tag [`EnvPalletSource`] resolves to when `PCBFORGE_PALLET_TAG` is unset.
pub const DEFAULT_PALLET_TAG: i64 = 1;

/// Result alias for the engine.
pub type Result<T> = std::result::Result<T, EngineError>;

// ---- errors ----------------------------------------------------------------

/// Anything that can go wrong driving one stage.
#[derive(Debug)]
pub enum EngineError {
    /// A persistence-layer failure.
    Db(rusqlite::Error),
    /// The board sits at a stage name absent from the graph.
    UnknownStage(String),
    /// An executor chose the alternate branch on a stage without `next_alt`.
    NoAltSuccessor(String),
    /// The pallet tag could not be read.
    Pallet(PalletError),
    /// A configuration value (e.g. a bring-up env var) was malformed.
    Config(String),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::Db(e) => write!(f, "database error: {e}"),
            EngineError::UnknownStage(s) => {
                write!(f, "board is at stage `{s}`, which is not in the graph")
            }
            EngineError::NoAltSuccessor(s) => write!(
                f,
                "stage `{s}`'s executor branched (AdvanceAlt) but the stage has no `next_alt`"
            ),
            EngineError::Pallet(e) => write!(f, "could not read pallet tag: {e}"),
            EngineError::Config(m) => write!(f, "configuration error: {m}"),
        }
    }
}

impl std::error::Error for EngineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            EngineError::Db(e) => Some(e),
            EngineError::Pallet(e) => Some(e),
            EngineError::UnknownStage(_)
            | EngineError::NoAltSuccessor(_)
            | EngineError::Config(_) => None,
        }
    }
}

/// Strictly parse a bring-up boolean env var: `1`/`true`/`yes` → `Some(true)`,
/// `0`/`false`/`no` → `Some(false)` (case- and whitespace-insensitive), unset →
/// `None`. Anything else is an error so a typo (`on`, `y`, `tru`) surfaces
/// instead of silently defaulting — mirroring [`EnvPalletSource`] (LR-04).
fn parse_env_bool(var: &str) -> Result<Option<bool>> {
    match env::var(var) {
        Err(_) => Ok(None),
        Ok(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" => Ok(Some(true)),
            "0" | "false" | "no" => Ok(Some(false)),
            other => Err(EngineError::Config(format!(
                "{var}=`{other}` is not a boolean (use 1/0, true/false, or yes/no)"
            ))),
        },
    }
}

impl From<rusqlite::Error> for EngineError {
    fn from(e: rusqlite::Error) -> Self {
        EngineError::Db(e)
    }
}

impl From<PalletError> for EngineError {
    fn from(e: PalletError) -> Self {
        EngineError::Pallet(e)
    }
}

// ---- pallet source ---------------------------------------------------------

/// Failure to read a pallet tag.
#[derive(Debug)]
pub struct PalletError(pub String);

impl std::fmt::Display for PalletError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for PalletError {}

/// Reads the pallet tag (AprilTag ID) of the pallet currently on the bed.
///
/// The real implementation lands in VIS-11 (camera + AprilTag detection); this
/// environment has no camera/OpenCV, so the engine depends only on this trait
/// and ships stub implementations ([`EnvPalletSource`], [`FixedPalletSource`]).
pub trait PalletSource {
    fn read_tag(&self) -> std::result::Result<i64, PalletError>;
}

/// Stub pallet source: reads `PCBFORGE_PALLET_TAG`, falling back to a fixed
/// default when unset. A malformed value is an error (so typos surface rather
/// than silently resolving the wrong pallet).
#[derive(Debug, Clone)]
pub struct EnvPalletSource {
    pub default: i64,
}

impl Default for EnvPalletSource {
    fn default() -> Self {
        Self {
            default: DEFAULT_PALLET_TAG,
        }
    }
}

impl PalletSource for EnvPalletSource {
    fn read_tag(&self) -> std::result::Result<i64, PalletError> {
        match env::var(ENV_PALLET_TAG) {
            Ok(raw) => raw.trim().parse::<i64>().map_err(|_| {
                PalletError(format!("{ENV_PALLET_TAG}=`{raw}` is not a valid tag id"))
            }),
            Err(_) => Ok(self.default),
        }
    }
}

/// A pallet source that always yields a fixed tag (deterministic tests).
#[derive(Debug, Clone, Copy)]
pub struct FixedPalletSource(pub i64);

impl PalletSource for FixedPalletSource {
    fn read_tag(&self) -> std::result::Result<i64, PalletError> {
        Ok(self.0)
    }
}

// ---- board defaults --------------------------------------------------------

/// Fields used to materialize a board the first time a pallet is seen. The
/// real design binding (a queued job) arrives with the scheduler; for now a
/// placeholder design is enough to give the pallet a board to walk.
#[derive(Debug, Clone)]
pub struct BoardDefaults {
    pub design_path: String,
    pub design_hash: String,
}

impl Default for BoardDefaults {
    fn default() -> Self {
        Self {
            design_path: "designs/placeholder.kicad_pcb".to_owned(),
            design_hash: "0".repeat(64),
        }
    }
}

// ---- executor interface ----------------------------------------------------

/// What an executor decided after running against the current stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageOutcome {
    /// The stage is done; advance to its default successor (`next`).
    Advance,
    /// The stage is done via the branch: advance to `next_alt` (ORC-6's flip
    /// stage entering the bottom-side flow). An error if the stage has none.
    AdvanceAlt,
    /// The stage needs to run again (e.g. a clearance loop that has not
    /// converged); leave the board where it is.
    Stay,
    /// Stop here without advancing (e.g. an escalation).
    Halt,
}

/// Everything an executor may touch while running one stage. Executors record
/// their work as runlog rows via [`StageCtx::record`] and may checkpoint resume
/// data into [`StageCtx::stage_state`].
pub struct StageCtx<'a> {
    pub db: &'a Db,
    pub board_id: i64,
    pub stage: &'a str,
    pub def: &'a StageDef,
    /// Executor-owned JSON resume blob, persisted with the board after the run.
    pub stage_state: &'a mut String,
}

impl StageCtx<'_> {
    /// Append a runlog row for this stage (board id and stage name are filled
    /// in from the context).
    pub fn record(&self, event: &str, detail: &str) -> Result<()> {
        self.db
            .append_runlog(Some(self.board_id), self.stage, event, detail)?;
        Ok(())
    }
}

/// Drives one stage. Implementors are held as trait objects in the
/// [`ExecutorRegistry`] so later tasks can swap in real behaviour.
pub trait StageExecutor {
    fn run(&self, ctx: &mut StageCtx) -> Result<StageOutcome>;
}

/// Manual (operator) stage. Records the operator prompt and auto-advances.
///
/// In a real UI/CLI this stage would gate on operator confirmation (the
/// operator physically performs the step, e.g. registers fiducials, then
/// confirms). In this non-interactive engine it records the prompt and advances
/// so the walk can run unattended; the confirmation gate is a later UI concern.
pub struct ManualExecutor;

impl StageExecutor for ManualExecutor {
    fn run(&self, ctx: &mut StageCtx) -> Result<StageOutcome> {
        ctx.record("prompt", &json_detail(&[("prompt", &ctx.def.detail)]))?;
        Ok(StageOutcome::Advance)
    }
}

/// How [`LaserExecutor`] gates on extraction airflow before emitting.
///
/// The airflow interlock (`airflow::require_airflow`) must run on every
/// laser-emitting path — a low CTS means the sail switch sees no extraction and
/// the stage must not burn. Real emission (ORC-3/DRV-6) will always
/// [`Require`](AirflowGate::Require); the bring-up stub defaults to
/// [`Skip`](AirflowGate::Skip) but records an `airflow_skipped` row so the
/// ungated state is auditable and can't be silently forgotten (LR-01).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AirflowGate {
    /// Bring-up: don't touch hardware, but record that the interlock did NOT
    /// run so it shows up in the runlog.
    Skip,
    /// Production: verify airflow for the stage's machine and [`Halt`] on
    /// failure ([`StageOutcome::Halt`]).
    Require,
}

/// Laser stage. Checks the airflow interlock per its [`AirflowGate`], records
/// the intent to emit a job set for the stage's machine and process, then
/// advances. Real emission (compile + burn + wait) is wired in ORC-3/DRV-6.
pub struct LaserExecutor {
    /// Airflow interlock policy (see [`AirflowGate`]).
    pub airflow: AirflowGate,
}

impl StageExecutor for LaserExecutor {
    fn run(&self, ctx: &mut StageCtx) -> Result<StageOutcome> {
        let machine = ctx.def.machine.as_deref().unwrap_or("");
        let process = ctx.def.process.as_deref().unwrap_or("");

        match self.airflow {
            AirflowGate::Skip => {
                ctx.record(
                    "airflow_skipped",
                    &json_detail(&[
                        ("machine", machine),
                        (
                            "note",
                            "bring-up stub: airflow interlock NOT checked \
                             (ORC-3/DRV-6 sets AirflowGate::Require)",
                        ),
                    ]),
                )?;
            }
            AirflowGate::Require => {
                // Unknown machine = hard error, fail closed (never burn blind).
                let m = parse_machine(machine)?;
                match crate::airflow::require_airflow(m) {
                    Ok(()) => {
                        ctx.record("airflow_ok", &json_detail(&[("machine", machine)]))?;
                    }
                    Err(e) => {
                        ctx.record(
                            "airflow_blocked",
                            &json_detail(&[("machine", machine), ("error", &e.to_string())]),
                        )?;
                        return Ok(StageOutcome::Halt);
                    }
                }
            }
        }

        ctx.record(
            "emit_intent",
            &json_detail(&[("machine", machine), ("process", process)]),
        )?;
        Ok(StageOutcome::Advance)
    }
}

/// Map a stage's machine string to a [`Machine`]; unknown = fail-closed error.
fn parse_machine(s: &str) -> Result<pcb_core::Machine> {
    match s.trim().to_ascii_lowercase().as_str() {
        "fiber" => Ok(pcb_core::Machine::Fiber),
        "uv" => Ok(pcb_core::Machine::Uv),
        other => Err(EngineError::Config(format!(
            "laser stage has unknown machine `{other}` (expected `fiber` or `uv`)"
        ))),
    }
}

/// ClearanceLoop stage — STUB. Records a single pass-through and advances.
///
/// ORC-3 replaces this with the real closed inspect/correct loop (emit a
/// corrective pass group, re-inspect, iterate until isolation is clear or the
/// iteration budget is spent, possibly returning [`StageOutcome::Stay`] between
/// iterations). Until then it records that it passed through and advances.
pub struct ClearanceLoopExecutor;

impl StageExecutor for ClearanceLoopExecutor {
    fn run(&self, ctx: &mut StageCtx) -> Result<StageOutcome> {
        ctx.record(
            "clearance_stub",
            &json_detail(&[("note", "ORC-3 replaces this stub; passing through")]),
        )?;
        Ok(StageOutcome::Advance)
    }
}

/// How the [`FlipExecutor`] decides whether the board on the bed is
/// double-sided (whether the flip stage branches into the bottom-side flow).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlipMode {
    /// Always single-sided: the flip stage passes straight through (`next`).
    SingleSided,
    /// Always double-sided: prompt the flip and branch (`next_alt`).
    DoubleSided,
    /// Read [`ENV_DOUBLE_SIDED`] (the bring-up operator signal, mirroring
    /// [`EnvPalletSource`]): `1`/`true`/`yes` → double-sided.
    FromEnv,
}

impl FlipMode {
    fn is_double_sided(self) -> Result<bool> {
        match self {
            FlipMode::SingleSided => Ok(false),
            FlipMode::DoubleSided => Ok(true),
            // A malformed value must error, not fall back to single-sided and
            // silently skip the bottom side (scrapped board) (LR-04).
            FlipMode::FromEnv => Ok(parse_env_bool(ENV_DOUBLE_SIDED)?.unwrap_or(false)),
        }
    }
}

/// Flip decision stage (ORC-6). On a single-sided board it records that no
/// flip is needed and takes the default path. On a double-sided board it
/// prompts the physical flip and branches into the bottom-side stages,
/// recording that bottom registration must use **mirror-aware expected
/// coordinates**: the same drilled through-holes, mirrored across the flip
/// axis with the beam entry→exit parallax applied (`cam::flip` /
/// the console's Back side compute them; the operator confirms in the
/// fiducial-check view).
pub struct FlipExecutor {
    pub mode: FlipMode,
}

impl StageExecutor for FlipExecutor {
    fn run(&self, ctx: &mut StageCtx) -> Result<StageOutcome> {
        if self.mode.is_double_sided()? {
            ctx.record(
                "flip_prompt",
                &json_detail(&[
                    ("prompt", "Flip the board left-right for the back side"),
                    (
                        "registration",
                        "expect the through-holes at mirror-aware coordinates \
                         (mirror across the flip axis + beam entry-exit offset); \
                         use the console's Back side / cam::flip to compute them",
                    ),
                ]),
            )?;
            Ok(StageOutcome::AdvanceAlt)
        } else {
            ctx.record(
                "flip_skip",
                &json_detail(&[("note", "single-sided board; no flip")]),
            )?;
            Ok(StageOutcome::Advance)
        }
    }
}

/// Maps each [`StageKind`] to its executor.
pub struct ExecutorRegistry {
    manual: Box<dyn StageExecutor>,
    laser: Box<dyn StageExecutor>,
    clearance: Box<dyn StageExecutor>,
    flip: Box<dyn StageExecutor>,
}

impl ExecutorRegistry {
    /// The default registry: the bring-up executors shipped in this module.
    /// The flip decision reads [`ENV_DOUBLE_SIDED`].
    pub fn with_defaults() -> Self {
        Self {
            manual: Box::new(ManualExecutor),
            laser: Box::new(LaserExecutor {
                airflow: AirflowGate::Skip,
            }),
            clearance: Box::new(ClearanceLoopExecutor),
            flip: Box::new(FlipExecutor {
                mode: FlipMode::FromEnv,
            }),
        }
    }

    /// A registry with an explicit flip decision (deterministic tests — env
    /// vars are process-global and hazardous under parallel test runs).
    pub fn with_flip_mode(mode: FlipMode) -> Self {
        Self {
            flip: Box::new(FlipExecutor { mode }),
            ..Self::with_defaults()
        }
    }

    /// The executor for `kind`.
    pub fn get(&self, kind: StageKind) -> &dyn StageExecutor {
        match kind {
            StageKind::Manual => self.manual.as_ref(),
            StageKind::Laser => self.laser.as_ref(),
            StageKind::ClearanceLoop => self.clearance.as_ref(),
            StageKind::Flip => self.flip.as_ref(),
        }
    }
}

impl Default for ExecutorRegistry {
    fn default() -> Self {
        Self::with_defaults()
    }
}

// ---- the step --------------------------------------------------------------

/// The outcome of one [`step`] — what the engine did to the board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepReport {
    /// Ran `stage`'s executor and moved the board to `to`.
    Advanced {
        board_id: i64,
        stage: String,
        to: String,
    },
    /// Ran `stage`'s executor; the board stays put (needs another run).
    Stayed { board_id: i64, stage: String },
    /// Nothing to do: the board is at a terminal stage (or an executor halted).
    Halted { board_id: i64, stage: String },
}

impl StepReport {
    /// The board this step acted on.
    pub fn board_id(&self) -> i64 {
        match self {
            StepReport::Advanced { board_id, .. }
            | StepReport::Stayed { board_id, .. }
            | StepReport::Halted { board_id, .. } => *board_id,
        }
    }
}

/// Run one stage for the pallet currently on the bed.
///
/// Resolves the pallet by tag (creating pallet and board on first sight),
/// reads the board's current stage, and — unless the stage is terminal — writes
/// a `stage_start` row, runs the executor, and on [`StageOutcome::Advance`]
/// writes a `stage_done` row and persists the board at its successor. A
/// terminal stage is a no-op that reports [`StepReport::Halted`], so repeated
/// calls after completion are idempotent.
pub fn step(
    db: &Db,
    graph: &StageGraph,
    registry: &ExecutorRegistry,
    pallet: &dyn PalletSource,
    defaults: &BoardDefaults,
) -> Result<StepReport> {
    // Run the whole step — pallet/board resolution and the stage transition —
    // in one `BEGIN IMMEDIATE` transaction. This makes the stage_start /
    // executor / stage_done / update_board writes atomic (a crash can't leave
    // the runlog and board.stage disagreeing and re-run the stage — LR-09),
    // makes the new-board insert-then-set-entry atomic so no board is ever
    // persisted at the placeholder `'start'` stage (LR-10), and serializes
    // concurrent steppers so two `next`s can't both run and advance (LR-11).
    db.begin_immediate()?;
    let result = step_txn(db, graph, registry, pallet, defaults);
    match &result {
        Ok(_) => db.commit()?,
        Err(_) => {
            let _ = db.rollback();
        }
    }
    result
}

fn step_txn(
    db: &Db,
    graph: &StageGraph,
    registry: &ExecutorRegistry,
    pallet: &dyn PalletSource,
    defaults: &BoardDefaults,
) -> Result<StepReport> {
    let tag = pallet.read_tag()?;
    let pallet_id = resolve_pallet(db, tag)?;
    let mut board = resolve_board(db, graph, pallet_id, defaults)?;

    let stage_name = board.stage.clone();
    let def = graph
        .stage(&stage_name)
        .ok_or_else(|| EngineError::UnknownStage(stage_name.clone()))?
        .clone();

    // A terminal stage has no successor: the board is complete. Nothing to run.
    if def.is_terminal() {
        return Ok(StepReport::Halted {
            board_id: board.id,
            stage: stage_name,
        });
    }

    db.append_runlog(
        Some(board.id),
        &stage_name,
        "stage_start",
        &json_detail(&[("detail", &def.detail)]),
    )?;

    let mut stage_state = board.stage_state.clone();
    let outcome = {
        let mut ctx = StageCtx {
            db,
            board_id: board.id,
            stage: &stage_name,
            def: &def,
            stage_state: &mut stage_state,
        };
        registry.get(def.kind).run(&mut ctx)?
    };
    board.stage_state = stage_state;

    // Advance the board to `next`, writing the `stage_done` row.
    let advance_to = |mut board: Board, next: String| -> Result<StepReport> {
        db.append_runlog(
            Some(board.id),
            &stage_name,
            "stage_done",
            &json_detail(&[("to", &next)]),
        )?;
        board.stage = next.clone();
        db.update_board(&board)?;
        Ok(StepReport::Advanced {
            board_id: board.id,
            stage: stage_name.clone(),
            to: next,
        })
    };

    match outcome {
        StageOutcome::Advance => {
            // `is_terminal` was false above, so `next` is Some here.
            let next = def
                .next
                .clone()
                .expect("non-terminal stage has a successor");
            advance_to(board, next)
        }
        StageOutcome::AdvanceAlt => {
            // The branch path (ORC-6 flip → bottom-side flow). Unlike `next`,
            // `next_alt` is optional, so an executor branching on a stage
            // without one is a graph/executor mismatch — a hard error.
            let next = def
                .next_alt
                .clone()
                .ok_or_else(|| EngineError::NoAltSuccessor(stage_name.clone()))?;
            advance_to(board, next)
        }
        StageOutcome::Stay => {
            // Persist any checkpointed resume data; stage is unchanged.
            db.update_board(&board)?;
            Ok(StepReport::Stayed {
                board_id: board.id,
                stage: stage_name,
            })
        }
        StageOutcome::Halt => {
            db.update_board(&board)?;
            Ok(StepReport::Halted {
                board_id: board.id,
                stage: stage_name,
            })
        }
    }
}

/// Find the pallet with `tag`, creating a bare one if it is new.
fn resolve_pallet(db: &Db, tag: i64) -> Result<i64> {
    if let Some(p) = db.get_pallet_by_tag(tag)? {
        return Ok(p.id);
    }
    let p = db.insert_pallet(tag, "", 0, 0)?;
    Ok(p.id)
}

/// Find `pallet_id`'s in-flight board, creating one (parked at the graph entry)
/// if none exists yet. The most recently created board for the pallet is used.
fn resolve_board(
    db: &Db,
    graph: &StageGraph,
    pallet_id: i64,
    defaults: &BoardDefaults,
) -> Result<Board> {
    let existing = db
        .list_boards()?
        .into_iter()
        .rfind(|b| b.pallet_id == Some(pallet_id));
    if let Some(board) = existing {
        return Ok(board);
    }
    // New board: `insert_board` parks it at the schema default `'start'`; move
    // it to the graph entry so the persisted stage is always a real stage name.
    // Both statements run inside `step`'s transaction, so a crash between them
    // rolls the insert back entirely — no board is ever left at `'start'`
    // (which no stage graph contains, and would brick the pallet) (LR-10).
    let mut board = db.insert_board(
        Some(pallet_id),
        &defaults.design_path,
        &defaults.design_hash,
    )?;
    board.stage = graph.entry.clone();
    db.update_board(&board)?;
    Ok(board)
}

/// Build a tiny JSON object from string key/value pairs. Values are the small
/// literals executors record (stage names, machine/process ids, notes); this
/// escapes `"` and `\` so those stay valid JSON without pulling in serde_json.
fn json_detail(fields: &[(&str, &str)]) -> String {
    let mut out = String::from("{");
    for (i, (k, v)) in fields.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(k);
        out.push_str("\":\"");
        escape_into(v, &mut out);
        out.push('"');
    }
    out.push('}');
    out
}

fn escape_into(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Any other control char is invalid raw in a JSON string (LR-27).
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            _ => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_machine_is_fail_closed_on_unknown() {
        assert_eq!(parse_machine("fiber").unwrap(), pcb_core::Machine::Fiber);
        assert_eq!(parse_machine(" UV ").unwrap(), pcb_core::Machine::Uv);
        assert!(matches!(parse_machine("plasma"), Err(EngineError::Config(_))));
        assert!(matches!(parse_machine(""), Err(EngineError::Config(_))));
    }

    #[test]
    fn parse_env_bool_rejects_typos_and_reads_both_polarities() {
        // A uniquely-named var keeps this off the shared-env race surface.
        let var = "PCBFORGE_TEST_DOUBLE_SIDED_LR04";
        // SAFETY: single-threaded test body; var is unique to this test.
        unsafe {
            std::env::set_var(var, "on");
            assert!(matches!(parse_env_bool(var), Err(EngineError::Config(_))));
            std::env::set_var(var, "YES");
            assert_eq!(parse_env_bool(var).unwrap(), Some(true));
            std::env::set_var(var, "0");
            assert_eq!(parse_env_bool(var).unwrap(), Some(false));
            std::env::remove_var(var);
        }
        assert_eq!(parse_env_bool(var).unwrap(), None);
    }

    #[test]
    fn env_source_falls_back_to_default_when_unset() {
        // Not asserting on a set var (env is process-global and racy across
        // tests); the default path is the deterministic one to check.
        let src = EnvPalletSource { default: 7 };
        // Only meaningful if the var is unset in this process.
        if env::var(ENV_PALLET_TAG).is_err() {
            assert_eq!(src.read_tag().unwrap(), 7);
        }
    }

    #[test]
    fn fixed_source_yields_its_tag() {
        assert_eq!(FixedPalletSource(42).read_tag().unwrap(), 42);
    }

    #[test]
    fn json_detail_escapes_quotes_and_backslashes() {
        let s = json_detail(&[("a", "he said \"hi\""), ("b", "c:\\x")]);
        assert_eq!(s, r#"{"a":"he said \"hi\"","b":"c:\\x"}"#);
    }
}
