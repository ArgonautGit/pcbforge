-- PCBForge persistence schema (playbook stand-in).
--
-- Provenance: authored by the coding agent at the operator's direction
-- (2026-07-08) because the playbook's verbatim schema was never provided.
-- ORC-1 uses this file as-is; recorded in docs/decisions.md.
--
-- Conventions: timestamps are ISO-8601 UTC strings; lengths are µm unless a
-- column says otherwise; JSON columns hold small structured blobs whose
-- shape is owned by the writing module.

PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER NOT NULL
);

-- A physical carrier pallet, identified by its AprilTag.
CREATE TABLE IF NOT EXISTS pallet (
    id INTEGER PRIMARY KEY,
    tag_id INTEGER NOT NULL UNIQUE, -- tag36h11 ID
    name TEXT NOT NULL DEFAULT '',
    -- board-origin offset of the pallet datum, µm, in bed frame
    datum_x_um INTEGER NOT NULL DEFAULT 0,
    datum_y_um INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

-- One physical board being fabricated.
CREATE TABLE IF NOT EXISTS board (
    id INTEGER PRIMARY KEY,
    pallet_id INTEGER REFERENCES pallet (id),
    -- design identity
    design_path TEXT NOT NULL, -- source .kicad_pcb path
    design_hash TEXT NOT NULL, -- sha256 of the design file
    -- stage-engine state (stage names come from docs/stages.ron)
    stage TEXT NOT NULL DEFAULT 'start',
    stage_state TEXT NOT NULL DEFAULT '{}', -- JSON: executor-owned resume data
    -- registration
    board_affine TEXT, -- JSON: 3x3 row-major, board->bed mm, NULL until registered
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE INDEX IF NOT EXISTS board_pallet_idx ON board (pallet_id);

-- Append-only log of everything that happened.
CREATE TABLE IF NOT EXISTS runlog (
    id INTEGER PRIMARY KEY,
    board_id INTEGER REFERENCES board (id),
    at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    stage TEXT NOT NULL DEFAULT '',
    event TEXT NOT NULL, -- e.g. stage_start, stage_done, emit, converge, escalate
    detail TEXT NOT NULL DEFAULT '{}' -- JSON payload owned by the writer
);

CREATE INDEX IF NOT EXISTS runlog_board_idx ON runlog (board_id, at);

-- Material table: proven process parameters per (machine, material, op).
CREATE TABLE IF NOT EXISTS material (
    id INTEGER PRIMARY KEY,
    machine TEXT NOT NULL, -- 'fiber' | 'uv'
    material TEXT NOT NULL, -- e.g. 'fr4-1oz'
    op TEXT NOT NULL, -- e.g. 'bulk_clear', 'corrective', 'mask_open'
    power_pct REAL NOT NULL,
    speed_mm_s REAL NOT NULL,
    frequency_khz REAL NOT NULL,
    pulse_ns INTEGER NOT NULL DEFAULT 0, -- 0 = source default
    interval_mm REAL NOT NULL,
    passes INTEGER NOT NULL,
    -- provenance: which ladder/test produced this row
    source TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE (machine, material, op)
);
