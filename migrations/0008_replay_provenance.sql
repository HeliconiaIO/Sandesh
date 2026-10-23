-- Replay provenance: the link from a replayed message back to its source.
--
-- Recorded in two places, deliberately, and the duplication is the point.
--
-- `X-Sandesh-Replay-Of` is written into the replayed message's own bytes, so
-- the link travels with the message: it survives export to mbox, an IMAP
-- FETCH, and a `GET /messages/{id}/raw`. A CI suite asserting that its
-- application handled a replay can see it without querying a second endpoint.
--
-- This column keeps the same link *queryable*. A header is content, and
-- content is searched, not joined; "which messages are replays of this one?"
-- is a question about structure, and answering it by scanning bodies is the
-- kind of thing `0003_deliveries_by_message.sql` was written to stop doing.
--
-- The two can disagree only if someone edits the stored bytes, which nothing
-- in Sandesh does. The column is the authority for the API; the header is the
-- disclosure to whatever reads the mail.
--
-- Nullable because almost no delivery is a replay. NULL rows are omitted from
-- the partial index below, so ordinary deliveries pay nothing for this.
ALTER TABLE deliveries ADD COLUMN replay_of_message_id BLOB
    REFERENCES messages(id) ON DELETE SET NULL;

-- ON DELETE SET NULL rather than CASCADE: deleting the original message must
-- not delete the record of the replay that happened. The replay is a real
-- event that occurred; losing its provenance is a smaller harm than losing the
-- event, and a dangling reference is not an option the schema allows.

CREATE INDEX idx_deliveries_replay_of ON deliveries(replay_of_message_id)
    WHERE replay_of_message_id IS NOT NULL;
