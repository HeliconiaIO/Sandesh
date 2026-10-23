-- An index for looking a delivery up by the message it produced.
--
-- 0001 indexed `deliveries(received_at DESC)`, which serves the "recent
-- deliveries" listing, and nothing else. The Delivery view asks the opposite
-- question — "how did *this* message get here?" — and with no index for it the
-- API answered by fetching the 500 most recent records and scanning them in
-- memory. That is not merely slow: a message older than the scan window has a
-- delivery record the scan cannot see, so the endpoint returned 404 for data
-- that plainly exists (Phase 7, finding (a)).
--
-- `message_id` is nullable and not unique. Nullable because a transaction
-- rejected outright stores no message and still deserves a record; SQLite
-- omits NULL rows from an index automatically, so those cost nothing here and
-- are reached through the recency index instead. Not unique because nothing in
-- the schema forbids two delivery records naming one message, and an index
-- that asserted otherwise would turn a diagnostics record into a constraint on
-- delivery behaviour.

CREATE INDEX idx_deliveries_message ON deliveries(message_id);
