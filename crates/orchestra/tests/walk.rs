//! ORC-2 acceptance: a board walks the whole stage graph across *separate*
//! process invocations. Each `pcbforge next` is one `engine::step`; a process
//! restart is modelled by dropping the `Db` and re-opening it on the same file
//! before the next step. If any state lived only in memory the walk would break.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use orchestra::db::Db;
use orchestra::engine::{
    self, BoardDefaults, ExecutorRegistry, FixedPalletSource, FlipMode, StepReport,
};
use orchestra::stages::StageGraph;

/// A unique db file under the temp dir (mirrors db.rs's test helper).
fn temp_db_path(tag: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("orchestra-walk-{}-{}", std::process::id(), n));
    std::fs::create_dir_all(&dir).expect("create temp test dir");
    dir.join(format!("{tag}.sqlite"))
}

/// One "process invocation": open the db fresh, take one step, drop the db.
/// The flip decision is explicit (not env-driven) so parallel tests stay
/// deterministic.
fn invoke_with(path: &PathBuf, pallet: &FixedPalletSource, flip: FlipMode) -> StepReport {
    let db = Db::open(path).expect("open db");
    let graph = StageGraph::load().expect("load graph");
    let registry = ExecutorRegistry::with_flip_mode(flip);
    let defaults = BoardDefaults::default();
    engine::step(&db, &graph, &registry, pallet, &defaults).expect("step")
    // `db` drops here — the connection closes, as at process exit.
}

/// Single-sided invocation (the original walk).
fn invoke(path: &PathBuf, pallet: &FixedPalletSource) -> StepReport {
    invoke_with(path, pallet, FlipMode::SingleSided)
}

#[test]
fn board_walks_fiducials_to_done_across_restarts() {
    let path = temp_db_path("walk");
    let pallet = FixedPalletSource(1001);

    // Step 1: fiducials -> bulk_top.
    let r1 = invoke(&path, &pallet);
    assert_eq!(
        r1,
        StepReport::Advanced {
            board_id: r1.board_id(),
            stage: "fiducials".into(),
            to: "bulk_top".into(),
        }
    );

    // Step 2: bulk_top -> iso_check (Laser stub).
    let r2 = invoke(&path, &pallet);
    assert_eq!(
        r2,
        StepReport::Advanced {
            board_id: r1.board_id(),
            stage: "bulk_top".into(),
            to: "iso_check".into(),
        }
    );

    // Step 3: iso_check -> flip (ClearanceLoop stub).
    let r3 = invoke(&path, &pallet);
    assert_eq!(
        r3,
        StepReport::Advanced {
            board_id: r1.board_id(),
            stage: "iso_check".into(),
            to: "flip".into(),
        }
    );

    // Step 4: flip -> done (single-sided: the branch is not taken).
    let r3b = invoke(&path, &pallet);
    assert_eq!(
        r3b,
        StepReport::Advanced {
            board_id: r1.board_id(),
            stage: "flip".into(),
            to: "done".into(),
        }
    );

    // Step 5: at terminal `done` — halts, idempotent.
    let r4 = invoke(&path, &pallet);
    assert_eq!(
        r4,
        StepReport::Halted {
            board_id: r1.board_id(),
            stage: "done".into(),
        }
    );
    // A fifth call stays halted (no runaway, no new rows).
    let r5 = invoke(&path, &pallet);
    assert_eq!(r5, r4);

    // Every step acted on the same board.
    let board_id = r1.board_id();

    // Persisted state survived the restarts: the board is parked at `done`.
    let db = Db::open(&path).unwrap();
    let board = db.get_board(board_id).unwrap().unwrap();
    assert_eq!(board.stage, "done");
    assert_eq!(
        board.pallet_id,
        db.get_pallet_by_tag(1001).unwrap().map(|p| p.id)
    );

    // Runlog has a start+done pair for each of the three worked stages, plus
    // each executor's own side-effect row — and nothing for the terminal stage.
    let log = db.list_runlog_for_board(board_id).unwrap();
    let events: Vec<(&str, &str)> = log
        .iter()
        .map(|e| (e.stage.as_str(), e.event.as_str()))
        .collect();
    assert_eq!(
        events,
        vec![
            ("fiducials", "stage_start"),
            ("fiducials", "prompt"),
            ("fiducials", "stage_done"),
            ("bulk_top", "stage_start"),
            ("bulk_top", "airflow_skipped"),
            ("bulk_top", "emit_intent"),
            ("bulk_top", "stage_done"),
            ("iso_check", "stage_start"),
            ("iso_check", "clearance_stub"),
            ("iso_check", "stage_done"),
            ("flip", "stage_start"),
            ("flip", "flip_skip"),
            ("flip", "stage_done"),
        ],
    );

    // No runlog rows were ever written against the terminal stage.
    assert!(!log.iter().any(|e| e.stage == "done"));

    // The Laser stub recorded the machine/process it would emit for.
    let emit = log.iter().find(|e| e.event == "emit_intent").unwrap();
    assert!(emit.detail.contains("fiber"), "detail: {}", emit.detail);
    assert!(
        emit.detail.contains("ablate-top"),
        "detail: {}",
        emit.detail
    );
}

/// ORC-6 (software half): a double-sided board branches at `flip` into the
/// bottom-side stages and walks them to `done`, with the flip prompt recording
/// the mirror-aware registration guidance.
#[test]
fn double_sided_board_branches_through_the_bottom_flow() {
    let path = temp_db_path("walk-double");
    let pallet = FixedPalletSource(2002);
    let ds = FlipMode::DoubleSided;

    // Top side: fiducials -> bulk_top -> iso_check -> flip.
    for _ in 0..3 {
        invoke_with(&path, &pallet, ds);
    }

    // The flip stage takes the branch into the bottom flow.
    let r = invoke_with(&path, &pallet, ds);
    assert_eq!(
        r,
        StepReport::Advanced {
            board_id: r.board_id(),
            stage: "flip".into(),
            to: "fiducials_bottom".into(),
        }
    );

    // Bottom side: fiducials_bottom -> bulk_bottom -> iso_check_bottom -> done.
    let seq = [
        ("fiducials_bottom", "bulk_bottom"),
        ("bulk_bottom", "iso_check_bottom"),
        ("iso_check_bottom", "done"),
    ];
    for (from, to) in seq {
        let s = invoke_with(&path, &pallet, ds);
        assert_eq!(
            s,
            StepReport::Advanced {
                board_id: r.board_id(),
                stage: from.into(),
                to: to.into(),
            }
        );
    }

    // Terminal and idempotent.
    let halt = invoke_with(&path, &pallet, ds);
    assert_eq!(
        halt,
        StepReport::Halted {
            board_id: r.board_id(),
            stage: "done".into(),
        }
    );

    // The flip prompt carried the mirror-aware registration guidance, and the
    // bottom Laser stage recorded the bottom process.
    let db = Db::open(&path).unwrap();
    let log = db.list_runlog_for_board(r.board_id()).unwrap();
    let prompt = log
        .iter()
        .find(|e| e.event == "flip_prompt")
        .expect("flip recorded its prompt");
    assert!(
        prompt.detail.contains("mirror-aware") && prompt.detail.contains("entry-exit"),
        "flip prompt names the mirror-aware coordinates: {}",
        prompt.detail
    );
    let bottom_emit = log
        .iter()
        .find(|e| e.stage == "bulk_bottom" && e.event == "emit_intent")
        .expect("bottom laser stage ran");
    assert!(
        bottom_emit.detail.contains("ablate-bottom"),
        "detail: {}",
        bottom_emit.detail
    );
}

#[test]
fn distinct_pallets_get_distinct_boards() {
    let path = temp_db_path("two-pallets");
    let a = invoke(&path, &FixedPalletSource(1));
    let b = invoke(&path, &FixedPalletSource(2));
    assert_ne!(a.board_id(), b.board_id());
    // Re-reading pallet 1 resolves back to its board, not pallet 2's.
    let a2 = invoke(&path, &FixedPalletSource(1));
    assert_eq!(a2.board_id(), a.board_id());
}
