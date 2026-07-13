//! `xtask` library surface — logic shared between the `cargo xtask` binary
//! and its integration tests.
//!
//! The binary (`src/main.rs`) owns simple commands like `fixtures`; commands
//! with testable pipelines live here as modules the tests can call directly.

pub mod seed_defect;
