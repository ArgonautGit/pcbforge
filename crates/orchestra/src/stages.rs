//! The stage DAG, deserialized from `docs/stages.ron`.
//!
//! A board advances through a directed graph of named stages. Each stage names
//! its executor [`StageKind`] and its (single, for now) successor. The graph is
//! authored in RON and embedded verbatim at compile time via [`StageGraph::load`];
//! see the header of `docs/stages.ron` for its provenance (agent-authored at the
//! operator's direction, standing in for the playbook's verbatim graph).
//!
//! This module owns only the *shape* of the graph and its structural validation
//! (entry resolves, every `next` names a real stage). Executor behaviour lives
//! in [`crate::engine`].

use std::collections::BTreeMap;

use serde::Deserialize;

/// `docs/stages.ron`, embedded verbatim. The path mirrors `db.rs`'s embedding
/// of `docs/schema.sql` (relative to this source file: up out of `src/`, out of
/// the crate, out of `crates/`, into `docs/`).
const STAGES_RON: &str = include_str!("../../../docs/stages.ron");

/// Which executor kind drives a stage. Unit variants deserialize from the bare
/// RON identifiers `Manual` / `Laser` / `ClearanceLoop`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum StageKind {
    /// An operator step the engine prompts for and records.
    Manual,
    /// Emits a compiled job set for a machine (real emission in ORC-3/DRV-6).
    Laser,
    /// The closed inspect/correct loop (stubbed here; ORC-3 replaces it).
    ClearanceLoop,
}

/// One stage: its executor kind, human detail, optional machine/process hints,
/// and its successor (`None` marks a terminal stage).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct StageDef {
    pub kind: StageKind,
    pub detail: String,
    #[serde(default)]
    pub machine: Option<String>,
    #[serde(default)]
    pub process: Option<String>,
    #[serde(default)]
    pub next: Option<String>,
}

impl StageDef {
    /// A terminal stage has no successor.
    pub fn is_terminal(&self) -> bool {
        self.next.is_none()
    }
}

/// The whole stage DAG: an entry stage plus the map of named stages.
///
/// Deserialized from the RON `Stages(entry: ..., stages: { ... })` — RON's
/// leading struct name is cosmetic and need not match this type's name.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename = "Stages")]
pub struct StageGraph {
    pub entry: String,
    pub stages: BTreeMap<String, StageDef>,
}

/// A structural problem with a parsed [`StageGraph`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphError {
    /// The `entry` stage is not present in `stages`.
    MissingEntry { entry: String },
    /// A stage's `next` names a stage that is not present in `stages`.
    DanglingNext { from: String, next: String },
}

impl std::fmt::Display for GraphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GraphError::MissingEntry { entry } => {
                write!(f, "entry stage `{entry}` is not defined in the graph")
            }
            GraphError::DanglingNext { from, next } => write!(
                f,
                "stage `{from}` advances to `{next}`, which is not defined in the graph"
            ),
        }
    }
}

impl std::error::Error for GraphError {}

impl StageGraph {
    /// Parse and validate the embedded `docs/stages.ron`.
    pub fn load() -> Result<Self, LoadError> {
        let graph: StageGraph = ron::from_str(STAGES_RON).map_err(LoadError::Parse)?;
        graph.validate().map_err(LoadError::Invalid)?;
        Ok(graph)
    }

    /// Parse and validate a graph from a RON string (used by tests).
    pub fn from_ron(src: &str) -> Result<Self, LoadError> {
        let graph: StageGraph = ron::from_str(src).map_err(LoadError::Parse)?;
        graph.validate().map_err(LoadError::Invalid)?;
        Ok(graph)
    }

    /// Look up a stage by name.
    pub fn stage(&self, name: &str) -> Option<&StageDef> {
        self.stages.get(name)
    }

    /// Check that `entry` resolves and every `next` names a real stage.
    pub fn validate(&self) -> Result<(), GraphError> {
        if !self.stages.contains_key(&self.entry) {
            return Err(GraphError::MissingEntry {
                entry: self.entry.clone(),
            });
        }
        for (name, def) in &self.stages {
            if let Some(next) = &def.next
                && !self.stages.contains_key(next)
            {
                return Err(GraphError::DanglingNext {
                    from: name.clone(),
                    next: next.clone(),
                });
            }
        }
        Ok(())
    }
}

/// Failure to load a [`StageGraph`]: a RON parse error or a structural one.
#[derive(Debug)]
pub enum LoadError {
    Parse(ron::error::SpannedError),
    Invalid(GraphError),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Parse(e) => write!(f, "failed to parse stage graph RON: {e}"),
            LoadError::Invalid(e) => write!(f, "stage graph is not well-formed: {e}"),
        }
    }
}

impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LoadError::Parse(e) => Some(e),
            LoadError::Invalid(e) => Some(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_graph_parses_and_is_well_formed() {
        let graph = StageGraph::load().expect("docs/stages.ron must parse and validate");
        assert_eq!(graph.entry, "fiducials");
        // Entry resolves.
        assert!(graph.stage(&graph.entry).is_some());
        // The documented walk is wired end to end.
        assert_eq!(graph.stage("fiducials").unwrap().kind, StageKind::Manual);
        assert_eq!(
            graph.stage("fiducials").unwrap().next.as_deref(),
            Some("bulk_top")
        );
        let bulk = graph.stage("bulk_top").unwrap();
        assert_eq!(bulk.kind, StageKind::Laser);
        assert_eq!(bulk.machine.as_deref(), Some("fiber"));
        assert_eq!(bulk.process.as_deref(), Some("ablate-top"));
        assert_eq!(bulk.next.as_deref(), Some("iso_check"));
        assert_eq!(
            graph.stage("iso_check").unwrap().kind,
            StageKind::ClearanceLoop
        );
        assert_eq!(
            graph.stage("iso_check").unwrap().next.as_deref(),
            Some("done")
        );
        assert!(graph.stage("done").unwrap().is_terminal());
    }

    #[test]
    fn every_next_names_a_real_stage() {
        let graph = StageGraph::load().unwrap();
        for def in graph.stages.values() {
            if let Some(next) = &def.next {
                assert!(
                    graph.stages.contains_key(next),
                    "dangling next: {next} is not a stage"
                );
            }
        }
    }

    #[test]
    fn missing_entry_is_rejected() {
        let src = r#"Stages(entry: "ghost", stages: { "real": (kind: Manual, detail: "x", next: None) })"#;
        let err = StageGraph::from_ron(src).unwrap_err();
        assert!(matches!(
            err,
            LoadError::Invalid(GraphError::MissingEntry { .. })
        ));
    }

    #[test]
    fn dangling_next_is_rejected() {
        let src = r#"Stages(entry: "a", stages: { "a": (kind: Manual, detail: "x", next: Some("nowhere")) })"#;
        let err = StageGraph::from_ron(src).unwrap_err();
        assert!(matches!(
            err,
            LoadError::Invalid(GraphError::DanglingNext { .. })
        ));
    }
}
