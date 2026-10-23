-- Webhook endpoints and the durable outbox that feeds them (ADR-085, ADR-086).
--
-- Phase 8 A2's acceptance criterion is "webhooks retry on failure **and
-- survive a restart**". Restart survival is named explicitly, which is what
-- forces a table: an in-memory queue meets the retry half and silently fails
-- the half that was written down. The worker therefore holds no state between
-- ticks -- everything needed to resume a pending delivery is in `outbox`, so a
-- restart is indistinguishable from a slow tick (ADR-086).
--
-- Two tables rather than one. `endpoints` is the registration; `outbox` is the
-- work. They are separated because their lifetimes differ: an endpoint is
-- long-lived and edited by a developer, while an outbox row is created per
-- event and reaches a terminal state within minutes. Folding the URL into each
-- outbox row would also make "change the endpoint" a rewrite of every pending
-- row rather than one update.
--
-- Endpoints persist (unlike the fault registry of ADR-084, which deliberately
-- does not). The reasoning there does not transfer: a forgotten fault makes a
-- server look broken, which is a hazard, whereas a forgotten webhook endpoint
-- is inert. More decisively, an outbox row that survived a restart pointing at
-- an endpoint that did not would be incoherent -- the durability the criterion
-- demands has to cover the target as well as the payload.

CREATE TABLE webhook_endpoints (
    id          BLOB PRIMARY KEY,          -- UUID v7, time-ordered
    url         TEXT NOT NULL,
    enabled     INTEGER NOT NULL DEFAULT 1,
    created_at  INTEGER NOT NULL           -- microseconds since the epoch
) STRICT;

-- One outbox row per *placement*, not per message. `MessageReceived` is emitted
-- once per placement (`message_repo.rs`), so a message delivered to three
-- accounts produces three events; collapsing them here would invent an
-- aggregate the bus does not have and force a per-account consumer to re-derive
-- what it was already told.
CREATE TABLE webhook_outbox (
    id              BLOB PRIMARY KEY,      -- UUID v7, time-ordered
    endpoint_id     BLOB NOT NULL REFERENCES webhook_endpoints(id) ON DELETE CASCADE,
    payload         TEXT NOT NULL,         -- JSON body, built at enqueue time
    attempts        INTEGER NOT NULL DEFAULT 0,
    next_attempt_at INTEGER NOT NULL,      -- microseconds since the epoch
    status          TEXT NOT NULL,         -- 'pending' | 'delivered' | 'failed'
    last_error      TEXT,                  -- why the most recent attempt failed
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
) STRICT;

-- The worker's only query: pending rows whose next attempt is due, oldest
-- first. Without this index every tick is a full scan of a table that retains
-- delivered and terminally-failed rows for the outbox view.
CREATE INDEX idx_outbox_due
    ON webhook_outbox (status, next_attempt_at);

-- The outbox view lists newest first, per endpoint.
CREATE INDEX idx_outbox_endpoint
    ON webhook_outbox (endpoint_id, created_at DESC);
