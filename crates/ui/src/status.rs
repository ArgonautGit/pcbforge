//! Read board/stage state from the orchestra SQLite DB into a plain snapshot
//! the console renders. No writes — the CLI verbs (shelled from the actions
//! panel) remain the only mutators, so the console never duplicates engine
//! logic (UI-1 constraint).

use std::path::Path;

use orchestra::db::Db;
use orchestra::stages::StageGraph;

/// One board's headline state for the status panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardStatus {
    pub id: i64,
    pub design: String,
    pub stage: String,
    pub registered: bool,
}

/// Everything the status panel shows in one read.
#[derive(Debug, Clone, Default)]
pub struct StatusSnapshot {
    pub boards: Vec<BoardStatus>,
    /// Linear stage sequence from the graph entry (for the position bar).
    pub stages: Vec<String>,
    /// Populated instead of the above on any read error.
    pub error: Option<String>,
}

/// Read a snapshot from the DB at `db_path` and the embedded stage graph.
/// Missing DB / parse errors are captured in [`StatusSnapshot::error`] rather
/// than panicking — the console shows the message and stays up.
pub fn snapshot(db_path: &Path) -> StatusSnapshot {
    let stages = match StageGraph::load() {
        Ok(g) => linear_stages(&g),
        Err(e) => {
            return StatusSnapshot {
                error: Some(format!("stage graph: {e}")),
                ..Default::default()
            };
        }
    };
    let db = match Db::open(db_path) {
        Ok(db) => db,
        Err(e) => {
            return StatusSnapshot {
                stages,
                error: Some(format!("open {}: {e}", db_path.display())),
                ..Default::default()
            };
        }
    };
    let boards = match db.list_boards() {
        Ok(bs) => bs
            .into_iter()
            .map(|b| BoardStatus {
                id: b.id,
                design: b.design_path,
                stage: b.stage,
                registered: b.board_affine.is_some(),
            })
            .collect(),
        Err(e) => {
            return StatusSnapshot {
                stages,
                error: Some(format!("list boards: {e}")),
                ..Default::default()
            };
        }
    };
    StatusSnapshot {
        boards,
        stages,
        error: None,
    }
}

/// Walk `entry → next → …` into a linear stage list, stopping at a terminal
/// stage or a cycle (defensive — the graph is validated elsewhere).
fn linear_stages(g: &StageGraph) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = Some(g.entry.clone());
    while let Some(name) = cur {
        if out.contains(&name) {
            break;
        }
        let next = g.stages.get(&name).and_then(|s| s.next.clone());
        out.push(name);
        cur = next;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_db_is_reported_not_panicked() {
        let snap = snapshot(Path::new("/nonexistent/dir/x.sqlite"));
        assert!(snap.error.is_some(), "bad path should surface an error");
        // Stage graph still loaded (it's embedded).
        assert!(
            !snap.stages.is_empty(),
            "stages come from the embedded graph"
        );
    }

    #[test]
    fn empty_db_has_no_boards_and_a_stage_list() {
        let dir = std::env::temp_dir().join(format!("ui-status-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("empty.sqlite");
        let _ = std::fs::remove_file(&path);
        // Opening creates+migrates the schema.
        let snap = snapshot(&path);
        assert!(
            snap.error.is_none(),
            "fresh DB opens clean: {:?}",
            snap.error
        );
        assert!(snap.boards.is_empty());
        assert!(
            snap.stages.iter().any(|s| !s.is_empty()),
            "a non-empty stage sequence"
        );
    }
}
