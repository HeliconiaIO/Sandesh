# Sandesh — API Design

**Status:** Partly implemented (Phase 3). §11 records exactly what exists.

---

## 1. Principles

1. **Versioned from day one.** Everything under `/api/v1`. Breaking changes get `/api/v2`.
2. **Every endpoint maps to a domain operation.** If it does not correspond to a method on a core
   service, it does not exist. No endpoints added because they sound useful.
3. **The API is not a UI backend.** It is the programmable interface; the UI is one consumer. CI
   scripts are an equally important consumer, which is why assertions must be expressible without a
   browser.
4. **Consistent shapes.** Same pagination, same error envelope, same identifier format everywhere.
5. **Raw bytes are always reachable.** Every message exposes an endpoint returning the exact octets.

## 2. Conventions

| Aspect | Convention |
| --- | --- |
| Base path | `/api/v1` |
| Content type | `application/json; charset=utf-8`, except raw/attachment endpoints |
| Identifiers | UUID v7 as canonical strings |
| Timestamps | RFC 3339 UTC, e.g. `2026-09-01T12:00:00Z` |
| Pagination | `?limit=` (default 50, max 500) `&cursor=` — opaque cursor, not offset |
| Field casing | `snake_case`, matching the Rust types the TypeScript is generated from |
| Errors | RFC 9457 Problem Details (§7) |
| Auth | None by default (loopback). Bearer token or basic auth when `SANDESH_API_AUTH=required` (ADR-075) |
| CORS | Disabled by default; configurable origins |

**Cursor, not offset:** messages arrive continuously during testing, and offset pagination
double-counts or skips rows under insertion. Cursors are stable.

## 3. Endpoints

### 3.1 Messages

| Method | Path | Operation |
| --- | --- | --- |
| `GET` | `/api/v1/messages` | List across all accounts. Filters: `account_id`, `mailbox_id`, `q`, `unread`, `has_attachment` |
| `GET` | `/api/v1/messages/{id}` | Full message: headers, bodies, MIME summary, attachment list |
| `GET` | `/api/v1/messages/{id}/raw` | **Exact original bytes.** `Content-Type: message/rfc822` |
| `GET` | `/api/v1/messages/{id}/headers` | Ordered headers, duplicates preserved, raw + decoded |
| `GET` | `/api/v1/messages/{id}/headers/analysis` | `Received:` hop timeline, DKIM/SPF/DMARC fields, `X-Spam-*`. Parsed, never verified |
| `GET` | `/api/v1/messages/{id}/mime` | MIME tree with part paths, types, sizes, diagnostics |
| `GET` | `/api/v1/messages/{id}/part/{path}` | One MIME part, decoded (`?raw=true` for undecoded) |
| `GET` | `/api/v1/messages/{id}/html` | HTML body, sanitised for safe preview — **not built** (§11.3) |
| `GET` | `/api/v1/messages/{id}/text` | Plain-text body — **not built** (§11.3) |
| `GET` | `/api/v1/messages/{id}/links` | Every URL the HTML parts reference, classified, hosts summarised. No URL is fetched |
| `GET` | `/api/v1/messages/{id}/delivery` | Delivery record: envelope, routing outcome, connection |
| `DELETE` | `/api/v1/messages/{id}` | Delete every entry for this message |
| `POST` | `/api/v1/messages/{id}/replay` | Re-deliver a stored message with a new `Message-ID`, linked back via `replay_of` |
| `POST` | `/api/v1/messages` | Inject a message directly (`message/rfc822` body) — bypasses SMTP, for fixtures |
| `POST` | `/api/v1/send` | Compose & send a message (`application/json`) with dual placement (INBOX + Sent) |

`GET /messages/{id}` addresses the message; where per-mailbox state (UID, flags) matters, it is
addressed as an entry (§3.3). Keeping these distinct in the API mirrors the domain and avoids
inventing a fictional "the" mailbox for a message that lives in three.

#### Compose and Send (`POST /api/v1/send`)
Accepts `{ from, to: [...], cc?: [...], bcc?: [...], subject, text_body?, html_body?, in_reply_to?, references? }`.
Synthesizes a standards-compliant RFC 5322 MIME message, resolves recipient mailboxes (placing into their `INBOX`), places a copy into the sender's `Sent` mailbox, and persists via `MessageStore`. Returns `{ id, subject, placements, created_at }`.

### 3.2 Mailboxes

| Method | Path | Operation |
| --- | --- | --- |
| `GET` | `/api/v1/mailboxes` | All mailboxes, hierarchical, with counts (`?account_id=`) |
| `POST` | `/api/v1/mailboxes` | Create (`{account_id, name}`) |
| `GET` | `/api/v1/mailboxes/{id}` | Detail: `uid_next`, `uid_validity`, counts, attributes |
| `PATCH` | `/api/v1/mailboxes/{id}` | Rename or change subscription |
| `DELETE` | `/api/v1/mailboxes/{id}` | Delete mailbox and its entries |
| `GET` | `/api/v1/mailboxes/{id}/entries` | Entries with UID and flags — the IMAP-faithful view |

### 3.3 Entries (message-in-mailbox)

The entry endpoints are what make the API IMAP-faithful rather than inbox-shaped.

| Method | Path | Operation |
| --- | --- | --- |
| `GET` | `/api/v1/entries/{id}` | Entry: UID, flags, internal date, message summary |
| `PATCH` | `/api/v1/entries/{id}/flags` | `{add: [...], remove: [...]}` — maps to `STORE` |
| `POST` | `/api/v1/entries/{id}/copy` | `{target_mailbox_id}` or `{mailbox_name}` → new entry, new UID |
| `POST` | `/api/v1/entries/{id}/move` | `{target_mailbox_id}` or `{mailbox_name}` |
| `DELETE` | `/api/v1/entries/{id}` | Remove this placement only |

**A destination must belong to the source entry's account** (ADR-067). This binds `copy`, `move`
and the bulk move, in both destination forms. A destination in another account is reported as
`404`, identically to one that does not exist, so the endpoint cannot be used to discover mailbox
ids in accounts the caller cannot otherwise see; a destination equal to the entry's current
mailbox is `409`, matching IMAP rather than surfacing a storage constraint as a `500`.
| `POST` | `/api/v1/entries/bulk` | Batched flag/move/delete over an entry id list |

The bulk endpoint exists because "mark all read" and "delete all" are constant UI actions, and
issuing 500 requests for them is wasteful. It is one domain operation applied to a set, not a
convenience wrapper.

### 3.4 Accounts

| Method | Path | Operation |
| --- | --- | --- |
| `GET` | `/api/v1/accounts` | List with identities and message counts |
| `POST` | `/api/v1/accounts` | Create (`{username, password?, addresses[]}`) |
| `GET` | `/api/v1/accounts/{id}` | Detail |
| `PATCH` | `/api/v1/accounts/{id}` | Update credential or identities |
| `DELETE` | `/api/v1/accounts/{id}` | Delete account, mailboxes and entries |

### 3.5 Attachments

| Method | Path | Operation |
| --- | --- | --- |
| `GET` | `/api/v1/messages/{id}/attachments` | Metadata list |
| `GET` | `/api/v1/attachments/{id}` | Download decoded bytes, correct `Content-Type`, `Content-Disposition` |
| `GET` | `/api/v1/attachments/{id}/thumbnail` | Image thumbnail (Phase 4; 404 for non-images) |

### 3.6 Search

| Method | Path | Operation |
| --- | --- | --- |
| `GET` | `/api/v1/search?q=...` | Filter-syntax search (STORAGE_DESIGN §7); paginated |
| `GET` | `/api/v1/search/explain?q=...` | Parsed query plan — a debugging aid for the search box itself |

### 3.7 Servers, health, configuration

| Method | Path | Operation |
| --- | --- | --- |
| `GET` | `/api/v1/servers` | Per-protocol status, bound address, connection counts — **not built** (§11.3) |
| `POST` | `/api/v1/servers/{name}/start` | Start a stopped listener — **not built** |
| `POST` | `/api/v1/servers/{name}/stop` | Stop a listener — **not built** |
| `GET` | `/api/v1/health` | Liveness — no auth, no dependencies. For Docker healthchecks |
| `GET` | `/api/v1/health/ready` | Readiness: DB reachable, migrations applied, listeners bound |
| `GET` | `/api/v1/info` | Version, build, data dir, uptime, **connection strings for each protocol** |
| `GET` | `/api/v1/config` | Effective configuration, secrets redacted — **not built** (§11.3) |
| `PATCH` | `/api/v1/config` | Update runtime-mutable settings only — **not built** |

`/info` is doing real product work: it is the source for the dashboard's copy-paste SMTP/IMAP/POP3
connection details, which is the single highest-value onboarding affordance in the product.

### 3.8 Diagnostics

| Method | Path | Operation |
| --- | --- | --- |
| `GET` | `/api/v1/deliveries` | Recent deliveries with routing outcomes — **implemented** |
| `GET` | `/api/v1/connections` | Recent connections, newest first, filterable by `?protocol=`. `503` when tracing is disabled |
| `GET` | `/api/v1/connections/{id}` | One connection and its protocol transcript |
| `GET` | `/api/v1/logs` | Recent structured log entries (ring buffer), filtered by `?level=` and `?target=` |
| `GET` | `/api/v1/logs/stream` | The live log, as SSE, with the same filters |

The transcript hangs off a *connection*, not a delivery (ADR-066): one connection can carry many
deliveries or none, and IMAP and POP3 produce none at all. A delivery names the connection that
carried it, so the delivery view can offer the transcript beside the routing outcome.

`/deliveries` is also the only way to reach a transaction the server refused outright — an
oversize message stores no message, so it has no message id to be found by.

### 3.9 Rules and webhooks

| Method | Path | Operation |
| --- | --- | --- |
| `GET` | `/api/v1/rules` | The configured rules, in evaluation order |
| `POST` | `/api/v1/rules` | Create a rule |
| `DELETE` | `/api/v1/rules/{id}` | Remove a rule |
| `GET` | `/api/v1/webhooks` | The registered endpoints |
| `POST` | `/api/v1/webhooks` | Register an endpoint |
| `DELETE` | `/api/v1/webhooks/{id}` | Remove an endpoint and everything queued for it |
| `GET` | `/api/v1/webhooks/outbox` | Recent notifications and their outcomes |

### 3.10 Workspaces, import/export and faults

| Method | Path | Operation |
| --- | --- | --- |
| `GET` | `/api/v1/workspaces` | List all workspaces |
| `POST` | `/api/v1/workspaces` | Create a workspace |
| `GET` | `/api/v1/workspaces/{id}` | Fetch a workspace |
| `PATCH` | `/api/v1/workspaces/{id}` | Update name, description, `auth_required` |
| `DELETE` | `/api/v1/workspaces/{id}` | Delete a workspace and its database file |
| `POST` | `/api/v1/workspaces/{id}/activate` | Set the active workspace |
| `POST` | `/api/v1/workspaces/{id}/purge` | Purge mail and traces in a workspace |
| `POST` | `/api/v1/purge` | Purge the currently active workspace |
| `GET` | `/api/v1/export` | Bulk export as `mboxrd` (or a single `.eml`); filters `account_id`, `mailbox_id`, `limit` |
| `POST` | `/api/v1/import` | Bulk import of an `mboxrd` file or a single `.eml`, through the injection path |
| `GET` | `/api/v1/faults` | The configured faults |
| `POST` | `/api/v1/faults` | Add a fault rule |
| `DELETE` | `/api/v1/faults` | Clear every fault |

## 4. Response shapes

**List:**
```json
{
  "items": [ { "...": "..." } ],
  "next_cursor": "eyJyIjoxMjN9",
  "total_estimate": 1423
}
```
`total_estimate` is explicitly an estimate — an exact count under continuous insertion is both
expensive and immediately stale, and naming the field honestly avoids implying otherwise.

**Message summary** (list rows):
```json
{
  "id": "0192f3a1-...",
  "subject": "Password reset",
  "from": {"address": "noreply@app.test", "display_name": "App"},
  "to": [{"address": "dev@example.test", "display_name": null}],
  "received_at": "2026-09-01T12:00:00Z",
  "size": 4821,
  "has_attachments": true,
  "parse_ok": true,
  "entries": [
    {"entry_id": "...", "mailbox_id": "...", "mailbox_name": "INBOX", "uid": 42, "flags": ["\\Seen"]}
  ]
}
```
A summary carries **all** its entries. The list view can then show a message present in two mailboxes
without a second round trip, and the API never has to pretend a message has one home.

## 5. WebSocket

**Endpoint:** `GET /api/v1/events` (upgrade). One connection carries all subscriptions.

**Client → server:**
```json
{"type": "subscribe",   "filters": {"account_id": "...", "mailbox_id": "...", "events": ["MessageReceived"]}}
{"type": "unsubscribe", "subscription_id": "..."}
{"type": "ping"}
```

**Server → client:**
```json
{
  "type": "event",
  "subscription_id": "sub-1",
  "seq": 10432,
  "event": {
    "kind": "MessageReceived",
    "occurred_at": "2026-09-01T12:00:00Z",
    "payload": {"message_id": "...", "entry_id": "...", "mailbox_id": "...", "account_id": "...", "uid": 42}
  }
}
```

Rules:
- **Events carry identifiers and minimal context, never full message bodies.** A client that wants
  the message fetches it. This keeps frame size bounded, avoids a second serialisation of the domain,
  and means a slow consumer cannot be starved by a 20 MB attachment.
- `seq` is a monotonic per-connection sequence number, so a client can detect gaps after a drop.
- On reconnect a client may send `{"type":"resume","after_seq":N}` (ADR-100). `after_seq` is a
  **`bus_seq`**, not the per-connection `seq` above: the per-connection counter restarts at 1 on
  every connection, so it cannot name a position across the reconnect resume exists for. Frames
  carry both.
  - If the window still holds everything after `N`, the server sends
    `{"type":"resumed","after_seq":N,"count":M}` followed by the `M` missed events, each marked
    `"replayed":true` so a client can tell history from live traffic. `count` is `0` when the
    client was already current — silence would be indistinguishable from a stall.
  - Otherwise it sends one `{"type":"resume_gap","after_seq":N,"oldest_seq":O}` and **no events**,
    and the client refetches. Replaying a partial history would be worse than replaying none: the
    client would have no way to know it was incomplete. (Earlier drafts called this frame
    `resync_required`; the name changed to say what happened rather than what to do about it.)
  Being explicit about a lost-events state is far better than silently diverging.
  - A client tracks the highest `bus_seq` it has **processed**, advancing it with a maximum rather
    than by assignment: replayed and live frames arrive on the same path, and arrival order is not
    guaranteed (§28). It does not advance on `resume_gap`, so the next reconnect retries the same
    position instead of skipping the events permanently. `web/src/api/events.ts` is the reference
    implementation.
- Server pings every 30s; a connection missing two is closed.
- Backpressure: a per-connection bounded queue (default 1000). On overflow the connection receives
  `{"type":"lagged","dropped":N}` and must resync. See [EVENT_MODEL.md](EVENT_MODEL.md).

## 6. Server-Sent Events

`GET /api/v1/events/sse` mirrors the WebSocket stream one-way. Trivial to add over the same event
subscriber, and it makes `curl -N` a usable way to watch mail arrive in a shell script — a genuine CI
and debugging affordance, not surface area for its own sake.

## 7. Errors

RFC 9457 Problem Details:

```json
{
  "type": "https://sandesh.dev/errors/not-found",
  "title": "Message not found",
  "status": 404,
  "detail": "No message with id 0192f3a1-...",
  "instance": "/api/v1/messages/0192f3a1-..."
}
```

Mapping from `CoreError` is specified in [CORE_SPEC.md §10](CORE_SPEC.md). Status codes used: `400`,
`401`, `403`, `404`, `409`, `413`, `422`, `429`, `500`, `503`, `507`.

## 8. Type generation

**As built (Phase 4).** One chain, no hand-written link in it:

```text
#[utoipa::path] on a handler ─┐
#[derive(ToSchema)] on a DTO ─┼─▶ docs/openapi.json ─▶ web/src/generated/api.ts ─▶ the UI
```

`cargo xtask generate-api` writes both artefacts; `--check` verifies them and is what CI runs. Both
are committed, so a contract change is reviewable in a diff (ADR-028).

The `OpenAPI` 3.1 document is served at **`/api/openapi.json`** — outside the versioned prefix,
because the document describes every version it knows about.

`ts-rs` is **not** used. Deriving the TypeScript from the `OpenAPI` document instead of separately
from the Rust types keeps one source of truth rather than two that can disagree (ADR-027).

**A caveat worth knowing.** `utoipa` cannot verify a `#[utoipa::path]` annotation against its
handler's actual return type. A response documented as an object while the handler sends an array
compiles cleanly and passes the drift check, because both artefacts are generated from the same
wrong annotation. `tests/http_surface.rs` closes this by comparing the documented shape against a
live response.

**Not yet built:** the rendered API reference inside the UI. The About view links to the raw
document instead.

## 9. Static assets

The built React app is embedded in the binary (`rust-embed`) and served from a router fallback.
Unknown non-`/api` paths fall through to `index.html` for client-side routing.

**Route precedence is load-bearing.** An axum fallback also catches unmatched paths *inside* a
nested router, so `/api/v1/does-not-exist` originally returned `index.html` with a `200` — which
every client would read as success. An explicit `/api/{*rest}` route now returns the API's `404`
before the fallback is reached, and a test holds it there (ADR-029).

If no UI has been built, the API still serves normally and a request for the UI explains how to
build one.

**Struck, not pending:** `--no-web` (ADR-076). The UI routes are always mounted, and deliberately
so — they are the router's fallback, so they answer only what nothing else claimed, and since
ADR-075 they answer it only to an authenticated caller. An API-only mode has no consumer; the
criterion is reinstated if one appears.

## 10. Open decisions

- **OD-6** Whether `PATCH /config` should be able to change listener ports at runtime, or whether
  those require a restart. Restart-only is simpler and safer. *Leaning: restart-only in v1.*
- **OD-7** Whether to expose a GraphQL endpoint. *Leaning: no.* REST plus WebSocket covers both
  consumers; GraphQL would add a second contract to keep in sync for little gain.

---

## 11. Implementation status

The **full read surface plus message injection, entry and mailbox mutation, attachments, search
and the event streams** are implemented. What remains missing is missing for a reason, given in
§11.3. `docs/openapi.json` is generated from the handlers and is the authoritative list; this
section is the annotated view of it and CI fails if the generated contract drifts (ADR-028).

Base path `/api/v1`, RFC 9457 errors, UUID-v7 string identifiers, RFC 3339 UTC timestamps and
`snake_case` fields are all as specified in §2.

### 11.1 Implemented

| Method | Path | Notes |
| --- | --- | --- |
| `GET` | `/health` | Liveness. Touches no dependency |
| `GET` | `/health/ready` | `503` when the database is unreachable |
| `GET` | `/info` | Version, real bound addresses, WebSocket path, database location |
| `GET` | `/accounts`, `/accounts/{id}` | With identities |
| `GET` | `/mailboxes`, `/mailboxes/{id}` | With counts, `uid_next`, `uid_validity` |
| `GET` | `/mailboxes/{id}/entries` | UID-cursor paginated — the IMAP-faithful view |
| `GET` | `/entries/{id}` | UID, flags, internal date |
| `PATCH` | `/entries/{id}/flags` | `{add, remove}` → IMAP `STORE` |
| `DELETE` | `/entries/{id}` | Removes that placement only; the message survives |
| `GET` | `/messages` | Summaries; filters `mailbox_id`, `account_id`; paginated |
| `GET` | `/messages/{id}` | Headers, attachments, diagnostics, all placements |
| `GET` | `/messages/{id}/raw` | **Exact original octets**, `message/rfc822` |
| `GET` | `/messages/{id}/headers` | Ordered, duplicates preserved, raw and decoded |
| `GET` | `/messages/{id}/headers/analysis` | The interpreted view: hop timeline with delays, DKIM/SPF/DMARC, `X-Spam-*`. Carries a `disclaimer`; nothing is verified |
| `GET` | `/messages/{id}/mime` | Part tree as byte ranges into the raw message |
| `GET` | `/messages/{id}/part/{path}` | One part's bytes |
| `GET` | `/messages/{id}/delivery` | Envelope and per-recipient routing outcome |
| `GET` | `/messages/{id}/attachments` | Attachment metadata for the message |
| `GET` | `/attachments/{id}` | One attachment's decoded bytes |
| `GET` | `/attachments/{id}/thumbnail` | Rasterised preview for image attachments |
| `POST` | `/messages` | Message injection, bypassing SMTP (Phase 5 core service) |
| `DELETE` | `/messages/{id}` | Removes the message and all its placements |
| `POST` | `/accounts`, `PATCH`/`DELETE` `/accounts/{id}` | Account lifecycle |
| `POST` | `/mailboxes`, `PATCH`/`DELETE` `/mailboxes/{id}` | Mailbox lifecycle |
| `POST` | `/entries/{id}/copy`, `/entries/{id}/move` | IMAP `COPY` / `MOVE` semantics |
| `POST` | `/entries/bulk` | Batched entry mutation |
| `GET` | `/search`, `/search/explain` | Query execution and its query plan |
| `GET` | `/deliveries` | All delivery records, including rejections with no message id (Phase 7 A1) |
| `POST` | `/send` | Outbound submission |
| `GET` | `/messages/{id}/links` | Link extraction; nothing is fetched |
| `POST` | `/messages/{id}/replay` | Re-delivery with provenance |
| `GET`/`POST`/`DELETE` | `/rules`, `/rules/{id}` | Message rules (§3.9) |
| `GET`/`POST`/`DELETE` | `/webhooks`, `/webhooks/{id}`, `/webhooks/outbox` | Webhook endpoints and delivery outbox (§3.9) |
| `GET`/`POST`/`PATCH`/`DELETE` | `/workspaces`, `/workspaces/{id}`, `/{id}/activate`, `/{id}/purge`, `/purge` | Workspace lifecycle (§3.10) |
| `GET` / `POST` | `/export`, `/import` | `mboxrd` / `.eml` round trip |
| `GET`/`POST`/`DELETE` | `/faults` | Fault injection rules |
| `GET` | `/connections`, `/connections/{id}`, `/logs`, `/logs/stream` | Diagnostics (§3.8) |
| `GET` | `/events` | WebSocket. See [EVENT_MODEL.md](EVENT_MODEL.md) §13 |
| `GET` | `/events/sse` | The SSE mirror of the same stream |
| `GET` | `/api/openapi.json` | The generated contract. **Outside** the `/api/v1` prefix |

A catch-all `/api/{*rest}` returns an RFC 9457 `404` rather than falling through to the static
asset handler, so a mistyped API path never answers with the UI's `index.html`.

### 11.2 Two clarifications the implementation forced

**A summary's `to` is the `To:` header, not the envelope recipients.** §4's example implied
envelope recipients, but envelope recipients belong to a *delivery*, not to immutable content: one
message delivered to three addresses has one set of bytes and three envelope recipients. They are
reported by `/messages/{id}/delivery`. Where the message actually landed is visible in `entries`,
which is what makes `Bcc` testing work — and `entries` carries **every** placement, so a message in
three mailboxes is one summary with three entries, exactly as §4 requires.

**The root MIME part is addressed as `root`.** The domain renders the root part path as the empty
string, which no URL path segment can carry, so `/messages/{id}/part/root` addresses it and every
other part uses the ordinary dotted form. Each part in the `/mime` response carries a `url_path`
field giving the spelling to use, so a client never has to know this rule.

### 11.3 Still not implemented

| Not built | Why |
| --- | --- |
| `/messages/{id}/html`, `/text` | HTML sanitisation is a real security surface. The UI renders sanitised content from the part endpoints instead, so no separate endpoint has been needed |
| ~~`/connections`, `/connections/{id}`~~ | **Built** (ADR-077). Recent transcripts newest-first with `?protocol=` filtering, and one record by id. Returns `503` when tracing is disabled, which is a different answer from an empty list |
| ~~`/messages/{id}/headers/analysis`~~ | **Built** (ADR-078). The `Received:` chain as a chronological hop list with per-hop delay, parsed DKIM/SPF/DMARC fields and `X-Spam-*`. Parsed, never verified: the response carries a `disclaimer` saying so. The flat `/headers` is unaffected |
| `/metrics` | **Built** (ADR-079, Phase 7 A4). Prometheus text format, outside `/api/v1` beside `/api/openapi.json`. Authenticated like every other path: a scrape discloses message, mailbox and delivery volumes, which is more than `/info` reports and `/info` is not exempt either. Absent from `docs/openapi.json` by design — it is not JSON and not part of the versioned contract |
| `/api/v1/logs` | **Built** (ADR-081, Phase 7 A6). The bounded log backlog, filtered by `level` (at or above) and `target` (substring). Bounded by the ring itself, so there is no cursor: the whole buffer fits one response |
| `/api/v1/logs/stream` | **Built** (ADR-081). The live log as SSE, filtered server-side by the same parameters. SSE rather than a second `WebSocket`: the stream is one-directional. Carries no message content or credentials — the log never held them, and a test enforces it |
| `/servers`, `/servers/{name}/start`, `/stop` | Runtime listener control interacts with OD-6, still open |
| `/config`, `PATCH /config` | Depends on OD-6 |
| ~~Auth (§2)~~ | **Built** (ADR-075). Bearer and basic auth behind `SANDESH_API_AUTH`, HTTPS behind `SANDESH_API_TLS`, both off by default so loopback stays frictionless. Only `/api/v1/health` is exempt |

### 11.4 Pagination as built

`?limit=` defaults to 50 and is capped at 500; `0` and anything above the cap are rejected with
`400`. `?cursor=` is a UID, opaque to clients. UIDs are monotonic and never reused, so a row
inserted mid-page appears *after* the cursor rather than shifting rows already seen — the
insertion-safety property §2 requires of cursors.

### 11.5 Error model as built

Every error is an RFC 9457 problem document with `application/problem+json`:

```json
{"type": "https://sandesh.dev/errors/not-found", "title": "Not found",
 "status": 404, "detail": "Message not found"}
```

`type` slugs are the stable contract: `not-found`, `bad-request`, `unprocessable`, `conflict`,
`internal`, `unavailable`, `unauthorized`. Statuses in use: `200`, `201`, `204`, `400`, `401`, `404`, `409`, `422`, `500`, `503`.

**Internal failures are redacted in exactly one place.** `ApiError::Internal` always renders as
"An internal error occurred." and the real message is logged instead, so a SQLite string, a
filesystem path or a `Debug` dump cannot escape by someone forgetting to sanitise at a call site.

### 11.6 CORS

Disabled by default, as §2 specifies, and a `*` origin is **rejected at startup** rather than
warned about: the Phase 4 UI is same-origin and needs no CORS, so the only legitimate use is a
named development origin — and a wildcard on a local mail server would let any page a developer
visits read their mail.
