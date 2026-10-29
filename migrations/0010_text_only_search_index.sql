-- Rebuild the search index under the text-only rule.
--
-- Until this release the indexer decoded *every* non-multipart part and indexed
-- it as trigrams, attachments included. A PDF or image pushed through
-- `from_utf8_lossy` is noise no search can match, and it made the index four
-- times the size of the mail: 4.1 GB of a 5.2 GB database for 100,000 messages
-- in the Phase 10 load test. The indexer now reads text parts only
-- (`MimePart::is_searchable_text`); rebuilt, that index was 432 MB.
--
-- Dropped and recreated rather than pruned. Deleting a document from FTS5
-- removes each of its trigrams one by one, and pruning the binary-bearing rows
-- that way kept a 100,000-message server from starting for nearly seven
-- minutes. Dropping takes a second. The repair task started with each
-- workspace (`SqliteSearchIndex::unindexed`) then reindexes every message from
-- its stored bytes in the background, at roughly 280 messages a second, while
-- the server is up. Until it finishes, text search scans instead of narrowing:
-- slower, never wrong, because the index is only a prefilter (0005).
--
-- The schema below is 0005's, unchanged.

DROP TABLE messages_fts_map;
DROP TABLE messages_fts;

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

CREATE TABLE messages_fts_map (
    message_id BLOB PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    rowid_ref  INTEGER NOT NULL,
    CHECK (length(message_id) = 16)
) STRICT;

CREATE INDEX idx_messages_fts_map_rowid ON messages_fts_map(rowid_ref);
