-- Auto-tagging rules and the record of which ones fired (ADR-087).
--
-- Two tables with different jobs. `rules` is configuration a developer curates;
-- `delivery_rules` is history, written once per delivery and never updated.
--
-- Rules persist, unlike the fault registry of ADR-084 and like the webhook
-- endpoints of ADR-086. Phase 8 has now made this choice three times, so the
-- reasoning matters more than the outcome: a forgotten fault makes a server
-- look broken, which is a hazard restart must clear, whereas the worst a
-- tagging rule does is put a keyword on a message -- visible, reversible
-- through IMAP STORE, and explained by `delivery_rules` below. Losing a curated
-- rule set on every restart would make the feature useless on a long-running
-- development server.

CREATE TABLE rules (
    id         BLOB PRIMARY KEY,        -- UUID v7, time-ordered
    name       TEXT NOT NULL,           -- shown in the delivery record
    conditions TEXT NOT NULL,           -- JSON array; see `sandesh-core::rules`
    action     TEXT NOT NULL,           -- JSON object
    enabled    INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL         -- microseconds since the epoch
) STRICT;

-- Conditions and actions are JSON rather than columns because both are sum
-- types: a condition is one of three shapes and an action will grow more in
-- Pass B. Normalising them would mean a table per variant and a join per
-- evaluation, for data that is only ever read whole, as a set, once per
-- delivery.

-- Which rules fired for a delivery, and what each did.
--
-- The rule's *name* and the applied *keyword* are copied in, not referenced.
-- A rule edited or deleted afterwards must not rewrite history, and a delivery
-- that renders as "rule 019a... matched" once its rule is gone explains
-- nothing -- which is precisely the opacity this increment exists to remove.
-- Same reasoning as ADR-086's webhook payload, rendered at enqueue time.
--
-- Deliberately no foreign key to `rules`: the row must outlive the rule.
CREATE TABLE delivery_rules (
    delivery_id BLOB NOT NULL REFERENCES deliveries(id) ON DELETE CASCADE,
    rule_id     BLOB NOT NULL,          -- not a FK; see above
    rule_name   TEXT NOT NULL,          -- as it stood when it fired
    keyword     TEXT NOT NULL,          -- what was added

    PRIMARY KEY (delivery_id, rule_id, keyword)
) STRICT;
