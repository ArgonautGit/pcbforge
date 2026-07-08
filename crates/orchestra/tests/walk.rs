//! ORC-2 acceptance: a board walks the whole stage graph across *separate*
//! process invocations. Each `pcbforge next` is one `engine::step`; a process
//! restart is modelled by dropping the `Db` and re-opening it on the same file
//! before the next step. If any state lived only in memory the walk would break.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use orchestra::db::Db;
use orchestra::engine::{self, BoardDefaults, ExecutorRegistry, FixedPalletSource, StepReport};
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
fn invoke(path: &PathBuf, pallet: &FixedPalletSource) -> StepReport {
    let db = Db::open(path).expect("open db");
    let graph = StageGraph::load().expect("load graph");
    let registry = ExecutorRegistry::with_defaults();
    let defaults = BoardDefaults::default();
    engine::step(&db, &graph, &registry, pallet, &defaults).expect("step")
    // `db` drops here — the connection closes, as at process exit.
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

    // Step 3: iso_check -> done (ClearanceLoop stub).
    let r3 = invoke(&path, &pallet);
    assert_eq!(
        r3,
        StepReport::Advanced {
            board_id: r1.board_id(),
            stage: "iso_check".into(),
            to: "done".into(),
        }
    );

    // Step 4: at terminal `done` — halts, idempotent.
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
            ("bulk_top", "emit_intent"),
            ("bulk_top", "stage_done"),
            ("iso_check", "stage_start"),
            ("iso_check", "clearance_stub"),
            ("iso_check", "stage_done"),
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
