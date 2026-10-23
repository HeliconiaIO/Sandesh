//! First-run setup.
//!
//! A fresh database has no accounts, and Sandesh's default routing policy sends
//! unmatched recipients to the catch-all account (`CORE_SPEC.md` §7). Without
//! one, `cargo run` would produce a server that accepts a connection and then
//! fails to deliver anything — the worst possible first experience.
//!
//! This is setup, not domain logic: it creates the account the configured
//! policy already implies. It makes no routing decision of its own.

use std::sync::Arc;

use sandesh_core::AccountService;

use crate::AppError;

/// The username of the account unmatched mail routes to.
pub const CATCH_ALL_USERNAME: &str = "catch-all";

/// Creates the catch-all account if there is not one already.
///
/// Idempotent: every start calls it, and a second call on an existing database
/// does nothing. Returns whether an account was created.
///
/// The account itself is built by `AccountService` (ADR-070), so the catch-all
/// satisfies the same invariants as any other account — including the rule that
/// there is at most one of it, which this function used to assume rather than
/// enforce. It carries no credential and no identity: it is reached precisely
/// when no identity matched.
///
/// # Errors
/// [`AppError::Bootstrap`] wrapping a storage failure — reported as a startup
/// failure, because an instance that cannot accept mail should not claim to
/// have started.
pub async fn ensure_catch_all(accounts: Arc<AccountService>) -> Result<bool, AppError> {
    let (_, created) = accounts
        .ensure_catch_all(CATCH_ALL_USERNAME)
        .await
        .map_err(|error| storage_failure(&error))?;

    if created {
        tracing::info!(
            username = CATCH_ALL_USERNAME,
            "created the catch-all account for unmatched recipients"
        );
    }
    Ok(created)
}

/// Wraps a core failure as a startup failure.
fn storage_failure(error: &sandesh_core::CoreError) -> AppError {
    AppError::Bootstrap(error.to_string())
}
