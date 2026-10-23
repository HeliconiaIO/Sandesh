-- Per-connection protocol transcripts (ADR-066).
--
-- Phase 7 requires that "every connection to every protocol produces a
-- retrievable trace". `deliveries.trace` could not satisfy that, because a
-- delivery is not a connection: one SMTP connection carries many transactions,
-- a connection that delivers nothing (a probe, a rejected RCPT, a failed
-- STARTTLS) produces no delivery row at all, and IMAP and POP3 never produce
-- one. Two of the three protocols had nowhere to record a transcript.
--
-- One row per *accepted socket*, not per `Connection`: a STARTTLS upgrade
-- builds a second `Connection` over the same socket (ADR-036), and splitting
-- that into two unrelated traces would hide exactly the handshake a developer
-- is trying to debug.
--
-- What a transcript contains is bounded by the privacy stance in
-- `crates/sandesh-app/src/logging.rs`: command and response lines only, DATA
-- payloads elided to a byte count, credentials never captured. A transcript is
-- a log, and this server holds password-reset links and one-time codes.

CREATE TABLE connections (
    id            BLOB PRIMARY KEY,          -- UUID v7, time-ordered
    protocol      TEXT NOT NULL,
    peer          TEXT,                      -- NULL for an in-memory transport
    encrypted     INTEGER NOT NULL DEFAULT 0,
    opened_at     INTEGER NOT NULL,          -- microseconds since the epoch
    closed_at     INTEGER,                   -- NULL while the connection is open
    -- How the conversation ended. NULL while still open; the domain owns the
    -- vocabulary, so this is deliberately not a CHECK-constrained enum — a new
    -- outcome must not require a migration to record.
    disposition   TEXT,
    -- The transcript, newline-separated. NULL means nothing was captured;
    -- empty means a connection was accepted and said nothing.
    transcript    TEXT,
    -- True when the transcript hit its line or byte cap and was cut short, so
    -- a reader can tell a short conversation from a truncated one.
    truncated     INTEGER NOT NULL DEFAULT 0,

    CHECK (protocol IN ('smtp', 'imap', 'pop3')),
    CHECK (encrypted IN (0, 1)),
    CHECK (truncated IN (0, 1)),
    -- A connection cannot close before it opened.
    CHECK (closed_at IS NULL OR closed_at >= opened_at)
) STRICT;

-- The listing is "most recent connections", mirroring idx_deliveries_received.
CREATE INDEX idx_connections_opened ON connections(opened_at DESC);

-- Filtering the listing by protocol is the other access pattern the trace
-- viewer needs: "show me the IMAP conversations".
CREATE INDEX idx_connections_protocol ON connections(protocol, opened_at DESC);

-- ------------------------------------------------------------- deliveries --

-- Links a delivery to the conversation that produced it, so the delivery view
-- can offer the transcript beside the routing outcome. Nullable: an API
-- injection has no connection, and a delivery recorded before this migration
-- has none either.
ALTER TABLE deliveries ADD COLUMN connection_id BLOB REFERENCES connections(id) ON DELETE SET NULL;

CREATE INDEX idx_deliveries_connection ON deliveries(connection_id);

-- `deliveries.trace` is now dead: it was written NULL by the only production
-- call site and is superseded by `connections.transcript`. Dropped rather than
-- left in place, because a second column that could hold a transcript is a
-- second place for one to diverge.
--
-- No data is lost — the column has never held a value. Safe to drop here:
-- SQLite has supported DROP COLUMN since 3.35, and no index or constraint
-- references `trace`.
ALTER TABLE deliveries DROP COLUMN trace;
