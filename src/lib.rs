//! Workspace root for Sandesh.
//!
//! This crate contains no functionality. It exists so that workspace-level
//! integration tests — chiefly `tests/architecture.rs`, which reasons about the
//! whole dependency graph — have a package to belong to.
//!
//! The engine lives in `crates/`:
//!
//! * `sandesh-core` — the canonical mail domain model
//! * `sandesh-storage` — SQLite implementations of the core's storage ports
