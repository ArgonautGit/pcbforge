//! rusqlite persistence layer over the fixed schema in `docs/schema.sql`.
//!
//! The schema file is the single source of truth and is embedded verbatim at
//! compile time. [`Db::open`] executes it (all DDL is `IF NOT EXISTS`, so
//! opening is idempotent) and stamps `schema_version = 1` on first creation.
//!
//! Timestamps and other defaults come from SQLite (`strftime(...,'now')`);
//! this layer never generates them in Rust. JSON columns are carried as plain
//! `String`s — their shape is owned by the writing module, not by this layer.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Row, params};

/// `docs/schema.sql`, embedded verbatim.
const SCHEMA_SQL: &str = include_str!("../../../docs/schema.sql");

/// Current schema version stamped into `schema_version` on first creation.
const SCHEMA_VERSION: i64 = 1;

/// Result alias for this module.
pub type Result<T> = rusqlite::Result<T>;

/// A physical carrier pallet, identified by its AprilTag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pallet {
    pub id: i64,
    /// tag36h11 ID.
    pub tag_id: i64,
    pub name: String,
    /// Board-origin offset of the pallet datum, µm, in bed frame.
    pub datum_x_um: i64,
    pub datum_y_um: i64,
    pub created_at: String,
}

/// One physical board being fabricated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Board {
    pub id: i64,
    pub pallet_id: Option<i64>,
    /// Source `.kicad_pcb` path.
    pub design_path: String,
    /// sha256 of the design file.
    pub design_hash: String,
    /// Stage-engine state (stage names come from `docs/stages.ron`).
    pub stage: String,
    /// JSON: executor-owned resume data.
    pub stage_state: String,
    /// JSON: 3x3 row-major, board->bed mm; `None` until registered.
    pub board_affine: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// One append-only runlog entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunlogEntry {
    pub id: i64,
    pub board_id: Option<i64>,
    pub at: String,
    pub stage: String,
    /// e.g. `stage_start`, `stage_done`, `emit`, `converge`, `escalate`.
    pub event: String,
    /// JSON payload owned by the writer.
    pub detail: String,
}

/// Proven process parameters for one `(machine, material, op)`.
#[derive(Debug, Clone, PartialEq)]
pub struct MaterialRow {
    pub id: i64,
    /// `'fiber' | 'uv'`.
    pub machine: String,
    /// e.g. `'fr4-1oz'`.
    pub material: String,
    /// e.g. `'bulk_clear'`, `'corrective'`, `'mask_open'`.
    pub op: String,
    pub power_pct: f64,
    pub speed_mm_s: f64,
    pub frequency_khz: f64,
    /// 0 = source default.
    pub pulse_ns: i64,
    pub interval_mm: f64,
    pub passes: i64,
    /// Provenance: which ladder/test produced this row.
    pub source: String,
    pub created_at: String,
}

/// Caller-supplied fields for a material upsert. `id` and `created_at` are
/// owned by the database.
#[derive(Debug, Clone, PartialEq)]
pub struct NewMaterial {
    pub machine: String,
    pub material: String,
    pub op: String,
    pub power_pct: f64,
    pub speed_mm_s: f64,
    pub frequency_khz: f64,
    pub pulse_ns: i64,
    pub interval_mm: f64,
    pub passes: i64,
    pub source: String,
}

/// Handle over one SQLite database file.
pub struct Db {
    conn: Connection,
}

impl Db {
    /// Open (creating if absent) the database at `path` and migrate it: the
    /// embedded `docs/schema.sql` is executed (idempotent — all DDL is
    /// `IF NOT EXISTS`) and `schema_version` is stamped if empty.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA_SQL)?;
        let versions: i64 =
            conn.query_row("SELECT COUNT(*) FROM schema_version", [], |r| r.get(0))?;
        if versions == 0 {
            conn.execute(
                "INSERT INTO schema_version (version) VALUES (?1)",
                params![SCHEMA_VERSION],
            )?;
        } else {
            // Fail loudly at open on schema drift, rather than at a random
            // query later when a migration changed the shape (LR-29).
            let stored: i64 =
                conn.query_row("SELECT version FROM schema_version", [], |r| r.get(0))?;
            if stored != SCHEMA_VERSION {
                return Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
                    Some(format!(
                        "database schema version {stored} != supported {SCHEMA_VERSION}; \
                         migrate or use a matching build"
                    )),
                ));
            }
        }
        Ok(Self { conn })
    }

    /// The stored schema version.
    pub fn schema_version(&self) -> Result<i64> {
        self.conn
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
    }

    // ---- pallet ----------------------------------------------------------

    /// Insert a pallet; `id` and `created_at` come from SQLite.
    pub fn insert_pallet(
        &self,
        tag_id: i64,
        name: &str,
        datum_x_um: i64,
        datum_y_um: i64,
    ) -> Result<Pallet> {
        self.conn.execute(
            "INSERT INTO pallet (tag_id, name, datum_x_um, datum_y_um)
             VALUES (?1, ?2, ?3, ?4)",
            params![tag_id, name, datum_x_um, datum_y_um],
        )?;
        let id = self.conn.last_insert_rowid();
        self.get_pallet(id).map(|p| {
            p.expect("row just inserted must exist") // infallible
        })
    }

    /// Fetch a pallet by `id`.
    pub fn get_pallet(&self, id: i64) -> Result<Option<Pallet>> {
        self.conn
            .query_row(
                "SELECT id, tag_id, name, datum_x_um, datum_y_um, created_at
                 FROM pallet WHERE id = ?1",
                params![id],
                pallet_from_row,
            )
            .optional()
    }

    /// Fetch a pallet by its (unique) AprilTag ID.
    pub fn get_pallet_by_tag(&self, tag_id: i64) -> Result<Option<Pallet>> {
        self.conn
            .query_row(
                "SELECT id, tag_id, name, datum_x_um, datum_y_um, created_at
                 FROM pallet WHERE tag_id = ?1",
                params![tag_id],
                pallet_from_row,
            )
            .optional()
    }

    /// Update a pallet's mutable fields (`tag_id`, `name`, datum offsets).
    pub fn update_pallet(&self, pallet: &Pallet) -> Result<()> {
        self.conn.execute(
            "UPDATE pallet SET tag_id = ?1, name = ?2, datum_x_um = ?3, datum_y_um = ?4
             WHERE id = ?5",
            params![
                pallet.tag_id,
                pallet.name,
                pallet.datum_x_um,
                pallet.datum_y_um,
                pallet.id
            ],
        )?;
        Ok(())
    }

    /// All pallets, ordered by `id`.
    pub fn list_pallets(&self) -> Result<Vec<Pallet>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, tag_id, name, datum_x_um, datum_y_um, created_at
             FROM pallet ORDER BY id",
        )?;
        let rows = stmt.query_map([], pallet_from_row)?;
        rows.collect()
    }

    // ---- board -----------------------------------------------------------

    /// Insert a board at the default stage (`'start'`); `id`, timestamps and
    /// stage defaults come from SQLite.
    pub fn insert_board(
        &self,
        pallet_id: Option<i64>,
        design_path: &str,
        design_hash: &str,
    ) -> Result<Board> {
        self.conn.execute(
            "INSERT INTO board (pallet_id, design_path, design_hash)
             VALUES (?1, ?2, ?3)",
            params![pallet_id, design_path, design_hash],
        )?;
        let id = self.conn.last_insert_rowid();
        self.get_board(id)
            .map(|b| b.expect("row just inserted must exist"))
    }

    /// Fetch a board by `id`.
    pub fn get_board(&self, id: i64) -> Result<Option<Board>> {
        self.conn
            .query_row(
                "SELECT id, pallet_id, design_path, design_hash, stage, stage_state,
                        board_affine, created_at, updated_at
                 FROM board WHERE id = ?1",
                params![id],
                board_from_row,
            )
            .optional()
    }

    /// Update a board's mutable fields (`pallet_id`, `stage`, `stage_state`,
    /// `board_affine`); `updated_at` is bumped by SQLite.
    pub fn update_board(&self, board: &Board) -> Result<()> {
        self.conn.execute(
            "UPDATE board SET pallet_id = ?1, stage = ?2, stage_state = ?3,
                    board_affine = ?4,
                    updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
             WHERE id = ?5",
            params![
                board.pallet_id,
                board.stage,
                board.stage_state,
                board.board_affine,
                board.id
            ],
        )?;
        Ok(())
    }

    /// All boards, ordered by `id`.
    pub fn list_boards(&self) -> Result<Vec<Board>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, pallet_id, design_path, design_hash, stage, stage_state,
                    board_affine, created_at, updated_at
             FROM board ORDER BY id",
        )?;
        let rows = stmt.query_map([], board_from_row)?;
        rows.collect()
    }

    // ---- runlog ----------------------------------------------------------

    /// Append one runlog entry; `id` and `at` come from SQLite.
    pub fn append_runlog(
        &self,
        board_id: Option<i64>,
        stage: &str,
        event: &str,
        detail: &str,
    ) -> Result<RunlogEntry> {
        self.conn.execute(
            "INSERT INTO runlog (board_id, stage, event, detail)
             VALUES (?1, ?2, ?3, ?4)",
            params![board_id, stage, event, detail],
        )?;
        let id = self.conn.last_insert_rowid();
        self.conn.query_row(
            "SELECT id, board_id, at, stage, event, detail FROM runlog WHERE id = ?1",
            params![id],
            runlog_from_row,
        )
    }

    /// All runlog entries for one board, in append order. Ordered by the
    /// monotonic rowid `id` alone — not the wall-clock `at`, which a backward
    /// NTP/clock step could reorder into a false history (LR-28).
    pub fn list_runlog_for_board(&self, board_id: i64) -> Result<Vec<RunlogEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, board_id, at, stage, event, detail
             FROM runlog WHERE board_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![board_id], runlog_from_row)?;
        rows.collect()
    }

    // ---- material --------------------------------------------------------

    /// Insert or replace the parameters for `(machine, material, op)`,
    /// keyed by the table's UNIQUE constraint. On conflict the process
    /// parameters and `source` are overwritten; `id` and `created_at` of the
    /// existing row are preserved.
    pub fn upsert_material(&self, m: &NewMaterial) -> Result<MaterialRow> {
        self.conn.execute(
            "INSERT INTO material (machine, material, op, power_pct, speed_mm_s,
                                   frequency_khz, pulse_ns, interval_mm, passes, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT (machine, material, op) DO UPDATE SET
                 power_pct = excluded.power_pct,
                 speed_mm_s = excluded.speed_mm_s,
                 frequency_khz = excluded.frequency_khz,
                 pulse_ns = excluded.pulse_ns,
                 interval_mm = excluded.interval_mm,
                 passes = excluded.passes,
                 source = excluded.source",
            params![
                m.machine,
                m.material,
                m.op,
                m.power_pct,
                m.speed_mm_s,
                m.frequency_khz,
                m.pulse_ns,
                m.interval_mm,
                m.passes,
                m.source
            ],
        )?;
        self.lookup_material(&m.machine, &m.material, &m.op)
            .map(|r| r.expect("row just upserted must exist"))
    }

    /// Look up the proven parameters for `(machine, material, op)`.
    pub fn lookup_material(
        &self,
        machine: &str,
        material: &str,
        op: &str,
    ) -> Result<Option<MaterialRow>> {
        self.conn
            .query_row(
                "SELECT id, machine, material, op, power_pct, speed_mm_s, frequency_khz,
                        pulse_ns, interval_mm, passes, source, created_at
                 FROM material WHERE machine = ?1 AND material = ?2 AND op = ?3",
                params![machine, material, op],
                material_from_row,
            )
            .optional()
    }

    /// All material rows, ordered by `(machine, material, op)`.
    pub fn list_materials(&self) -> Result<Vec<MaterialRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, machine, material, op, power_pct, speed_mm_s, frequency_khz,
                    pulse_ns, interval_mm, passes, source, created_at
             FROM material ORDER BY machine, material, op",
        )?;
        let rows = stmt.query_map([], material_from_row)?;
        rows.collect()
    }
}

// ---- row mappers -----------------------------------------------------------

fn pallet_from_row(row: &Row<'_>) -> rusqlite::Result<Pallet> {
    Ok(Pallet {
        id: row.get(0)?,
        tag_id: row.get(1)?,
        name: row.get(2)?,
        datum_x_um: row.get(3)?,
        datum_y_um: row.get(4)?,
        created_at: row.get(5)?,
    })
}

fn board_from_row(row: &Row<'_>) -> rusqlite::Result<Board> {
    Ok(Board {
        id: row.get(0)?,
        pallet_id: row.get(1)?,
        design_path: row.get(2)?,
        design_hash: row.get(3)?,
        stage: row.get(4)?,
        stage_state: row.get(5)?,
        board_affine: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn runlog_from_row(row: &Row<'_>) -> rusqlite::Result<RunlogEntry> {
    Ok(RunlogEntry {
        id: row.get(0)?,
        board_id: row.get(1)?,
        at: row.get(2)?,
        stage: row.get(3)?,
        event: row.get(4)?,
        detail: row.get(5)?,
    })
}

fn material_from_row(row: &Row<'_>) -> rusqlite::Result<MaterialRow> {
    Ok(MaterialRow {
        id: row.get(0)?,
        machine: row.get(1)?,
        material: row.get(2)?,
        op: row.get(3)?,
        power_pct: row.get(4)?,
        speed_mm_s: row.get(5)?,
        frequency_khz: row.get(6)?,
        pulse_ns: row.get(7)?,
        interval_mm: row.get(8)?,
        passes: row.get(9)?,
        source: row.get(10)?,
        created_at: row.get(11)?,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    /// A unique database path under the system temp dir. The parent test dir
    /// is created; the file itself is left for SQLite to create.
    fn temp_db_path(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("orchestra-db-test-{}-{}", std::process::id(), n));
        std::fs::create_dir_all(&dir).expect("create temp test dir");
        dir.join(format!("{tag}.sqlite"))
    }

    fn sample_material(op: &str) -> NewMaterial {
        NewMaterial {
            machine: "fiber".into(),
            material: "fr4-1oz".into(),
            op: op.into(),
            power_pct: 80.0,
            speed_mm_s: 900.0,
            frequency_khz: 45.0,
            pulse_ns: 120,
            interval_mm: 0.03,
            passes: 8,
            source: "ladder-2026-07-01".into(),
        }
    }

    #[test]
    fn open_stamps_schema_version_and_is_idempotent() {
        let path = temp_db_path("version");
        let db = Db::open(&path).unwrap();
        assert_eq!(db.schema_version().unwrap(), 1);
        drop(db);
        // Second open: no error, version unchanged (single row, still 1).
        let db2 = Db::open(&path).unwrap();
        assert_eq!(db2.schema_version().unwrap(), 1);
        let rows: i64 = db2
            .conn
            .query_row("SELECT COUNT(*) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn pallet_round_trip() {
        let db = Db::open(temp_db_path("pallet")).unwrap();
        let p = db.insert_pallet(42, "left-bed", 1500, -250).unwrap();
        assert_eq!(p.tag_id, 42);
        assert_eq!(p.name, "left-bed");
        assert_eq!(p.datum_x_um, 1500);
        assert_eq!(p.datum_y_um, -250);
        assert!(!p.created_at.is_empty());

        assert_eq!(db.get_pallet(p.id).unwrap().unwrap(), p);
        assert_eq!(db.get_pallet_by_tag(42).unwrap().unwrap(), p);
        assert_eq!(db.list_pallets().unwrap(), vec![p.clone()]);

        let mut renamed = p.clone();
        renamed.name = "right-bed".into();
        renamed.datum_x_um = 0;
        db.update_pallet(&renamed).unwrap();
        assert_eq!(db.get_pallet(p.id).unwrap().unwrap(), renamed);

        assert!(db.get_pallet(9999).unwrap().is_none());
    }

    #[test]
    fn board_round_trip_and_stage_update() {
        let db = Db::open(temp_db_path("board")).unwrap();
        let pallet = db.insert_pallet(7, "", 0, 0).unwrap();
        let b = db
            .insert_board(Some(pallet.id), "designs/blinky.kicad_pcb", "abc123")
            .unwrap();
        assert_eq!(b.pallet_id, Some(pallet.id));
        assert_eq!(b.stage, "start");
        assert_eq!(b.stage_state, "{}");
        assert_eq!(b.board_affine, None);
        assert_eq!(db.get_board(b.id).unwrap().unwrap(), b);
        assert_eq!(db.list_boards().unwrap(), vec![b.clone()]);

        let mut moved = b.clone();
        moved.stage = "isolation".into();
        moved.stage_state = r#"{"pass_group":3}"#.into();
        moved.board_affine = Some("[1,0,0,0,1,0,0,0,1]".into());
        db.update_board(&moved).unwrap();

        let got = db.get_board(b.id).unwrap().unwrap();
        assert_eq!(got.stage, "isolation");
        assert_eq!(got.stage_state, r#"{"pass_group":3}"#);
        assert_eq!(got.board_affine.as_deref(), Some("[1,0,0,0,1,0,0,0,1]"));
        // created_at is immutable; updated_at is maintained by SQLite.
        assert_eq!(got.created_at, b.created_at);
        assert!(!got.updated_at.is_empty());
    }

    #[test]
    fn runlog_append_and_list_for_board() {
        let db = Db::open(temp_db_path("runlog")).unwrap();
        let b = db.insert_board(None, "d.kicad_pcb", "h").unwrap();
        let other = db.insert_board(None, "e.kicad_pcb", "h2").unwrap();

        let e1 = db
            .append_runlog(Some(b.id), "start", "stage_start", "{}")
            .unwrap();
        let e2 = db
            .append_runlog(Some(b.id), "start", "stage_done", r#"{"ok":true}"#)
            .unwrap();
        db.append_runlog(Some(other.id), "start", "stage_start", "{}")
            .unwrap();
        // A board-less entry must not appear in any board's log.
        db.append_runlog(None, "", "escalate", "{}").unwrap();

        assert_eq!(e1.board_id, Some(b.id));
        assert_eq!(e1.event, "stage_start");
        assert!(!e1.at.is_empty());

        let log = db.list_runlog_for_board(b.id).unwrap();
        assert_eq!(log, vec![e1, e2]);
    }

    #[test]
    fn material_upsert_and_lookup() {
        let db = Db::open(temp_db_path("material")).unwrap();
        let first = db.upsert_material(&sample_material("bulk_clear")).unwrap();
        assert_eq!(first.power_pct, 80.0);
        assert_eq!(first.passes, 8);
        assert_eq!(
            db.lookup_material("fiber", "fr4-1oz", "bulk_clear")
                .unwrap()
                .unwrap(),
            first
        );

        // Upsert on the same key overwrites parameters, keeps id/created_at.
        let mut better = sample_material("bulk_clear");
        better.power_pct = 65.0;
        better.passes = 6;
        better.source = "ladder-2026-07-05".into();
        let second = db.upsert_material(&better).unwrap();
        assert_eq!(second.id, first.id);
        assert_eq!(second.created_at, first.created_at);
        assert_eq!(second.power_pct, 65.0);
        assert_eq!(second.passes, 6);
        assert_eq!(second.source, "ladder-2026-07-05");

        // A different op is a distinct row.
        db.upsert_material(&sample_material("corrective")).unwrap();
        assert_eq!(db.list_materials().unwrap().len(), 2);
        assert!(
            db.lookup_material("uv", "fr4-1oz", "bulk_clear")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn second_open_sees_all_data() {
        let path = temp_db_path("reopen");
        let (pallet, board, entry, material) = {
            let db = Db::open(&path).unwrap();
            let p = db.insert_pallet(3, "carrier", 10, 20).unwrap();
            let mut b = db.insert_board(Some(p.id), "x.kicad_pcb", "hash").unwrap();
            b.stage = "mask_open".into();
            b.stage_state = r#"{"step":2}"#.into();
            db.update_board(&b).unwrap();
            let e = db
                .append_runlog(Some(b.id), "mask_open", "emit", r#"{"file":"j1"}"#)
                .unwrap();
            let m = db.upsert_material(&sample_material("mask_open")).unwrap();
            (p, db.get_board(b.id).unwrap().unwrap(), e, m)
        }; // first connection dropped

        let db2 = Db::open(&path).unwrap();
        assert_eq!(db2.get_pallet(pallet.id).unwrap().unwrap(), pallet);
        let got_board = db2.get_board(board.id).unwrap().unwrap();
        assert_eq!(got_board, board);
        assert_eq!(got_board.stage, "mask_open");
        assert_eq!(got_board.stage_state, r#"{"step":2}"#);
        assert_eq!(
            db2.list_runlog_for_board(board.id).unwrap(),
            vec![entry.clone()]
        );
        assert_eq!(
            db2.lookup_material("fiber", "fr4-1oz", "mask_open")
                .unwrap()
                .unwrap(),
            material
        );
    }
}
