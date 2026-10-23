-- Sandesh initial schema.
--
-- Implements docs/STORAGE_DESIGN.md §2. The structural decision that shapes
-- everything here is ADR-009: `messages` has NO `mailbox_id`. A message's
-- membership in a mailbox lives in `message_entries`, which carries the UID and
-- flags for that placement. IMAP COPY puts one message in two mailboxes with
-- two UIDs and two independent flag sets; a single-owner column cannot express
-- that without duplicating the message.
--
-- All tables are STRICT (SQLite 3.37+): column types are enforced, so a type
-- error cannot silently corrupt stored mail.

-- ---------------------------------------------------------------- accounts --

CREATE TABLE accounts (
    id               BLOB PRIMARY KEY,       -- UUID v7, time-ordered
    username         TEXT NOT NULL UNIQUE,
    credential_hash  TEXT,                   -- Argon2id; NULL = no credential
    credential_plain TEXT,                   -- only when CRAM-MD5 is enabled
    is_catch_all     INTEGER NOT NULL DEFAULT 0,
    created_at       INTEGER NOT NULL,       -- unix micros, UTC

    CHECK (is_catch_all IN (0, 1)),
    CHECK (length(id) = 16)
) STRICT;

-- At most one catch-all account. A partial unique index expresses this
-- directly; enforcing it in Rust alone would leave the invariant unguarded
-- against concurrent writers.
CREATE UNIQUE INDEX idx_accounts_catch_all
    ON accounts(is_catch_all) WHERE is_catch_all = 1;

CREATE TABLE email_identities (
    id          BLOB PRIMARY KEY,
    account_id  BLOB NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    address     TEXT NOT NULL,               -- normalised: local@lowercased-domain
    address_raw TEXT NOT NULL,               -- exactly as supplied
    is_primary  INTEGER NOT NULL DEFAULT 0,

    CHECK (is_primary IN (0, 1))
) STRICT;

-- One address routes to exactly one account: the uniqueness that makes
-- delivery routing deterministic.
CREATE UNIQUE INDEX idx_identity_address ON email_identities(address);
CREATE INDEX idx_identity_account ON email_identities(account_id);

-- --------------------------------------------------------------- mailboxes --

CREATE TABLE mailboxes (
    id           BLOB PRIMARY KEY,
    account_id   BLOB NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,              -- full path, '/' delimiter, Unicode
    parent_id    BLOB REFERENCES mailboxes(id) ON DELETE CASCADE,
    uid_validity INTEGER NOT NULL,
    uid_next     INTEGER NOT NULL DEFAULT 1, -- never decreases, even after EXPUNGE
    attributes   TEXT NOT NULL DEFAULT '',   -- CSV: \Sent,\Trash,\Noselect,...
    subscribed   INTEGER NOT NULL DEFAULT 1,
    created_at   INTEGER NOT NULL,

    CHECK (subscribed IN (0, 1)),
    CHECK (uid_next >= 1),
    CHECK (length(name) > 0)
) STRICT;

CREATE UNIQUE INDEX idx_mailbox_name ON mailboxes(account_id, name);
CREATE INDEX idx_mailbox_parent ON mailboxes(parent_id);

-- ---------------------------------------------------------------- messages --

CREATE TABLE messages (
    id            BLOB PRIMARY KEY,
    sha256        BLOB NOT NULL,
    size          INTEGER NOT NULL,          -- RFC822.SIZE, authoritative
    raw_inline    BLOB,                      -- set iff size <= threshold
    raw_blob_ref  TEXT,                      -- set iff stored externally
    received_at   INTEGER NOT NULL,
    source        TEXT NOT NULL,             -- 'smtp' | 'imap_append' | 'api'

    -- SMTP envelope, distinct from the message headers. RCPT TO is recorded
    -- per-delivery (it is multi-valued), not here.
    env_mail_from  TEXT,                     -- NULL = null reverse-path <>
    env_helo       TEXT,
    env_peer       TEXT,
    env_tls        TEXT,
    env_auth_ident TEXT,

    -- Denormalised header values. A deliberate, documented duplication:
    -- rendering a 500-row inbox must not mean parsing 500 messages. Derived
    -- from the raw bytes and rebuilt on reparse; raw stays authoritative.
    hdr_subject     TEXT,
    hdr_from        TEXT,
    hdr_to          TEXT,
    hdr_cc          TEXT,
    hdr_bcc         TEXT,
    hdr_date        INTEGER,
    hdr_message_id  TEXT,
    hdr_in_reply_to TEXT,

    has_attachments INTEGER NOT NULL DEFAULT 0,
    has_inline      INTEGER NOT NULL DEFAULT 0,
    parse_version   INTEGER NOT NULL,        -- bump invalidates derived data
    parse_ok        INTEGER NOT NULL DEFAULT 1,

    -- Raw bytes live in exactly one place, always.
    CHECK ((raw_inline IS NULL) != (raw_blob_ref IS NULL)),
    CHECK (size >= 0),
    CHECK (length(sha256) = 32),
    CHECK (has_attachments IN (0, 1)),
    CHECK (has_inline IN (0, 1)),
    CHECK (parse_ok IN (0, 1)),
    CHECK (source IN ('smtp', 'imap_append', 'api'))
) STRICT;

CREATE INDEX idx_messages_received ON messages(received_at DESC);
CREATE INDEX idx_messages_sha256   ON messages(sha256);
CREATE INDEX idx_messages_msgid    ON messages(hdr_message_id);

-- --------------------------------------------------------- message_entries --
-- The linchpin (ADR-009). One row = one placement of one message in one
-- mailbox, carrying that placement's own UID and its own flags.

CREATE TABLE message_entries (
    id            BLOB PRIMARY KEY,
    message_id    BLOB NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    mailbox_id    BLOB NOT NULL REFERENCES mailboxes(id) ON DELETE CASCADE,
    uid           INTEGER NOT NULL,
    flags         INTEGER NOT NULL DEFAULT 0, -- bitmask of the 5 system flags
    internal_date INTEGER NOT NULL,           -- IMAP INTERNALDATE
    added_at      INTEGER NOT NULL,
    mod_seq       INTEGER NOT NULL DEFAULT 1, -- reserved for CONDSTORE (OD-2)

    CHECK (uid > 0),                          -- RFC 9051 reserves UID 0
    CHECK (flags >= 0 AND flags < 32)         -- five defined system flags
) STRICT;

-- UID uniqueness within a mailbox. This is a correctness constraint, not an
-- optimisation: a duplicate UID corrupts every connected client's cache.
CREATE UNIQUE INDEX idx_entry_uid ON message_entries(mailbox_id, uid);

-- A message may be placed in a given mailbox only once. Two envelope
-- recipients resolving to the same mailbox produce one entry
-- (CORE_SPEC §7.3), and the database enforces that rather than trusting
-- the caller.
CREATE UNIQUE INDEX idx_entry_unique_placement
    ON message_entries(mailbox_id, message_id);

CREATE INDEX idx_entry_message ON message_entries(message_id);
CREATE INDEX idx_entry_flags   ON message_entries(mailbox_id, flags);
CREATE INDEX idx_entry_intdate ON message_entries(mailbox_id, internal_date);

CREATE TABLE entry_keywords (
    entry_id BLOB NOT NULL REFERENCES message_entries(id) ON DELETE CASCADE,
    keyword  TEXT NOT NULL,

    PRIMARY KEY (entry_id, keyword)
) STRICT;

-- ------------------------------------------------------- parsed structure --

CREATE TABLE message_headers (
    message_id    BLOB NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    ordinal       INTEGER NOT NULL,          -- received order; significant
    name          TEXT NOT NULL,             -- case preserved
    name_lower    TEXT NOT NULL,             -- for lookup
    raw_value     BLOB NOT NULL,             -- exact bytes, folding intact
    decoded_value TEXT,                      -- RFC 2047 decoded, best effort
    byte_start    INTEGER NOT NULL,
    byte_end      INTEGER NOT NULL,

    PRIMARY KEY (message_id, ordinal),
    CHECK (byte_end >= byte_start)
) STRICT;

CREATE INDEX idx_headers_name ON message_headers(message_id, name_lower);

CREATE TABLE mime_parts (
    message_id    BLOB NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    part_path     TEXT NOT NULL,             -- IMAP part number: '', '1', '2.1'
    parent_path   TEXT,
    content_type  TEXT NOT NULL,
    charset       TEXT,
    encoding      TEXT NOT NULL,
    disposition   TEXT,
    filename      TEXT,
    content_id    TEXT,
    header_start  INTEGER NOT NULL,          -- byte ranges into the raw message
    header_end    INTEGER NOT NULL,
    body_start    INTEGER NOT NULL,
    body_end      INTEGER NOT NULL,
    decoded_size  INTEGER,
    is_attachment INTEGER NOT NULL DEFAULT 0,

    PRIMARY KEY (message_id, part_path),
    CHECK (header_end >= header_start),
    CHECK (body_end >= body_start),
    CHECK (is_attachment IN (0, 1))
) STRICT;

CREATE TABLE attachments (
    id           BLOB PRIMARY KEY,
    message_id   BLOB NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    part_path    TEXT NOT NULL,
    filename     TEXT,
    content_type TEXT NOT NULL,
    content_id   TEXT,
    size         INTEGER NOT NULL,           -- decoded size
    is_inline    INTEGER NOT NULL DEFAULT 0,
    cache_ref    TEXT,                       -- decoded bytes cached; may be NULL

    FOREIGN KEY (message_id, part_path)
        REFERENCES mime_parts(message_id, part_path) ON DELETE CASCADE,
    CHECK (is_inline IN (0, 1)),
    CHECK (size >= 0)
) STRICT;

CREATE INDEX idx_attach_message ON attachments(message_id);

-- -------------------------------------------------------------- deliveries --

CREATE TABLE deliveries (
    id          BLOB PRIMARY KEY,
    message_id  BLOB REFERENCES messages(id) ON DELETE CASCADE,
    source      TEXT NOT NULL,
    peer        TEXT,
    mail_from   TEXT,
    helo        TEXT,
    received_at INTEGER NOT NULL,
    duration_us INTEGER NOT NULL,
    trace       TEXT,

    CHECK (source IN ('smtp', 'imap_append', 'api'))
) STRICT;

CREATE INDEX idx_deliveries_received ON deliveries(received_at DESC);

CREATE TABLE delivery_recipients (
    delivery_id         BLOB NOT NULL REFERENCES deliveries(id) ON DELETE CASCADE,
    rcpt_to             TEXT NOT NULL,
    resolved_account_id BLOB REFERENCES accounts(id) ON DELETE SET NULL,
    entry_id            BLOB REFERENCES message_entries(id) ON DELETE SET NULL,
    outcome             TEXT NOT NULL,
    reason              TEXT,

    PRIMARY KEY (delivery_id, rcpt_to),
    CHECK (outcome IN ('delivered', 'catch_all', 'rejected'))
) STRICT;

-- -------------------------------------------------------------------- meta --

CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;
