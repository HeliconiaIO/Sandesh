-- CONDSTORE (RFC 7162): a mailbox-scoped monotonic modification sequence.
--
-- 0001 reserved `message_entries.mod_seq` as a per-entry counter defaulting to
-- 1 and incremented in place. That is not a MODSEQ. RFC 7162 §3 requires a
-- value that is monotonic *within the mailbox* and never reused, so that
-- "everything changed since X" is answerable by comparison alone. A per-entry
-- counter cannot answer it: two entries changed at different times both sit at
-- 2, and nothing records the mailbox's high-water mark for HIGHESTMODSEQ.
--
-- This migration adds that high-water mark and rebases the existing per-entry
-- values onto the shared sequence. It is additive: no column is dropped and no
-- unrelated table is touched.

-- ------------------------------------------------------- mailboxes.high water --
-- The allocator. Every stamp is `UPDATE ... SET highest_mod_seq =
-- highest_mod_seq + 1 RETURNING`, mirroring how `uid_next` allocates UIDs
-- (mailbox_repo::allocate_uid) — one atomic statement, so two concurrent flag
-- changes cannot be handed the same value.
--
-- RFC 7162 §3 requires HIGHESTMODSEQ to be non-zero for a mailbox that has
-- ever been reported, and permits 1 as the base for a mailbox with no changes.
-- Existing rows therefore start at 1 and are raised below to cover whatever
-- per-entry values 0001 already wrote.
ALTER TABLE mailboxes
    ADD COLUMN highest_mod_seq INTEGER NOT NULL DEFAULT 1;

-- Preserve existing data: raise each mailbox's high-water mark to at least the
-- largest per-entry value it already contains, so no already-issued mod_seq
-- can ever be allocated a second time. Mailboxes with no entries keep 1.
--
-- Written as a correlated subquery rather than a join because SQLite's UPDATE
-- ... FROM is comparatively recent and this must apply on older engines.
UPDATE mailboxes
SET highest_mod_seq = MAX(
    highest_mod_seq,
    COALESCE(
        (SELECT MAX(mod_seq) FROM message_entries
          WHERE message_entries.mailbox_id = mailboxes.id),
        1
    )
);

-- Guard the invariant in the schema, not only in Rust. A zero or negative
-- high-water mark would make HIGHESTMODSEQ unreportable and break the
-- comparison CONDSTORE is built on. SQLite cannot add a CHECK to an existing
-- table via ALTER, so this is enforced on the allocator's write path and
-- asserted by the migration test rather than declared here.

-- ------------------------------------------------- message_entries.mod_seq ----
-- The column already exists and keeps its name and type; only its *meaning*
-- changes, from a per-entry counter to a value drawn from the mailbox
-- sequence. Existing values are left in place: they are already <= their
-- mailbox's new high-water mark, so they remain valid, comparable, and never
-- re-issued. Rebasing them to distinct values would be a fabrication — the
-- ordering information was never recorded and cannot be recovered.

-- An entry's mod_seq is the hot predicate for `SEARCH MODSEQ n`,
-- `FETCH (CHANGEDSINCE n)` and `STORE ... UNCHANGEDSINCE n`, always scoped to
-- one mailbox. The composite index makes each a range scan rather than a scan
-- of the mailbox.
CREATE INDEX idx_entry_mod_seq ON message_entries(mailbox_id, mod_seq);
