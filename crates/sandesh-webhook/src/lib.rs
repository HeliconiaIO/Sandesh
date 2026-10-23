//! Outbound webhook delivery (ADR-085).
//!
//! This crate owns the HTTP client, the retry worker and nothing else. The
//! types, the state machine and the backoff schedule are domain logic and live
//! in `sandesh-core`; the SQL lives in `sandesh-storage`. `sandesh-core` must
//! never reach any of this, and `tests/architecture.rs` enforces both halves:
//! the core's ban list, and this crate's `sandesh-*` closure being exactly
//! `sandesh-core`.
//!
//! The split is why [`RetryPolicy`](sandesh_core::RetryPolicy) is testable
//! without a socket: the worker asks it what to do and cannot quietly disagree.

mod client;
mod worker;

pub use client::{DeliveryError, WebhookClient};
pub use worker::OutboxWorker;
