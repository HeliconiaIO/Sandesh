//! The outbox drain: one polling task, stateless between ticks (ADR-086).
//!
//! # Why polling
//!
//! The acceptance criterion is "webhooks retry on failure **and survive a
//! restart**", and polling is what makes the second half structural rather than
//! asserted. The worker holds nothing: every fact needed to resume a pending
//! delivery is a column in `webhook_outbox`, so a process restart is
//! indistinguishable from a slow tick. The restart test therefore exercises the
//! real mechanism instead of a reload path written specially to pass it.
//!
//! An event-driven wake would cut happy-path latency from half a tick to near
//! zero, but retries still need the poll, so the poll is not optional and the
//! wake would be a second path to the same drain. Latency is not in the
//! criterion (ADR-086).

use crate::client::{DeliveryError, WebhookClient};
use sandesh_core::{OutboxStatus, OutboxStore, Retry, RetryPolicy, Timestamp};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// How often the outbox is checked for due entries.
const TICK: Duration = Duration::from_millis(500);

/// How many entries one tick may drain.
///
/// Bounded so a large backlog is spread over several passes rather than loaded
/// into memory at once, and so one endpoint's backlog cannot starve the tick.
const BATCH: u32 = 32;

/// Drains the outbox until cancelled.
#[derive(Debug)]
pub struct OutboxWorker {
    outbox: Arc<dyn OutboxStore>,
    client: WebhookClient,
    policy: RetryPolicy,
    tick: Duration,
}

impl OutboxWorker {
    /// Builds a worker over `outbox` with the default retry policy.
    #[must_use]
    pub fn new(outbox: Arc<dyn OutboxStore>) -> Self {
        Self {
            outbox,
            client: WebhookClient::new(),
            policy: RetryPolicy::default(),
            tick: TICK,
        }
    }

    /// Overrides the retry policy.
    #[must_use]
    pub const fn with_policy(mut self, policy: RetryPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Overrides the poll interval, so a test need not wait out the default.
    #[must_use]
    pub const fn with_tick(mut self, tick: Duration) -> Self {
        self.tick = tick;
        self
    }

    /// Runs until `shutdown` is cancelled.
    ///
    /// Cancellation is checked between ticks rather than inside an attempt, so
    /// an in-flight POST finishes on its own terms. Dropping it mid-request
    /// would leave an entry whose row says `pending` while the endpoint has in
    /// fact received it -- a duplicate on the next start, caused by the
    /// shutdown rather than by any failure.
    pub async fn run(self, shutdown: CancellationToken) {
        tracing::debug!("webhook outbox worker started");

        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                () = tokio::time::sleep(self.tick) => self.drain_once().await,
            }
        }

        tracing::debug!("webhook outbox worker stopped");
    }

    /// Attempts every entry that is currently due.
    ///
    /// Public so a test can drive one pass deterministically instead of racing
    /// the tick.
    pub async fn drain_once(&self) {
        let due = match self.outbox.due(Timestamp::now(), BATCH).await {
            Ok(entries) => entries,
            Err(error) => {
                // A failed read is not a reason to stop: the next tick tries
                // again, and the entries are still in the table.
                tracing::warn!(%error, "could not read the webhook outbox");
                return;
            }
        };

        if due.is_empty() {
            return;
        }

        // One endpoint lookup per tick rather than per entry: a batch is
        // usually several notifications for the same handful of endpoints.
        let urls: HashMap<_, _> = match self.outbox.endpoints().await {
            Ok(endpoints) => endpoints
                .into_iter()
                .map(|e| (e.id, (e.url, e.secret)))
                .collect(),
            Err(error) => {
                tracing::warn!(%error, "could not read webhook endpoints");
                return;
            }
        };

        for entry in due {
            let Some((url, secret)) = urls.get(&entry.endpoint_id) else {
                // The endpoint was removed between the enqueue and now. The
                // cascade should have taken the entry with it, so reaching here
                // means the row outlived its target and will never be
                // deliverable; fail it rather than retrying forever.
                self.finish(entry.id, OutboxStatus::Failed, Some("endpoint removed"))
                    .await;
                continue;
            };

            // Signed per attempt, not per entry: the timestamp is inside the
            // signed material, so a retry carries the time it was actually
            // sent. Reusing the first attempt's signature would hand a
            // receiver checking clock skew a payload that looks stale.
            let sent_at = Timestamp::now().as_micros() / 1_000_000;
            let signature = secret
                .as_ref()
                .map(|secret| sandesh_core::sign_payload(secret, &entry.payload, sent_at));

            match self
                .client
                .post(url, &entry.payload, signature.as_deref())
                .await
            {
                Ok(()) => self.finish(entry.id, OutboxStatus::Delivered, None).await,
                Err(error) => self.reschedule(&entry, &error).await,
            }
        }
    }

    /// Records a terminal outcome.
    async fn finish(&self, id: sandesh_core::OutboxId, status: OutboxStatus, error: Option<&str>) {
        // A terminal entry is never due again, so its next-attempt time is
        // meaningless; `now` keeps the column monotonic rather than encoding a
        // retry that will not happen.
        if let Err(error) = self
            .outbox
            .record_attempt(id, status, Timestamp::now(), error)
            .await
        {
            tracing::warn!(%error, "could not record a webhook outcome");
        }
    }

    /// Schedules the next attempt, or gives up.
    async fn reschedule(&self, entry: &sandesh_core::OutboxEntry, error: &DeliveryError) {
        let detail = error.to_string();

        // `attempts + 1` is what the row will hold once `record_attempt` has
        // incremented it, and the policy is defined in terms of the attempt
        // that just failed -- so the two cannot drift apart.
        match self.policy.schedule(entry.attempts + 1) {
            Retry::After(delay) => {
                let micros = i64::try_from(delay.as_micros()).unwrap_or(i64::MAX);
                let next = Timestamp::from_micros(Timestamp::now().as_micros() + micros);

                tracing::debug!(%detail, attempts = entry.attempts + 1, "webhook retry scheduled");
                if let Err(error) = self
                    .outbox
                    .record_attempt(entry.id, OutboxStatus::Pending, next, Some(&detail))
                    .await
                {
                    tracing::warn!(%error, "could not reschedule a webhook");
                }
            }
            Retry::GiveUp => {
                // Terminal failure stays visible in the outbox view rather than
                // being deleted: a webhook that silently stopped is the opacity
                // the Phase 7 diagnostics work exists to remove.
                tracing::warn!(%detail, "webhook gave up after exhausting its attempts");
                self.finish(entry.id, OutboxStatus::Failed, Some(&detail))
                    .await;
            }
        }
    }
}
