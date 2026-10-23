-- The FTS5 full-text index (STORAGE_DESIGN.md §7, ROADMAP gap 4).
--
-- What this is, and — more importantly — what it is not.
--
-- It is a **candidate prefilter**. It is not the search. RFC 9051 §6.4.4
-- defines the IMAP text keys as *substring* matches, and FTS5 tokenises into
-- words: `SEARCH BODY "ubject"` must match "Subject", and no word-tokenised
-- index can answer that. So `SearchQuery::matches` remains the authority on
-- every result, and this table exists only to avoid loading messages that
-- provably cannot match. Deleting it must leave search correct and only
-- slower, which `NullSearchIndex` and the parity tests both assert.
--
-- Two deviations from the §7 sketch, both deliberate:
--
--  1. **Not an external-content table.** §7 proposed `content =
--     'messages_search_src'` over a view. External content keys the index by
--     `content_rowid`, but `messages.id` is a 16-byte BLOB primary key, not a
--     rowid alias, so every lookup would need a rowid→id join and every
--     rebuild would depend on rowids staying stable — which VACUUM does not
--     promise. Storing the text here costs disk on a corpus that is already
--     bounded by the §5 message cap, and buys a table that can be dropped and
--     rebuilt from `messages` alone.
--
--  2. **`tokenize = 'trigram'`, not `unicode61`.** This is the correction that
--     makes the index sound at all. §7 specified `unicode61`, which tokenises
--     into *words* — and a word index cannot answer a substring query: the body
--     "xfoobar" contains the substring "foo", but has no token beginning "foo",
--     so a `unicode61` prefix query excludes it. That is a **false negative**,
--     the one error a prefilter may never make, and it would have shown up as
--     IMAP `SEARCH BODY` quietly missing mail. `trigram` indexes every
--     three-character sequence and matches substrings case-insensitively, which
--     is exactly RFC 9051 §6.4.4's semantics. Its cost is a larger index and a
--     three-character minimum; queries shorter than that are answered by a scan.
--
-- Maintenance is explicit, in the same transaction as the write it mirrors, so
-- the index cannot be left describing a message that no longer exists.
CREATE VIRTUAL TABLE messages_fts USING fts5(
    message_id UNINDEXED,
    subject,
    from_addr,
    to_addr,
    cc_addr,
    body_text,
    -- Trigram requires `detail = 'full'` (the default): it is a substring
    -- index, and substring matching needs positions.
    tokenize = 'trigram'
);

-- FTS5 virtual tables cannot carry a UNIQUE constraint or an index of their
-- own, so uniqueness of `message_id` is maintained by the writer (delete then
-- insert, inside one transaction) rather than declared here. This lookup table
-- gives that delete somewhere to aim: without it, removing one message means a
-- full scan of the index.
CREATE TABLE messages_fts_map (
    message_id BLOB PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    rowid_ref  INTEGER NOT NULL,
    CHECK (length(message_id) = 16)
) STRICT;

CREATE INDEX idx_messages_fts_map_rowid ON messages_fts_map(rowid_ref);
