# Sandesh — Decisions

**Status:** Phase 0. Records the significant architectural decisions, the open questions, and the
§33 consistency audit.

---

## 1. Architecture decision records

### ADR-001 — Rust, Tokio, and a hexagonal core

**Decision.** Rust with Tokio; `sandesh-core` is a pure domain crate with ports (traits) implemented
by infrastructure crates.

**Context.** The product must be a single static binary, run three concurrent network protocols, and
allow protocols, storage and UI to be replaced independently.

**Alternatives.** A layered architecture with core depending on storage (simpler, but makes the core
untestable without a database and couples the domain to SQLite); a single crate (fast initially,
impossible to enforce boundaries in).

**Consequences.** Slightly more ceremony (trait definitions, in-memory doubles). In exchange, every
architectural invariant in ARCHITECTURE §12 becomes mechanically checkable, and the Phase 9 desktop
port becomes a packaging exercise rather than a rewrite.

---

### ADR-002 — Implement the protocol servers ourselves

**Decision.** Write the SMTP, IMAP and POP3 *servers* in-house on shared `sandesh-proto` scaffolding,
using `imap-codec` for IMAP grammar and hand-written parsers for SMTP and POP3.

**Context.** Research on 2026-09-01 found no production-quality server-side Rust crate for any of the
three: `samotop` unmaintained since 2022 on the wrong runtime; `mailin` at 3 reverse dependencies with
a sync-first design; **no IMAP or POP3 server crate exists at all**. The codec layer below, however,
is excellent.

**Alternatives.** Wrap `mailin` (only covers SMTP, design mismatch); port GreenMail or Mailpit
(foreign design plus licence entanglement); defer IMAP (it is a headline capability).

**Consequences.** This is the largest effort and largest correctness risk in the project, mitigated by
taking a mature codec for the one genuinely large grammar, by phasing, and by testing against real
mail clients. In return we control the state machine — which matters, because a *development* mail
server must accept malformed mail, never relay, and expose full protocol traces, all of which a
general-purpose MTA library resists.

**Full evaluation:** [DEPENDENCIES.md §2](DEPENDENCIES.md).

---

### ADR-003 — Raw bytes are authoritative; parsed data is a cache

**Decision.** Store the exact received octets. All parsed structure (headers, MIME parts,
attachments) is derived, versioned by `parse_version`, and regenerable. Part bodies are byte ranges
into raw, never copies.

**Context.** MIME testing, malformed-message debugging, header inspection and DKIM analysis all
require the original bytes. Reconstructing a message from a parsed tree is never byte-exact.

**Consequences.** IMAP `FETCH BODY[2.1]` is a raw slice, so it is exact by construction. A parser
upgrade triggers a background reparse with no data loss. Denormalised header columns are an
acknowledged, documented duplication for list-rendering performance. And the UI's best feature — the
MIME-tree ↔ raw-source linkage — falls directly out of this decision.

---

### ADR-004 — Target IMAP4rev2, advertise IMAP4rev1

**Decision.** Implement RFC 9051 (IMAP4rev2) and advertise both `IMAP4rev2` and `IMAP4rev1`,
negotiating with `ENABLE`.

**Context.** rev2 folds UIDPLUS, MOVE, IDLE, SPECIAL-USE, LIST-EXTENDED and ENABLE into the base
spec. Targeting it yields a coherent modern feature set instead of an arbitrary extension pile. Many
clients still speak only rev1.

**Consequences.** Some dual-behaviour handling (notably modified-UTF-7 mailbox names for rev1
clients, and `\Recent` synthesis). `\Recent` is not modelled in the domain, per its rev2 deprecation.

---

### ADR-005 — Authentication disabled by default

**Decision.** Ship with authentication off; any credentials are accepted. Enabling it is one config
flag.

**Context.** Sandesh binds to loopback and exists to remove setup friction. Requiring credentials on
first run contradicts the product's central promise.

**Risk, stated plainly.** A developer who binds Sandesh to `0.0.0.0` without enabling auth exposes an
open mail server. Mitigations: loopback-only default, a prominent dashboard indicator of the current
posture, and a startup warning when binding non-loopback without auth.

**Alternative rejected.** Generating a random password on first run — better security, but it turns
"start it and point your app at it" into "start it, find the password, configure it", which is the
friction the product exists to remove. CI and shared deployments should enable auth.

---

### ADR-006 — POP3 does not take an exclusive mailbox lock

**Decision.** Deviate from RFC 1939's exclusive-access requirement. Concurrent POP3, IMAP and UI
access to a mailbox is permitted.

**Context.** RFC 1939 §3 specifies an exclusive lock for the session's duration. A developer will
routinely have the Web UI open while testing POP3; locking them out would be hostile and confusing.

**Consequences.** The POP3 session's message-number snapshot (taken at authentication) already
provides the consistency the lock exists to guarantee: numbering is stable for the session regardless
of concurrent activity. A documented, deliberate deviation.

---

### ADR-007 — Hybrid raw storage: inline below 256 KiB, filesystem above

**Decision.** Raw messages ≤256 KiB are stored as inline SQLite BLOBs; larger ones go to a
content-addressed filesystem blob store. A `CHECK` constraint enforces exactly one location.

**Context.** Most test messages are small, where inline storage keeps delivery a single atomic
transaction with no orphan risk. Large messages are rare and are precisely the ones that degrade row
reads and bloat the database.

**Consequences.** Two code paths, and a startup sweep to reclaim orphaned blobs. Ordering
(blob fsynced before the row commits) guarantees a crash leaves a reclaimable orphan, never a
dangling reference. The threshold is an estimate to be benchmarked (OD-4).

---

### ADR-008 — Events are domain facts, not WebSocket messages

**Decision.** `sandesh-events` depends on no transport library. WebSocket, SSE, IMAP IDLE, logging and
metrics are peer subscribers. Events carry identifiers, not message bodies.

**Context.** IMAP IDLE and the UI's live inbox are driven by the same underlying facts. Designing
events around WebSocket would force IDLE into a second mechanism, and the two would drift.

**Consequences.** Consumers make a follow-up fetch for content — which bounds frame size and prevents
a 20 MB attachment from starving a slow consumer. The bus needed zero IMAP awareness to support IDLE,
which is the test the design had to pass.

---

### ADR-009 — Message and placement are separate entities

**Decision.** `Message` (immutable content) is separate from `MessageEntry` (a placement in a mailbox
carrying UID and flags). No `mailbox_id` on `messages`.

**Context.** IMAP `COPY` puts one message in two mailboxes with independent UIDs and flags. A
single-owner column cannot express this; it would force duplicating the message, breaking
store-once and making "the same message" unidentifiable across mailboxes.

**Consequences.** The most important structural decision in the schema. It makes multi-recipient
delivery, COPY, and MOVE natural, and it is why the API addresses entries as well as messages.

---

### ADR-010 — Auth and TLS move before IMAP (Phase 4.5)

**Decision.** Build SASL and TLS in `sandesh-proto` before the IMAP phase, rather than after all
protocols as the brief proposed.

**Context.** IMAP's state machine is entangled with authentication: the Not Authenticated state,
`AUTHENTICATE`, `STARTTLS`, `LOGINDISABLED`, SASL-IR. Building against stubbed auth and retrofitting
means writing the session state machine twice.

**Consequences.** SMTP gains AUTH and STARTTLS earlier as a side effect. The only change to the
brief's phase order beyond this is none — everything else is kept.

---

### ADR-011 — Core ports are async; the core still has no runtime

**Decision.** The storage ports in `sandesh-core` are `#[async_trait]` traits.
`async-trait` remains the core's only concession to asynchrony.

**Context.** A synchronous core is desirable, but every real storage backend
performs I/O and `sqlx` is async-only. Forcing synchronous ports would push
`block_on` into the storage layer, making it hostile to any async caller — and
Phase 2's SMTP server is async by necessity.

**Why this does not violate ARCHITECTURE §12.1.** `async-trait` is a procedural
macro that desugars to `Pin<Box<dyn Future>>`. It pulls in **no runtime**:
`sandesh-core`'s resolved dependency closure contains no `tokio`, `async-std` or
`smol`. This is asserted mechanically by `tests/architecture.rs`, which walks the
`cargo metadata` graph rather than trusting the manifest.

**Consequences.** One boxed allocation per port call — irrelevant beside the I/O
it wraps. The domain model itself (entities, value objects, parsing) stays fully
synchronous and runtime-free; only the ports are async.

---

### ADR-012 — Phase 1 creates only the crates it implements

**Decision.** The workspace contains `sandesh-core`, `sandesh-storage`, and a
library-less root package hosting `tests/architecture.rs`. The seven remaining
crates from ARCHITECTURE §3 are not created yet.

**Context.** Empty placeholder crates would add workspace noise, build time, and
a misleading impression of progress, without adding structure.

**Consequences.** The architecture test verifies dependency direction from real
manifests rather than directory names, so it will not silently pass over a crate
that does not exist. Each crate is added by the phase that implements it.

---

### ADR-013 — A message may be placed in a mailbox only once

**Decision.** A unique index on `message_entries(mailbox_id, message_id)`
enforces at most one placement of a given message per mailbox.

**Context.** `CORE_SPEC.md` §7.3 requires that two envelope recipients resolving
to the same mailbox produce one entry. That was specified as a routing rule; a
rule enforced only in application code is unguarded against concurrent writers.

**Consequences.** IMAP `COPY` of a message into a mailbox that already holds it
returns a `Conflict` rather than creating a duplicate. This matches the
observable behaviour of real IMAP servers and keeps UID assignment meaningful.

---

### ADR-014 — Parse diagnostics are not persisted in Phase 1

**Decision.** `ParseDiagnostic` values are produced during parsing and used at
delivery time, but are not written to the database.

**Context.** `STORAGE_DESIGN.md` §2 defines no table for them. They are fully
regenerable from the raw bytes, which remain authoritative, and nothing in Phase
1 reads them back.

**Consequences.** `MessageStore::get` returns an empty diagnostics list. The
Message Inspector (Phase 4) can either reparse on demand — cheap, and always
current with the installed parser — or a `message_diagnostics` table can be added
then. Deferred rather than guessed at.

**Alternative rejected.** Persisting them now, before any consumer exists, would
fix a schema around an unvalidated guess about what the inspector needs.

---

### ADR-015 — SMTP is ingress-only and never relays

**Decision.** The SMTP server accepts mail and delivers it locally. No outbound path exists in the
code at all.

**Context.** `CORE_SPEC.md` §7.4 states this as a rule. Phase 2 makes it structural: `DeliveryService`
has no client, no queue and no socket, so there is nothing to accidentally configure into a relay.

**Consequences.** Accidentally forwarding a developer's test mail to real recipients is the worst
failure a development mail tool can have, and the absence of an outbound code path means it cannot
happen through misconfiguration. An explicit opt-in relay remains a possible future feature; it would
be a new component, not a flag.

---

### ADR-016 — Capabilities are advertised only when implemented

**Decision.** `EHLO` lists `SIZE`, `8BITMIME`, `PIPELINING` and `ENHANCEDSTATUSCODES`. `AUTH` and
`STARTTLS` are absent until Phase 4.5 implements them.

**Context.** A client that sees `STARTTLS` will attempt it; a client that sees `AUTH` may refuse to
send without it. Advertising an unimplemented capability produces failures that look like network
problems and are extremely hard to diagnose from the client side.

**Consequences.** Enforced by construction rather than by discipline: `AuthMode::is_enabled` and
`TlsMode::advertises_starttls` return `false` in this phase regardless of configuration, and a test
asserts it. Phase 4.5 changes those two functions and the capabilities appear.

**Note on PIPELINING.** Multiple commands in one TCP segment must work whether or not the capability
is advertised, because TCP has no message boundaries. It is advertised because the line reader makes
the behaviour genuinely correct, not to claim credit for something incidental.

---

### ADR-017 — A message with no resolvable recipient is stored, not discarded

**Decision.** Under the default `catch_all` policy with no catch-all account configured, an accepted
message is stored with **no placement**, and the delivery record explains why.

**Context.** Three options: refuse at `RCPT TO` (that is the `strict` policy, which is opt-in);
accept and discard; or accept and store unplaced.

**Consequences.** Accepting with `250` and then discarding is the worst of the three — the sender
believes delivery succeeded and a developer hunting a missing message finds nothing. Storing it
unplaced means the bytes and the reason are both recoverable through the delivery record, which is
exactly what the Diagnostics view is for. The cost is that `messages` can hold rows no mailbox
references; the existing orphan sweep already handles them.

---

### ADR-018 — Session lifecycle is expressed in types, not in a status field

**Decision.** `SmtpServer::bind` consumes the server and returns a `BoundServer`; `BoundServer::serve`
consumes that. Neither double-start nor use-after-shutdown compiles.

**Context.** The brief asked for explicit lifecycle semantics with tests where practical. A runtime
`status` field would need checking at every entry point and would still permit the misuse.

**Consequences.** The invalid states are unrepresentable, so they need no tests and no runtime
checks. `ServerStatus` remains as an *observation* — bound address, live connections — which is what
the Servers view will render, rather than as a guard.

---

### ADR-019 — Recipients live on the delivery record (OD-16 resolved)

**Decision.** `Message.envelope.rcpt_to` comes back empty from `MessageStore::get`. The authoritative
recipient list is `delivery_recipients`, read through `DeliveryStore`.

**Context.** Phase 1 flagged this as OD-16. `Message` is immutable content shared across placements;
recipients are multi-valued and belong to the *transaction* that delivered it. The same message
redelivered later would have different recipients.

**Consequences.** A consumer wanting "who was this sent to?" reads the delivery, which also carries
how each recipient resolved and which placement it produced — strictly more useful than a bare list
on the message. Documented in `CORE_SPEC.md` §13 and `SMTP_DESIGN.md` §7.

---

### ADR-020 — The event bus is a `tokio::sync::broadcast`, not a bespoke fan-out

**Context.** `EVENT_MODEL.md` §5–6 requires a total order, bounded per-subscriber queues, oldest-drop
overflow, a countable lag signal, and a producer that never blocks.

**Decision.** Use `tokio::sync::broadcast` and assign `seq` inside the same critical section as the
send.

**Why.** `broadcast` provides exactly the specified semantics — per-receiver ring buffers,
oldest-drop, and a `Lagged(n)` error carrying the count — so a hand-written fan-out would be
reimplementing a well-tested primitive and getting the overflow accounting wrong. The one thing it
does not provide is the sequence number, and stamping it *outside* the send would be a real bug:
two threads could take 1 and 2 and then publish 2, 1, making the documented total order a lie. So
stamping and sending are one critical section, held over no await and no I/O.

**Consequences.** The bus depends on `tokio` (permitted for `sandesh-events` by ARCHITECTURE §3.2)
but on nothing transport-shaped. A late subscriber gets no replay, which §5 already specifies.

### ADR-021 — Two sequence numbers on the wire: `seq` and `bus_seq`

**Context.** `EVENT_MODEL.md` §3 makes `seq` the bus's global order; `API_DESIGN.md` §5 makes `seq`
a per-connection counter so a client can detect gaps after a drop. These are different numbers.

**Decision.** A WebSocket frame carries both: `seq` is the per-connection counter (the API
contract), and `bus_seq` is the bus's global order.

**Why.** A per-connection counter is what a reconnecting client actually needs — a gap in *its*
stream is what tells it to resynchronise, and the global number cannot say that because filtering
means a connection legitimately never sees most of it. But the global order is what lets two
observers, or a log and a UI, be correlated. Exposing only one would lose a real capability;
overloading one name for both would be a contradiction between two specifications.

### ADR-022 — A CORS wildcard is refused at startup rather than warned about

**Context.** `API_DESIGN.md` §2 says CORS is disabled by default with configurable origins.

**Decision.** `HttpConfig::validate` **rejects** `*` as an origin, failing startup.

**Why.** Sandesh's API exposes every message it has received, unauthenticated, on loopback. With
`Allow-Origin: *`, any page a developer happened to visit could read all of it — password reset
links, one-time codes, everything. The Phase 4 UI is served from the same origin and needs no CORS
at all, so the only legitimate use is a named development origin, which is easy to write out. There
is no configuration this setting makes possible that is worth that exposure, so it is not a warning.

### ADR-023 — The root MIME part is addressed as `root` in URLs

**Context.** `PartPath` renders the root as the empty string. `API_DESIGN.md` §3.1 exposes
`/messages/{id}/part/{path}`.

**Decision.** The URL spelling of the root part is `root`; every other part uses the dotted form.
Each part in `/mime` carries a `url_path` field giving the spelling to use.

**Why.** An empty path segment matches no route, which would leave the root part — the whole body
of any non-multipart message, the common case — unaddressable. Changing the domain's rendering to
suit URLs would push a transport concern into the core. Naming the special case in the API layer,
and having the API tell clients the spelling rather than making them know the rule, keeps the
workaround where it belongs and invisible to consumers.

### ADR-024 — A message summary's `to` is the header, not the envelope recipients

**Context.** `API_DESIGN.md` §4's example summary implies envelope recipients. `ADR-019` had
already established that recipients live on the delivery record, not the message.

**Decision.** `MessageSummary.to` reports the decoded `To:` header. Envelope recipients are
reported by `/messages/{id}/delivery`.

**Why.** These are genuinely different facts and ADR-019 settled where each lives: one message
delivered to three addresses is one set of bytes with three envelope recipients, so envelope
recipients cannot be a property of the message. Putting them on the summary anyway would have meant
either denormalising them onto the message or making the list endpoint join deliveries. What makes
`Bcc` visible is not the `to` field but `entries`, which shows every mailbox the message actually
landed in — a `Bcc` recipient appears there while appearing in no header at all.

### ADR-025 — `cargo run` from the workspace root starts Sandesh

**Context.** The root package hosts cross-crate integration tests; the binary lives in
`sandesh-app`. Bare `cargo run` at the root failed to resolve a binary.

**Decision.** The root package declares a `sandesh` binary and `default-run`; both it and
`sandesh-app`'s binary are two-line calls into `sandesh_app::run_cli`.

**Why.** `cargo run` is the command the README and the Phase 3 acceptance criteria promise, and
"actually, run `cargo run -p sandesh-app`" is a poor first experience. Keeping the entry point in
the library rather than duplicating a `main` means there is one implementation, and it stays
testable in-process — which is what lets the integration tests start a real application on
ephemeral ports instead of spawning a binary and scraping stdout.


### ADR-026 — `utoipa` generates the `OpenAPI` document from the handlers

**Context.** Phase 4 needs a machine-readable contract. The API is axum 0.8 with hand-written DTOs
that already carry doc comments and `serde` attributes.

**Decision.** `utoipa` 5, with `#[utoipa::path]` on each handler and `ToSchema` on each DTO. The
document is assembled in `sandesh-api::openapi` and served at `/api/openapi.json`. No second HTTP
framework, and no hand-maintained `openapi.yaml`.

**Why.** The alternative — writing the document by hand — guarantees drift: the two artefacts have
different authors, different review pressure, and nothing forcing them together. Deriving it from
the handlers makes the annotation and the code adjacent, so they are edited in the same diff.
`utoipa` was chosen over the alternatives because it plugs into the existing axum router rather
than replacing it, and adds no runtime behaviour.

**Known limitation.** `utoipa` cannot verify an annotation against its handler's return type: a
response documented as an object while the handler sends an array compiles cleanly. That gap was
found in practice during Phase 4 (the headers endpoint) and is now covered by a structural test in
`tests/http_surface.rs`, which compares the documented shape against a live response.

### ADR-027 — `TypeScript` is generated from `OpenAPI`, not from Rust via `ts-rs`

**Context.** `API_DESIGN` anticipated `ts-rs` for shared types. Phase 4 needed both an endpoint
contract and frontend types, and `ts-rs` provides only the latter.

**Decision.** One chain: Rust handlers → `openapi.json` → `web/src/generated/api.ts`, generated by
`openapi-typescript`. `ts-rs` is not used.

**Why.** Running both would mean two derive stacks on every DTO, two generators, and two drift
checks guarding the same types — with the possibility of the two disagreeing, which is the exact
failure the generation was meant to prevent. `OpenAPI` is a superset here: it carries the endpoint
contract `ts-rs` cannot express, and the schemas it already contains are sufficient for the types.
`#[serde(flatten)]` on `MessageDetail` also survives the `OpenAPI` path, which `ts-rs` handles less
directly.

**Consequence.** The DTOs are the single source of truth, and the frontend cannot restate a backend
shape by hand without a type error.

### ADR-028 — Generated artefacts are committed and verified in CI

**Context.** `openapi.json` and `api.ts` are build outputs. They can be committed or generated
during the build.

**Decision.** Both are committed. `cargo xtask generate-api` writes them; `--check` regenerates into
place, compares, restores, and fails on a difference. CI runs `--check`.

**Why.** For a local developer tool, a committed contract means the API is reviewable in a diff — a
breaking change is visible to a reviewer as a changed schema rather than hidden in a build step —
and a Rust-only contributor needs no Node toolchain to build and run Sandesh. The cost is that the
files can go stale, which is precisely what the CI gate removes. The guard was verified by
deliberately changing a DTO and confirming `--check` fails with an actionable message.

### ADR-029 — The built UI is embedded in the binary

**Context.** `cargo run` must produce a usable application (§87), and Sandesh must stay packageable
for Linux, macOS and Windows later.

**Decision.** `rust-embed` embeds `web/dist` into `sandesh-api` at compile time. Assets are served
from a router *fallback*; `/api/{*rest}` is claimed by an explicit 404 route above it.

**Why.** One artefact, no asset directory to deploy beside it, and no absolute path from the
building machine baked into the result. The fallback ordering is load-bearing and was wrong on the
first attempt: axum's fallback catches unmatched *nested* paths too, so an unknown `/api/...` path
returned the HTML shell with a 200. Every client would have read that as success. The explicit
`/api` route fixes it and a test in `tests/http_surface.rs` holds it fixed.

When no UI has been built, the server explains how to build one rather than serving a blank page,
so a Rust-only contributor is never confronted with a broken-looking application.

### ADR-030 — Email HTML renders in a fully-sandboxed iframe with no script

**Context.** Email HTML is attacker-controlled: anything that can reach the SMTP port can put markup
in front of a developer, and the product's job is to display it.

**Decision.** `srcdoc` into an iframe whose `sandbox` attribute is the empty string, with a
`default-src 'none'` CSP inside the frame. Remote images are blocked until explicitly requested. The
application origin sends `script-src 'self'` with no `unsafe-inline` and no `unsafe-eval`.

**Why.** An empty sandbox denies every capability the specification can deny. The two that matter
are `allow-scripts` and `allow-same-origin`; together they are equivalent to no sandbox at all, so
neither is present. A message that needs script to render is a message a developer needs to *see*
failing — and the Raw and MIME tabs already show precisely what it contains, which is the actual
job. Remote images stay blocked by default because loading one silently confirms the address was
read, which a mail-testing tool must not do on its user's behalf.

**Enforcement.** Asserted in unit tests, in `tests/http_surface.rs` against live headers, and in a
browser test that delivers a message attempting to reach `window.parent`, set the application's
title, and write to its `localStorage` — verifying all three fail.

### ADR-031 — Runtime configuration comes from the environment, read in the composition root

**Context.** Until Phase 4.5 every path and port was fixed: `AppConfig::local` hard-coded
`sandesh.db` and `sandesh-blobs` relative to the working directory, with no flag or variable to move
them. The only way to relocate the data was to run the process somewhere else, which is a blunt
instrument that moves every other relative path with it. This surfaced while isolating the browser
tests, whose configuration set variables that did not exist and therefore did nothing (OD-24).

**Decision.** Eight `SANDESH_*` environment variables cover the paths, both listener addresses, the
log filter and the two enable switches. They are read in `sandesh-app::env` and applied over
`AppConfig::local` before anything is opened. A set-but-unusable value fails startup and names the
variable; an unset or empty one leaves the default alone.

**Why the environment, and why not in `sandesh-config`.** The environment is what a container, a
systemd unit and a `.env` file already speak, so it is the smallest thing that unblocks running
Sandesh as a service. It is read in the composition root because `sandesh-config` holds the
configuration *model* and is read by every protocol adapter — that crate depends on nothing but
`thiserror`, and its own module documentation already placed layered loading in the application
crate. CLI flags and a config file can layer on top later without changing either.

**Why failing rather than ignoring.** A mistyped variable that is silently ignored costs far more
time than one that stops startup, because the symptom — configuration that "had no effect" — points
nowhere. `SANDESH_HTTP_ADDR=8080` is the common case, and it is answered with the correction rather
than a parser message.

**Consequence.** A missing data directory is now created, matching what the blob store already did.
That asymmetry existed before and was unreachable while the paths were fixed; the first person to
point `SANDESH_DATA_DIR` at a fresh directory would otherwise have met `SQLite` error 14, which
mentions neither the directory nor the cause.

### ADR-032 — CRAM-MD5 is dropped; `PLAIN` and `LOGIN` only

**Context.** The Phase 0 plan listed three SASL mechanisms. CRAM-MD5 is a challenge-response scheme:
the password never crosses the wire, which sounds strictly better than `PLAIN`.

**Decision.** Implement `PLAIN` and `LOGIN`. Do not implement CRAM-MD5. `Credential.plain` and the
`credential_plain` column are deprecated and never written.

**Why.** CRAM-MD5 avoids sending the password only by requiring the *server* to hold a recoverable
secret — it must compute the same HMAC, so it cannot store a one-way hash. That trades credentials
in flight for credentials at rest, and at rest is worse: a single database read yields every
password, where the wire is already protected by TLS (ADR-033). It also depends on HMAC-MD5, and
RFC 8314 §4.1 recommends against the whole family.

Sandesh therefore stores only Argon2id hashes. There is no recoverable secret anywhere in the
system, which is a property worth more than a mechanism almost nothing still requires.

**Consequence.** A client configured for CRAM-MD5 is refused rather than silently downgraded —
`Mechanism::parse` returns `None`, and it never appears in the advertised list.

### ADR-033 — `AUTH` is offered only on an encrypted connection

**Context.** `PLAIN` and `LOGIN` both transmit the password. Offering them on a cleartext connection
puts a credential on the wire.

**Decision.** `AuthMode::is_offered_over(encrypted)` gates advertisement: `AUTH` is absent from the
`EHLO` list and refused on an unencrypted connection, whatever the configured mode. Authentication
remains **disabled by default** (ADR-005), so this only takes effect once an operator turns it on.

**Why.** RFC 8314 §4.1. A client that sees no `AUTH` does not attempt it, which is a much better
outcome than one that attempts it and succeeds insecurely. Making this a property of the
configuration type rather than a check inside the session means every future protocol — IMAP and
POP3 both need it — inherits the same rule instead of reimplementing it.

**Consequence.** Enabling auth without TLS is inert. A configuration that silently does nothing is
worse than one that fails, so `SmtpConfig::warnings` reports it at startup and names the fix.

### ADR-034 — `rustls` with a generated, persisted self-signed certificate

**Context.** A local mail server needs TLS to make authentication safe, and nobody has a CA-signed
certificate for `localhost`.

**Decision.** `rustls` (ring provider). On first use with TLS enabled, Sandesh generates a
self-signed certificate for `localhost`, `127.0.0.1` and `::1`, and persists it beside the database
(ADR-031). The private key is written `0600` on Unix. A supplied certificate and key are used when
configured. The SHA-256 fingerprint is reported through `/api/v1/info` and shown in the UI.

**Why `rustls`.** No C toolchain, no `unsafe` in the handshake, and it is what the async ecosystem
already assumes. OpenSSL would add a system dependency to a tool whose entire premise is that
`cargo run` works.

**Why persist rather than regenerate.** A certificate that changed on every restart would retrain
the operator to click through warnings — which is precisely the habit that makes certificate
warnings useless — and would invalidate any trust they had deliberately granted. The fingerprint is
displayed because comparing it is the only way to trust a self-signed certificate on purpose.

**Loading happens before any listener binds**, so a broken certificate fails startup rather than
surfacing on the first client handshake.

### ADR-035 — Credentials are unloggable by construction

**Context.** A password passes through a command parser, a session state machine and a delivery
service on its way to a verifier. Every one of those derives `Debug`, and `tracing` renders `Debug`
into log lines. One thoughtless `#[derive(Debug)]` on a struct holding a `String` password writes
credentials to disk forever.

**Decision.** Passwords are carried in `sandesh_proto::Secret`, whose `Debug` **and** `Display` both
render `[redacted]`. Reading the value requires `Secret::expose()`.

**Why a type rather than a rule.** "Do not log passwords" is a code-review convention, and
conventions fail silently and permanently — a leaked credential in a log file is not recoverable by
fixing the code later. A type makes the mistake impossible instead of forbidden: a `Secret` nested
inside a struct that derives `Debug` still redacts, which is the realistic leak. `expose()` is named
to be conspicuous so every place a credential is handled in the clear can be found by grepping.

Redacting `Display` matters as much as `Debug`: `tracing::info!(%password)` uses `Display`, and a
type that redacts only one still leaks through the other.

### ADR-036 — `STARTTLS` restarts the session rather than mutating it

**Context.** RFC 3207 §4.2 requires a server to discard everything it learned before the handshake:
the `EHLO` domain, any authentication, any open transaction. The obvious implementation is to keep
the session object, swap its stream, and set an `encrypted = true` flag.

**Decision.** The session does not upgrade itself. On `STARTTLS` it sends the `220`, returns
`Outcome::UpgradeToTls`, and **ends**. The server performs the handshake and runs a brand-new
`Session` over the TLS stream.

**Why.** Two properties fall out of this that a flag would not give:

*Discarded state is discarded because it no longer exists.* Keeping the session and flipping a flag
means every field is still populated — the pre-TLS `EHLO` domain, a half-open transaction, an
`authenticated_as` from before the upgrade. Each would have to be reset by hand, and a field added
later would be forgotten. A new session starts at `State::Connected` with nothing carried over, so
the client must greet again exactly as the RFC says.

*Pipelined plaintext cannot be executed.* Bytes an attacker appends to the cleartext `STARTTLS`
line sit in the read buffer. Returning consumes the session, which drops that `Connection` and its
buffer; the server then builds a new `Connection` over the TLS stream. There is no code path that
could reach the buffered bytes — the guarantee is structural, not a check that could be removed.

**Verified by mutation.** A deliberately weakened server that kept the cleartext session after the
`220` was caught by five tests, including one that appends `MAIL FROM:<injected@attacker.test>` to
the `STARTTLS` line and asserts the client's own transaction still opens cleanly.

**Cost.** The session's `run` must report an outcome rather than returning `()`, and the server
grew a `serve_connection` function. That is a small price for removing a class of bug.

**Consequence.** `Session::run_to_outcome` takes a `greet` flag: the post-upgrade session must not
re-issue a `220`, because the `220` that invited the handshake was the greeting.

### ADR-037 — Implicit TLS handshakes before the SMTP session exists

**Context.** `TlsMode::Implicit` had been part of the configuration type since Phase 2, and
Phase 4.5 wired `StartTls` without wiring it. The result was worse than an unimplemented feature:
`load_tls` created an acceptor for `Implicit`, `is_enabled()` returned true, startup succeeded, the
banner reported `TLS: implicit` — and the listener served **plaintext SMTP**. An operator who chose
implicit TLS got an unencrypted server with no indication anything was wrong.

Verified rather than assumed: a probe against an `Implicit` listener returned
`220 sandesh.test Sandesh ESMTP ready` in cleartext.

**Decision.** `serve_connection` branches on the mode *before reading a byte*. For `Implicit` the
`rustls` handshake completes first, and the session is constructed over the already-encrypted
stream:

```text
Implicit:  TCP accept ──▶ TLS handshake ──▶ session (encrypted from byte 0)
StartTls:  cleartext session ──STARTTLS──▶ 220 ──▶ handshake ──▶ fresh session
```

`StartTls` is untouched (ADR-036 stands).

**Why branch in the server rather than inside the session.** The session is generic over stream
halves and cannot wrap a socket. More importantly, branching here means there is **no code path on
this mode that constructs a cleartext `Connection`** — the guarantee is structural rather than a
flag that could be set wrongly. An attempt to add a plaintext fallback after a failed handshake does
not compile: the stream has been moved into the handshake.

**A TLS mode with no acceptor now fails startup.** Previously it would have degraded to plaintext.
`ServerError::MissingTlsAcceptor` names the mode and the fix. This applies to `StartTls` too, where
the old behaviour was to advertise `STARTTLS` and then fail every handshake.

**Handshake failure closes the connection.** Nothing can be replied — the port never speaks
cleartext — so it is logged at `debug` and the socket dropped. Emphatically not a fallback, which
would reinstate the downgrade this ADR exists to remove.

**Consequences.**

* `AUTH` is offered on the first `EHLO` (ADR-033 is satisfied from byte 0, with no upgrade needed).
* `STARTTLS` is never advertised inside implicit TLS (RFC 3207 §4.2), and a `STARTTLS` command is
  refused rather than starting a nested handshake.
* Encryption still does not imply authentication: `AuthMode::Required` is enforced independently,
  and `AuthMode::Disabled` remains the default (ADR-005).

**Verified by mutation.** Reverting the branch fails six tests including the plaintext-downgrade
guard; removing the startup validation fails two; the plaintext fallback fails to compile.

## 2. Open decisions

| ID | Decision | Where | Leaning | Resolve by |
| --- | --- | --- | --- | --- |
| **OD-1** | Cross-delivery message dedup by `sha256` | CORE_SPEC §12 | No cross-delivery dedup in v1; the index exists to enable it later | Phase 1 |
| **OD-2** | CONDSTORE/QRESYNC support | CORE_SPEC §12 | CONDSTORE implemented (ADR-059); QRESYNC still deferred, needs expunge history | Resolved / Post-v1 |
| **OD-3** | Per-account quotas and the IMAP QUOTA extension | CORE_SPEC §12 | Global caps only in v1 | Phase 5 |
| **OD-4** | The 256 KiB inline/external threshold | STORAGE_DESIGN §9 | Benchmark against a real corpus | Phase 1 |
| **OD-5** | Store vs recompute `body_text` for FTS | STORAGE_DESIGN §9 | Store, accepting the duplication | Phase 1 |
| **OD-6** | Runtime-mutable listener ports | API_DESIGN §10 | Restart-only in v1 | Phase 3 |
| **OD-7** | A GraphQL endpoint | API_DESIGN §10 | No — a second contract for little gain | Closed unless demand appears |
| ~~**OD-8**~~ | ~~Durable event log for webhook retries~~ | **Resolved in Phase 8 A2** (ADR-085, ADR-086): a webhook-local outbox — a SQLite table drained by a polling worker — not a durable bus. The bus stays in-process and bounded; durability is owned by the one consumer that needs it | — | Closed |
| ~~**OD-9**~~ | ~~Project licence~~ | **Resolved in Phase 10 by ADR-099:** Apache-2.0. `LICENSE` is the canonical text; the manifests already declared it | — | Closed |
| ~~**OD-10**~~ | ~~Frontend router~~ | **Resolved in Phase 4:** React Router. Four URL-addressable routes; smaller API surface than TanStack Router and no codegen step | — | Closed |
| **OD-11** | Whether `imap-next` replaces part of `sandesh-proto`'s IMAP session layer | DEPENDENCIES §9 | Evaluate with the real state machine in hand | Phase 5 |
| **OD-12** | `imap-codec` 2.0 alpha status | DEPENDENCIES §9 | Pin exact; isolate to one module; re-confirm | Phase 5 start |
| **OD-13** | HTML compatibility linting in v1 | UI_UX_DIRECTION §14 | Phase 8 — it needs a maintained dataset. Unchanged after Phase 4 | Phase 8 |
| **OD-14** | A compose affordance in the UI | UI_UX_DIRECTION §14 | Replay/resend only, no composer. Neither built in Phase 4: replay needs a message-injection endpoint that does not exist | Phase 5 |
| **OD-15** | Whether `message_diagnostics` should be persisted or reparsed on demand | ADR-014 | Reparse on demand. Phase 4 confirms it: the inspector shows diagnostics for the message on screen and never asks for history | Phase 6 |
| ~~**OD-16**~~ | ~~`Envelope.rcpt_to` on `MessageStore::get`~~ | **Resolved in Phase 2 by ADR-019** | Recipients live on the delivery record | Closed |
| **OD-17** | Whether shutdown should drain in-flight sessions rather than letting them finish on their own | SMTP_DESIGN §13 | Let them finish; they are already bounded by timeouts. Revisit if a deployment needs a hard stop | Phase 10 |
| ~~**OD-18**~~ | ~~Whether per-connection protocol transcripts should be captured now or with the Diagnostics view~~ | **Resolved in Phase 7 by ADR-066** | Captured, but not into `deliveries.trace`: a connection is not a delivery, so transcripts live on a `connections` table and that column was dropped | Closed |
| ~~**OD-19**~~ | ~~Whether WebSocket `resume`/`after_seq` is worth a per-connection replay buffer~~ | **Resolved by ADR-100, against this leaning.** The leaning held for a *lagging* client, which is present to act on the frame it receives; it does not hold for a *disconnected* one, which receives nothing and cannot tell an idle period from a missed one. A bounded window in the bus — not a per-connection buffer, and not durability — closes that, and an unrecoverable gap is reported rather than filled with partial history | — | Closed |
| **OD-20** | Whether the API should own message injection (`POST /messages`) before IMAP exists | API_DESIGN §11.3 | Wait for Phase 5's core operations; a Phase 3 implementation would put placement logic in an HTTP handler | Phase 5 |

---


### New in Phase 4

| ID | Decision | Options | Current leaning | Resolve by |
| --- | --- | --- | --- | --- |
| **OD-21** | How the message list grows past one page | Infinite scroll on the UID cursor; numbered pages; a "load more" control | Infinite scroll with virtualisation, since the cursor pagination already exists and only one interaction should be built (§92) | Phase 5 |
| **OD-22** | Whether `GET /messages/{id}/delivery` should resolve by message id | Index the lookup; accept the recent-records scan; drop the endpoint | Index it — the Delivery tab is the answer to "why did this land here?", which is the question a developer most often has and most rarely gets | Phase 5 |
| **OD-23** | Whether the frontend needs a contract test that catches annotation-vs-handler mismatches per endpoint | The current structural check over live responses; a generated client exercised against a running server; accept the gap | Keep the structural check and extend it as endpoints are added; it already caught one real defect | Phase 5 |
| ~~**OD-24**~~ | ~~How Sandesh's paths and ports are configured at runtime~~ | **Resolved in Phase 4.5 by ADR-031:** eight `SANDESH_*` environment variables, read in the composition root. CLI flags and a file can layer on later | — | Closed |

## 3. Consistency audit (§33)

Performed against the complete documentation set.

### Can SMTP, IMAP and POP3 all operate against the same domain model?

**Yes.** The model was derived from protocol requirements, not UI needs (CORE_SPEC §1). All three
reach the same services. The three points where protocol semantics diverge are resolved in adapter
session state, catalogued explicitly in CORE_SPEC §9: IMAP sequence numbers and `\Recent`, POP3
session-scoped deletion and numbering, SMTP transaction state. The POP3 case is the sharpest test —
`DELE` must be invisible to IMAP until `QUIT` — and it is satisfied by staging deletes in the session
and calling the core once on entering UPDATE state. Verified by cross-protocol tests 7 and 8.

### Can the storage model represent IMAP mailbox/folder/flag semantics?

**Yes.** `message_entries` (ADR-009) gives per-mailbox UIDs and per-placement flags, with a unique
index on `(mailbox_id, uid)`. `uid_next` and `uid_validity` live on the mailbox, per RFC 9051 §2.3.1.1.
System flags are a bitmask; keywords are an open side table. Hierarchy is modelled by `parent_id` plus
a path name. `mod_seq` carries the mailbox-scoped CONDSTORE sequence (ADR-059). Verified by
cross-protocol test 10.

### Can we preserve and retrieve the exact original message?

**Yes.** ADR-003 makes raw bytes authoritative, stored verbatim with no line-ending normalisation. A
`CHECK` constraint guarantees exactly one storage location. MIME parts are byte ranges, so
`FETCH BODY[2.1]` is a slice of the original. `RFC822.SIZE` is computed from the stored bytes.
Verified by cross-protocol tests 1, 2 and 12, which assert byte equality including for malformed
messages.

### Can protocol implementations be replaced without rewriting the core?

**Yes.** Adapters depend only on `sandesh-core` and `sandesh-proto` (ARCHITECTURE §3.2), never on
storage or each other. They call services and map `CoreError` to protocol responses; `CoreError`
carries no protocol status codes (CORE_SPEC §10). Mechanically enforced by `cargo-deny` bans and
`tests/architecture.rs`.

### Can the React UI be replaced without rewriting the mail engine?

**Yes.** The UI's only contracts are `/api/v1` and the WebSocket, and `sandesh-api` does not depend
on the UI. Every acceptance criterion through Phase 3 is verifiable without a browser.

*(Written when this answer cited `--no-web`. That flag was struck rather than built — ADR-076 — and
the conclusion is unchanged and now needs no flag: the UI routes are the router's **fallback**, so
they answer only what the API did not claim, and replacing the UI means replacing what that fallback
serves.)*

### Can Tauri eventually wrap the same engine?

**Yes**, and the requirement is imposed now rather than later: `sandesh-app` exposes an embeddable
`Engine` type from Phase 2, with `main()` as a thin CLI wrapper, and the UI reaches its backend
through a configurable base URL (ARCHITECTURE §11). Phase 9's acceptance criteria require *zero*
engine or UI changes — making that phase the audit of every earlier phase's boundary discipline.

### Can the same engine run headlessly?

**Yes.** The engine has no external service dependencies, so native and Docker deployments are the
same binary with the same data directory (ARCHITECTURE §10).

*(Written when this answer cited `--no-web` as the CI configuration. The flag was struck — ADR-076 —
and CI needed no such thing: the Rust suite drives the engine in-process and the browser suite is a
separate job, so nothing ever asked for a UI-less binary. That is the evidence the flag had no
consumer.)*

### Can a test exercise SMTP → Core → SQLite → IMAP without the UI?

**Yes.** This is the keystone test (TESTING_STRATEGY §7), driven by third-party clients (`lettre`,
`async-imap`) so it is an interoperability test rather than a self-consistency one, and asserting byte
equality end to end. It is a Phase 5 acceptance criterion, and thirteen cross-protocol scenarios build
on the same harness.

### Cross-document consistency

Checked and consistent: default ports (README ↔ PROTOCOL_CAPABILITIES ↔ ROADMAP); crate names and
dependency direction (ARCHITECTURE ↔ DEPENDENCIES ↔ ROADMAP); the entry/message split (CORE_SPEC ↔
STORAGE_DESIGN ↔ API_DESIGN ↔ EVENT_MODEL); event names (EVENT_MODEL ↔ API_DESIGN ↔ TESTING_STRATEGY);
phase numbering after the Phase 4.5 insertion (ROADMAP ↔ PROTOCOL_CAPABILITIES ↔ TESTING_STRATEGY);
search syntax shared by REST and IMAP through one `SearchQuery` type (STORAGE_DESIGN ↔ API_DESIGN ↔
UI_UX_DIRECTION).

**Two inconsistencies were found and fixed during the audit:**

1. **Event naming.** The brief proposed `MessageCreated`/`MessageUpdated`/`MessageMoved`. In this
   domain a `Message` is immutable and cannot be updated or moved — only a placement can. Renamed to
   `EntryCreated`/`EntryFlagsChanged`/`EntryMoved`, with `MessageReceived` and `MessageDeleted`
   retained as genuine message-level facts. Recorded in EVENT_MODEL §4.1.

2. **Phase numbering.** Moving auth and TLS earlier (ADR-010) would have renumbered every later phase
   relative to the brief. Introduced as **Phase 4.5** instead, so Phase 5 remains IMAP and Phase 6
   remains POP3 as the brief proposed, and all cross-document phase references stay valid.

### ADR-038 — The IMAP session state is a type, not a set of flags

**Context.** RFC 9051 §3 defines four connection states, and almost every command is legal in only
some of them. The obvious implementation is an `authenticated: bool` beside a
`selected: Option<Mailbox>`, tested at the top of each command handler.

**Decision.** `sandesh_imap::State` is an enum — `NotAuthenticated`, `Authenticated`,
`Selected { read_only }`, `Logout` — and one exhaustively-matched `transition(state, command)`
function is the sole authority on whether a command may run. Handlers do not re-check.

**Why.** Two booleans admit the state "selected but not authenticated", which is nonsense that a bug
can nevertheless reach; an enum cannot represent it. And scattering the checks means the *absence* of
a check is invisible — the failure mode is a mailbox command that silently works before
authentication. Because `transition` matches `CommandKind` exhaustively, adding a command without
deciding its state rules is a compile error rather than an accidental permit.

`Selected` carries `read_only` rather than a separate flag so that `EXAMINE`'s promise (RFC 9051
§6.3.3) is enforced in the same place: `STORE`, `COPY`, `MOVE` and `EXPUNGE` are refused on a
read-only selection by the state machine, not by each handler remembering to ask.

**Consequence.** A failed `SELECT` must return the session to `Authenticated` (RFC 9051 §6.3.2) —
it does *not* keep the previous mailbox. `Transition::Deselect` exists to make that visible rather
than implicit, because keeping a stale selection after a failed switch is the classic version of
this bug.

### ADR-039 — `IDLE` is deferred, and therefore unrepresentable

**Context.** RFC 2177 `IDLE` lets a client wait for mailbox changes instead of polling. It is
valuable and widely used, but it interacts with the event bus, connection timeouts and the command
loop simultaneously, and a half-working `IDLE` is worse than none: clients that see the capability
stop polling and then miss updates entirely.

**Decision.** Phase 5 does not implement `IDLE`. It is absent from `CommandKind`, so it cannot be
parsed, and absent from the capability list, so it is never advertised. Clients fall back to polling
with `NOOP`, which RFC 9051 supports and which every client implements.

**Why unrepresentable rather than merely unimplemented.** A `CommandKind::Idle` variant that returned
`BAD` would be one careless capability-list edit away from being advertised. Leaving the variant out
means the truthfulness of `CAPABILITY` is enforced by the compiler: there is no value to advertise.
A parser test asserts `IDLE` is rejected, so the deferral is verified rather than assumed.

**Revisit when** the event-driven unsolicited-response path is in place and load-tested; `IDLE`
becomes a thin layer over it rather than a new subsystem.

### ADR-040 — The IMAP parser pauses at literals instead of owning the socket

**Context.** An IMAP command is not a line. A command carrying a literal spans several lines and
requires the server to emit a continuation request (`+`) and then read an exact octet count before
parsing can finish. The straightforward implementation hands the parser the connection and lets it
read as needed.

**Decision.** `parse_line` is a pure function returning either `Parsed::Complete` or
`Parsed::NeedLiteral(PendingLiteral)`. The session — which owns the socket — sends the continuation,
reads exactly `PendingLiteral::length()` bounded octets, and calls `resume()`. Multiple literals in
one command work by iteration.

**Why.** A parser that reads from a socket can only be tested with a socket, and the inputs that
matter most here are the malformed ones: a literal that declares 4 GiB, twenty digits of length, a
`}` with trailing junk, a literal inside an unclosed list. Keeping I/O out makes all of those
synchronous unit tests, and the phase's parser suite covers them.

**Bounds precede allocation.** The declared length is checked against the configured limit *before*
anything is allocated, so `{4294967295}` costs a rejection rather than four gigabytes. A length that
cannot fit a `usize` is reported as malformed rather than wrapping to a small permitted value.
Nesting depth (`MAX_NESTING_DEPTH`) and token count (`MAX_TOKENS`) are bounded for the same reason:
`((((…))))` must not recurse until the stack is gone. Mutation tests confirm that removing either
bound makes the suite fail.

**Literals stay `Vec<u8>`.** RFC 9051 §4.3 makes a literal explicitly binary — it carries message
octets. Decoding it as UTF-8 would corrupt the bytes the product guarantees to preserve, so only
atoms and quoted strings, which the grammar constrains to text, become `String`.

### ADR-041 — IMAP offers two listeners, not one TLS mode

**Context.** SMTP selects one TLS posture per process: `SmtpConfig::tls_mode` is `Disabled`,
`StartTls` or `Implicit`, and the listener behaves accordingly (ADR-037). Reusing that shape for IMAP
was the obvious move.

**Decision.** `ImapConfig` instead carries two optional addresses — `bind` for cleartext (with a
`start_tls` flag) and `implicit_tls_bind` for IMAP-over-TLS — and `ImapServer` accepts on both.

**Why.** IMAP deployments conventionally serve 143 and 993 *simultaneously*, because mail clients
differ in which they will use; a client configured for IMAPS cannot be talked into `STARTTLS` and
vice versa. A single `TlsMode` forces a choice that the protocol's own convention does not, and
working around it would mean running two `ImapServer`s and reconciling their shutdown and connection
caps by hand.

Both addresses are `Option`, so a TLS-only deployment can have *no* cleartext listener at all rather
than one it must remember not to use. Both default to loopback, and `implicit_tls_bind` defaults to
`None`: enabling it would generate a certificate and open a second port that a developer running
`cargo run` did not ask for.

`start_tls` is a `bool` rather than a reused `TlsMode` deliberately. `TlsMode::Implicit` on the
cleartext listener is a contradiction, and a type that cannot express it is better than a validation
rule that rejects it.

**Consequence.** One `BoundServer` owns two `Listener`s and two accept loops joined under one
cancellation token, so a single shutdown signal stops both and neither can outlive the application.

### ADR-042 — Dual `IMAP4rev2` and `IMAP4rev1` capability advertising for universal client compatibility

**Context.** Real-client testing during Phase 5 found that Python's standard `imaplib` (and applications
built on it, including Odoo `fetchmail`) refuses to talk to the server if `IMAP4rev1` is absent, failing
with "server not IMAP4 compliant". `imaplib` hardcodes `AllowedVersions = ('IMAP4REV1', 'IMAP4')` and does
not recognise `IMAP4rev2`.

RFC 9051 §6.1.1 explicitly permits advertising `IMAP4rev1` alongside `IMAP4rev2`:
> "A server that supports both IMAP4rev1 and IMAP4rev2 might advertise both IMAP4rev1 and IMAP4rev2 in
> its CAPABILITY response."

**Decision.** Advertise both `IMAP4rev1` and `IMAP4rev2`. Provide essential RFC 3501 and RFC 9051 mailbox
operations (`SELECT`, `EXAMINE`, `SEARCH`, `FETCH`, `STORE`, `CLOSE`, `UNSELECT`), and in developer mode
(`AuthMode::Disabled`), permit cleartext `LOGIN` without advertising `LOGINDISABLED`.

**Why.** Sandesh is an inspection and local development mail server, designed to work drop-in alongside
tools like GreenMail and Mailpit. Requiring clients to either support bleeding-edge RFC 9051 or hack their
standard library would lock out standard ecosystems (Python `imaplib`, Odoo, PHP `imap`, Ruby `net/imap`,
legacy Thunderbird profiles). By implementing clean baseline mailbox operations and advertising both
`IMAP4rev1` and `IMAP4rev2`, standard clients can connect, authenticate, inspect, and fetch mail seamlessly,
while modern RFC 9051 clients can negotiate rev2 capabilities.

**Consequence.** `real_client::imap4rev1_and_imap4rev2_are_advertised` and cross-protocol tests assert that
both capabilities are present. Clients like Odoo fetchmail can test connection, authenticate, poll `(UNSEEN)`
messages, fetch exact RFC822 bytes, and mark messages `\Seen`.

### ADR-043 — Counted reads live in `sandesh-proto`, not in the IMAP crate

**Context.** IMAP literals are counted octets, not lines (RFC 9051 §4.3). `sandesh-proto` framed only
CRLF-delimited lines, because SMTP needs nothing else. The IMAP session needed to read exactly *N*
bytes, which could have been done by taking the raw stream back with `Connection::into_parts` and
reading it directly in `sandesh-imap`.

**Decision.** `LineReader::read_exact` and `Connection::read_exact_owned` were added to
`sandesh-proto` instead, taking the length *and* a maximum.

**Why.** The buffer is the reason. A counted read must consume bytes the line reader has already
buffered — a client may pipeline the literal payload into the same TCP segment as the command — so
reading the socket directly would silently skip them and desynchronise the stream. That is a framing
concern, and framing is exactly what this crate owns.

It also stays protocol-agnostic: nothing in the signature names IMAP, and POP3 will want the same
primitive for `RETR`. The alternative — a second buffer in `sandesh-imap` — is how two layers come to
disagree about which bytes have been consumed.

**The limit is a parameter, not a trust.** `read_exact` rejects `length > max_payload` *before*
allocating, and allocation grows as bytes arrive rather than reserving the declared length up front.
So a client that declares a permitted-but-large literal and then stalls costs nothing while it does.
The IMAP parser already applies its own configured limit (ADR-040); this is the transport-level
backstop, and taking the limit as an argument is what makes it a backstop rather than a duplicate.

---

### ADR-044 — The sequence-number ↔ UID mapping is one shared type

**Context.** RFC 9051 §2.3.1.2 defines a message sequence number as a message's *position* in the
mailbox, counting from 1, in ascending UID order. Every message command — `FETCH`, `SEARCH`,
`STORE`, `COPY`, `MOVE`, `EXPUNGE` — resolves client-supplied identifiers through that mapping, in
either sequence or UID mode.

The obvious implementation is for each handler to index the entry vector it already holds. That is
how the placeholder `SEARCH`/`FETCH`/`STORE` handlers removed by increment 3 worked, and it is
wrong in a way that does not announce itself: each handler acquires its own opinion about ordering,
about whether numbering starts at 0 or 1, and about what a UID that is not present should do.

**Decision.** One type, `sandesh_imap::SequenceMap`, owns the mapping. It sorts by UID on
construction, exposes `by_sequence` (rejecting 0), `by_uid`, `sequence_of` and `enumerated`, and is
built once per `SELECT` and stored in the session's `SelectedMailbox`. Increment 4's message
commands resolve identifiers through it and are forbidden from re-deriving the ordering.

**Why.** The invariant is *cross-command*: a `FETCH 2` and a `STORE 2` in the same session must name
the same message, and a `SEARCH` result in sequence mode must be usable as a `FETCH` argument. An
invariant that spans commands cannot be maintained by each command separately. Concentrating it also
makes it testable without a socket — the ordering, the 1-basing, the UID-gap behaviour and the
"new arrival extends the mapping" case are unit tests, not protocol choreography.

**Consequence.** `SequenceMap` sorts even though `MessageStore::list_entries` already returns
`ORDER BY uid`. That is deliberate redundancy: the type *is* the definition of the ordering, and it
must not silently depend on one adapter's SQL to be correct.

---

### ADR-045 — `RECENT` is not reported, and asking for it is an error

**Context.** RFC 3501 defined `\Recent` as "arrived since the last session opened this mailbox" and
required `SELECT` to report a `RECENT` count and `STATUS` to accept a `RECENT` item. RFC 9051
**removed** it: it is unimplementable in a way that is both correct and useful when several clients
share a mailbox, and rev2 clients do not rely on it.

Sandesh keeps no `\Recent` state. Its flag model (`SystemFlags`) has no such flag, and its storage
schema has no column that could carry one.

**Decision.** `SELECT`/`EXAMINE` emit no `* n RECENT` line, and `STATUS` rejects a `RECENT` item with
`BAD` rather than answering it.

**Why not report `RECENT 0`?** Because it would be a fabricated value. `0` is a meaningful answer —
it asserts that nothing has arrived since the last session — and this server has no basis for that
assertion. The project's rule is that a capability or a value is either real or absent; a constant
dressed as a measurement is the failure mode the truthful-capabilities discipline exists to prevent.

**Consequence.** A strictly rev1 client that requires a `RECENT` line in the `SELECT` response will
be unhappy. That is the same trade recorded in ADR-042: this is a rev2 server that advertises the
rev1 *name* for client compatibility, not a server that implements RFC 3501 semantics. Increment 3
adds no `RECENT` to either command, and increment 4 must not add one either.

---

### ADR-046 — A selection is a snapshot, and `FETCH` reads it

**Context.** Increment 3 built the selected mailbox as a `SequenceMap` captured at `SELECT` and
never refreshed. Increment 4A's brief (§20) required that behaviour be decided and documented
before `FETCH` was built on top of it, rather than left to emerge.

The question is what happens here:

```text
Session A: SELECT INBOX          ← snapshot taken: 1 message
Session B: delivers a message
Session A: FETCH 1:*             ← 1 message, or 2?
```

**Decision.** The snapshot is retained. A selection fixes the set of messages and their sequence
numbers at `SELECT` time; a message arriving afterwards is invisible to that session until it
re-issues `SELECT`/`EXAMINE`. `FETCH` resolves every identifier against that fixed map.

Two consequences are made explicit, and both are tested:

* **New arrivals are not visible.** `FETCH 1:*` returns the snapshot's messages, not the mailbox's
  current contents.
* **Concurrently removed placements degrade cleanly.** Metadata items (`UID`, `FLAGS`,
  `INTERNALDATE`) are answered from the snapshot and still succeed. Content items load the message
  through the port; if it is gone, that message is *omitted* from the response and the command
  still completes `OK`. A `FETCH` never fails because someone else changed the mailbox.

The one thing that *is* written back into the snapshot is `\Seen`, when a non-peek body fetch sets
it (`SequenceMap::set_flags`). Flags only — never membership, never ordering.

**Why not refresh on every command?** Because RFC 9051 §5.2 makes the *server* responsible for
telling the client when the mailbox changes, via untagged `EXISTS`/`EXPUNGE` responses. Silently
renumbering messages under a client that has not been told is worse than a stale view: the client's
next `FETCH 2` would name a different message than the one it just listed. Refreshing correctly
means emitting those untagged updates, which is unsolicited-update work explicitly out of scope for
this increment (brief §31). A stale-but-consistent view is a defensible IMAP posture; an
inconsistent fresh one is not.

**Why this is safe to defer.** No client loses data. A client that wants current contents issues
`NOOP` then re-`SELECT`s, or simply re-selects — both are ordinary IMAP. What a client cannot yet
do is *rely on being told* about arrivals without asking, which is exactly what `IDLE` (ADR-039,
deferred) and unsolicited updates are for.

**Consequence.** Increment 4B (`STORE`/`EXPUNGE`) will mutate the mailbox and must decide how the
snapshot is updated for the *acting* session, which is a narrower question than general
concurrency. Whenever unsolicited updates arrive, this ADR is the one to revisit, and the tests in
`crates/sandesh-imap/tests/fetch.rs` under "Snapshot semantics" are the ones that will need to
change deliberately.

---

### ADR-047 — The acting session's snapshot is refreshed on flag mutation

**Context.** ADR-046 fixed a selection as a snapshot taken at `SELECT`, and
increment 4A's `FETCH` answered `FLAGS` from it. That was sound while `FETCH` was the only
command, because the one flag it changes — `\Seen` on a non-peek body fetch — already wrote back
into the snapshot.

`STORE` makes the gap unavoidable. Without a decision here:

```text
STORE 1 +FLAGS (\Seen)   → OK, and storage really has \Seen
FETCH 1 FLAGS            → () — the snapshot still holds the SELECT-time flags
```

A session contradicting *its own* successful command is indefensible. It is also the exact failure
that breaks a fetching agent: it marks a message handled, re-reads it, sees it unhandled, and
processes it twice.

**Decision.** A successful flag mutation updates the acting session's snapshot for the affected
placement, with the flags **storage returned** — never a value this adapter computed. The mechanism
is `SequenceMap::set_flags`, reached through `SelectedMailbox::record_flags`, and both writers use
it: `FETCH`'s implicit `\Seen`, and every `STORE`.

Three boundaries are deliberate:

* **Flags only.** Membership and sequence numbering stay fixed at `SELECT` (ADR-046). Renumbering
  mid-session would make a client's next `FETCH 2` name a different message than the one it just
  listed.
* **The acting session only.** Another session's snapshot is untouched, and no unsolicited
  `FETCH`/`EXISTS` is sent. Cross-session staleness remains exactly as ADR-046 describes it, and
  `another_sessions_store_is_not_visible_until_reselect` pins that.
* **`.SILENT` updates it too.** Silence suppresses the *report*, not the bookkeeping
  (RFC 9051 §6.4.6). A `.SILENT` store that left the snapshot stale would be the same bug wearing a
  disguise, so the update happens before the silence check.

**Why this and not a refresh-on-every-command.** Re-reading the mailbox per command would also fix
the symptom, but it would silently change which messages a sequence number names — the thing
ADR-046 exists to prevent — and doing it *correctly* means emitting untagged `EXISTS`/`EXPUNGE`,
which is unsolicited-update work out of scope here. Writing back the one value we just changed is
the smallest change that makes a session self-consistent without touching the ordering guarantee.

**Consequence.** Self-consistency is now a per-session property, and the boundary is precise: a
session sees its own mutations immediately and other sessions' mutations only after re-selecting.
When unsolicited updates arrive, this ADR and ADR-046 are the pair to revisit; `EXPUNGE`
(increment 4B) is the next command that will press on the membership half, which this ADR
deliberately does not address.

---

### ADR-048 — `SearchQuery` lives in the domain, and matching is a pure function

**Context.** `STORAGE_DESIGN.md` §7 has always specified that IMAP `SEARCH` and the REST/UI filter
syntax compile into one `SearchQuery` type, "so results cannot diverge between UI and IMAP". It also
anticipates an FTS5 index behind a `SearchIndex` trait for content keys.

Increment 4B.2's audit found that **none of it existed**: no `SearchQuery`, no `SearchIndex`, no FTS
table in the schema. The design document described a plan, and `SEARCH` was the first surface to
need it.

**Decision.** Build the plan, not the index.

* `sandesh_core::SearchQuery` is the shared plan — a criteria tree with `And`/`Or`/`Not` and
  self-contained leaves. It lives in the domain, so a second surface compiles into the same type.
* Matching is `SearchQuery::matches(Option<&Message>, &MessageEntry)`: pure, total, no I/O.
* The **caller** supplies the candidate set. For IMAP that is the session's `SequenceMap`, which is
  what makes mailbox scoping structural rather than a filter that could be omitted.
* `sandesh_imap::search` compiles RFC 9051 keys into it and nothing else.

No FTS5 index, and no `SearchIndex` trait yet.

**Why matching is a function rather than a query.** A query type that knew how to find its own
candidates would need storage access, which would put either SQL in the domain or search semantics
in the adapter. Separating "what to match" from "what to consider" keeps both out, and it is why a
mailbox-scope bug is not expressible here: the adapter cannot hand the matcher a placement from
another mailbox, because it only has its own snapshot.

**Why not FTS5 now.** The index is a *scaling* optimisation, and adding it means a schema migration
plus `body_text` maintenance on every insert — a change to the delivery path, in an increment whose
brief was `SEARCH`. Content keys instead evaluate against the already-parsed message, and
`SearchQuery::needs_message` means flag-only searches load no bodies at all. `SEARCH UNSEEN`, the
query that actually matters for fetching agents, is therefore independent of mailbox size.

**Consequence.** `BODY`/`TEXT` cost is linear in the total size of candidate messages. That is
acceptable at developer-tool scale and is recorded as a known limitation rather than hidden. When
the index arrives, `SearchQuery` is the plan it executes — the type does not change, only who
answers it. A REST search surface can compile into the same type without touching IMAP.

**Also decided here.** `NEW`, `OLD` and `RECENT` are refused, not answered: this server keeps no
`\Recent` state (ADR-045), so any answer would be fabricated. An unsupported `CHARSET` is
`NO [BADCHARSET (US-ASCII UTF-8)]` per RFC 9051 §6.4.4 — a refusal the client can retry — rather
than `BAD`, and rather than being searched as if it were UTF-8.

---

### ADR-049 — `EXPUNGE` mutates the acting session's membership; other sessions keep their snapshot

**Context.** ADR-046 made a selection a snapshot taken at `SELECT`, and ADR-047 relaxed that for
flag *values* while explicitly keeping **membership and sequence numbering** fixed — renumbering
mid-session would make a client's next `FETCH 2` name a different message than the one it just
listed.

`EXPUNGE` cannot honour that boundary, and this is the one command where the boundary is wrong.
RFC 9051 §6.4.3 has the server report each removal as `* <seq> EXPUNGE`, and §7.4.1 defines that
line as *renumbering the mailbox*: everything above the reported number moves down by one. The
client is therefore told to renumber. If the server does not renumber with it, the two disagree
immediately:

```text
SELECT INBOX             → * 2 EXISTS            (UIDs 7, 8)
STORE 1 +FLAGS (\Deleted)
EXPUNGE                  → * 1 EXPUNGE, OK       client now holds one message at seq 1
FETCH 1 (UID)            → UID 7                 ← the message that was just expunged
```

Answering `UID 7` there is not staleness a client can work around: the server has reported a
removal and then served the removed message under the number the client was told to reuse. A
fetching agent reprocesses it forever.

**Decision.** A successful `EXPUNGE`/`UID EXPUNGE` **removes the expunged placements from the
acting session's `SequenceMap` and renumbers the survivors contiguously**, before the tagged `OK`
is written. UIDs are never rewritten. The mechanism is `SequenceMap::remove_uids`, reached through
`SelectedMailbox::remove_expunged` — the sole membership-mutating path, deliberately separate from
`set_flags`, which still refuses to touch membership.

Four boundaries are deliberate:

* **Only what storage actually removed.** The plan is filtered through
  `ExpungePlan::retain_removed` against the `EntryId`s `delete_entry` reported as deleted, so a
  placement another session expunged first produces no `EXPUNGE` line and no snapshot change. A
  failed operation emits no event and tells the client nothing happened.
* **Sequence numbers are captured before any removal.** `ExpungePlan` is computed in full against
  the unmutated snapshot, so the reported numbers are positions as they stood when the command
  arrived. Reports are then emitted **highest first**, because each `* <seq> EXPUNGE` decrements
  what follows it: ascending order would have the client remove the wrong messages, a bug invisible
  unless one command removes two.
* **The acting session only.** Another session's snapshot is untouched and receives no unsolicited
  `EXISTS`/`EXPUNGE`. Cross-session staleness stays exactly as ADR-046 describes it, and
  `another_session_keeps_its_snapshot_until_it_reselects` pins it. This increment implements no
  `IDLE` and no unsolicited synchronisation.
* **`UID EXPUNGE` intersects, it does not substitute.** RFC 4315 §2.1: the set narrows which
  `\Deleted` messages go. A UID in the set without `\Deleted` survives, which is what lets a client
  expunge a set it captured earlier without racing its own flag state.

**Why not refresh the whole snapshot from storage.** Re-listing the mailbox would also produce a
correct count, but it would additionally absorb *other* sessions' changes — new deliveries
included — silently changing which message every sequence number names, with no untagged report to
the client. That is the unsolicited-update problem ADR-046 defers, and it is strictly larger than
what `EXPUNGE` requires. Removing exactly the placements this command removed is the smallest
change that keeps the session consistent with the reports it just sent.

**Also decided here.** `EXPUNGE` takes no argument and removes what is *marked*, never what a
client names. The whole command — including a `UID EXPUNGE` set — is parsed and validated before
the first `delete_entry`, so a malformed command removes nothing. Orphan collection runs after the
removals via `collect_orphans` (emitting `MessageDeleted`); a failure there is logged, not reported,
because the expunge succeeded and an uncollected orphan is reclaimable later. Removal targets
`EntryId` — one *placement* — so another mailbox's placement of the same message is untouched by
construction, and the message itself survives until no placement references it.

---

### ADR-050 — `COPY` reads the source and writes the destination; a same-mailbox copy is refused

**Context.** `COPY` is the first message command whose write lands somewhere other than the
selected mailbox, and that breaks two assumptions earlier increments had baked in.

*First, read-only selections.* The state machine grouped `COPY` with `STORE`, `MOVE` and `EXPUNGE`
as "mutating", so an `EXAMINE`d selection refused it. But `COPY` does not touch the selected
mailbox at all: RFC 9051 §6.4.7 permits it on a read-only selection precisely because the new
placement is created elsewhere. Refusing it would stop an archiving client from filing mail it
opened read-only — a real workflow, refused on a technicality.

*Second, the destination's UIDs.* `copy_entry` allocates consecutively today, so it is tempting to
render `[COPYUID]`'s destination set as `uid_next .. uid_next + n`. That is a guess about a storage
detail. Two clients copying into one mailbox interleave allocations, and the guess then names UIDs
belonging to someone else's messages — while looking perfectly well formed.

**Decision.**

* **`COPY` is gated as a reader, not a writer.** `transition` moves it beside `FETCH` and `SEARCH`,
  so it is legal on an `EXAMINE`d selection. `MOVE` stays with the writers — it removes from the
  source. The *destination's* writability is checked in the handler, because only the handler knows
  which mailbox that is: a nonexistent destination is `NO [TRYCREATE]` (RFC 9051 §6.4.7, never an
  implicit create), and a `\Noselect` name is refused.
* **`[COPYUID]` reports the UIDs storage returned.** Each `(source, destination)` pair is recorded
  from the `MessageEntry` `copy_entry` handed back, and `CopyResult::copyuid` renders the two sets
  **positionally** — the *n*th UID of each names the same message (RFC 4315 §3). Neither set is
  sorted independently, because sorting one alone silently re-pairs the messages. A copy that
  created nothing carries no `[COPYUID]` at all, since an empty set would be malformed.
* **The source snapshot is untouched.** `COPY` creates placements and removes nothing, so it calls
  neither `record_flags` nor `remove_expunged`. Source UIDs, flags, membership and sequence
  numbering are all unchanged, and the underlying `Message` is shared — `copy_entry` reuses
  `message_id`, which is what keeps the content-addressed blob single-copy.
* **A same-mailbox `COPY` is refused with `NO`.** This is the snapshot decision the increment
  brief anticipated, and it resolves in an unexpected direction: the domain model places a message
  in a given mailbox **at most once** (`CORE_SPEC.md` §7.3 — two envelope recipients resolving to
  one mailbox produce one entry), enforced by a unique index on `(mailbox_id, message_id)`. The
  second placement RFC 9051 §6.4.7 would allow therefore *cannot exist* in this data model. Given
  that, reporting `OK` while creating nothing is the one unacceptable answer: it tells the client a
  copy happened that did not. Refusing is honest, and it needs no snapshot change at all — which is
  the smallest consistent change available.

**Why not relax the placement invariant.** Allowing duplicate placements would make same-mailbox
`COPY` work, but it would also change delivery: the idempotent placement in `CORE_SPEC.md` §7.3 is
what stops a message addressed to two aliases of one mailbox from arriving twice. That is a domain
decision with consequences well beyond `COPY`, and it is not this increment's to make.

**Also decided here.** The set, the mailbox name, the destination's existence and its selectability
are all validated before the first `copy_entry`, so a `COPY` that is going to fail creates no
placement (RFC 9051 §6.4.7). Out-of-range members are not errors (§6.4.8): an empty result is a
successful `COPY` that copied nothing. A placement removed since the snapshot was taken is skipped
rather than failing the command, matching how `FETCH` and `STORE` degrade (ADR-046), and it
contributes no `[COPYUID]` pair — so the reported sets never name a placement that was not created.
Destination membership follows the existing snapshot policy: another session with the destination
selected sees the copy only when it re-selects, and no unsolicited update is sent.

### ADR-051 — A same-mailbox `MOVE` is refused, for the same reason a same-mailbox `COPY` is

**Context.** `MOVE` into the currently selected mailbox is the case RFC 6851 leaves a server free to
handle as it sees fit, and it interacts badly with the placement invariant ADR-050 already ran into.

The storage port makes its own choice here: `MessageStore::move_entry` sees `source.mailbox_id ==
target_mailbox` and returns the source placement unchanged. That is the right thing for *storage* to
do — the requested end state is the current state, so there is nothing to write — but it is the
wrong thing to report as a move. The returned entry carries the *source* UID, so `[COPYUID]` would
pair a message with itself, and the handler would then send `* n EXPUNGE` for a message that is
still in the mailbox. The client would remove it from its view while the server kept it.

**Decision.** A `MOVE` whose destination is the selected mailbox is refused with `NO`, in the
handler, before anything is attempted. The domain places a message in a mailbox **at most once**
(`CORE_SPEC.md` §7.3, a unique index on `(mailbox_id, message_id)`), so the destination placement
such a move would create is the source placement it would remove — there is nowhere to move the
message *to*. RFC 6851 §3.3 does not require a server to support moving a message onto itself, and a
`NO` a client can act on beats an `OK` that silently did nothing.

This mirrors ADR-050 exactly, which is the point: one invariant, one consequence, stated the same
way in both commands rather than each handler inventing its own answer.

**Why not let `move_entry`'s no-op through.** Because the protocol response would then be a lie in
two directions at once — a `[COPYUID]` naming a destination UID equal to the source UID, and an
`EXPUNGE` for a message that never left. Either alone corrupts the client's mapping; together they
would leave the client believing the message had moved to a UID that is actually its old one.

**Verified by mutation.** Removing the refusal makes `move_into_the_selected_mailbox_is_refused`
fail: the message is asserted still present in `INBOX`, and the response asserted to carry no
`EXPUNGE` line.

### ADR-052 — A partially completed `MOVE` reports `NO`, and still reports what moved

**Context.** `MessageStore::move_entry` moves **one** placement per transaction. A multi-message
`MOVE` is therefore a sequence of commits, not a single one, and it can stop half way — a placement
another session expunged between this session's `SELECT` and this command, or a storage error on the
third of five messages.

Two things are then true at once, and they pull in opposite directions. The moves that succeeded are
**already committed**: those messages really are in the destination and really are gone from the
source, and a client that is not told will hold a stale view of both mailboxes. But the command did
not do what was asked, and `OK` asserts that it did.

**Decision.** Both facts are reported, separately.

* The untagged responses describe **what actually happened**: `* <seq> EXPUNGE` for each placement
  that moved, and a `[COPYUID]` naming only the pairs that were created. A placement that did not
  move appears in neither — `MovePlan::record` is called only after `move_entry` returns, so a
  failure is recorded nowhere and cannot be reported.
* The **tagged response is `NO`** when `MovePlan::is_partial()` — when fewer placements moved than
  the set named — with a text saying how many of how many moved. `OK` is reserved for a `MOVE` that
  moved everything it named.

A set that named *no* message is not partial: nothing was asked, so nothing was missed, and RFC 9051
§6.4.8 makes that a successful command that moved nothing.

**The limitation, stated plainly.** This increment does **not** make a multi-message `MOVE` atomic.
Making it so would require a port that moves a set of placements in one transaction —
`move_entries(&[EntryId], MailboxId)` — which is a storage-layer change with its own rollback and
event-ordering consequences, and is not this increment's to make. What is guaranteed today is:
each *individual* message moves atomically or not at all (storage's transaction), no message is
reported as moved unless it was, and a partial result is never reported as success. A client that
receives `NO` from a `MOVE` must re-examine both mailboxes rather than assume nothing happened —
which is exactly what the untagged responses tell it.

**Why not roll back the successful moves.** Because there is no transaction spanning them to roll
back, and "undoing" them by moving the messages back would be a second multi-message operation with
the identical failure mode — one that could now fail with the messages split across two mailboxes
and no record of where they started.

**Verified by mutation.** Making `is_partial` return `false` unconditionally fails
`a_move_that_lost_one_placement_is_partial`. Reporting `planned` rather than `moved` in
`report_order` or `moved_uids` fails `only_moved_placements_are_reported_as_expunged`.

### ADR-053 — `IDLE` consumes the existing event bus through a core port, not a second mechanism

**Context.** Unsolicited mailbox updates need a live stream of what changed. The project already has
one: `sandesh-events::EventBus`, which the Web UI and the API consume. But `tests/architecture.rs`
forbids `sandesh-imap` from depending on any `sandesh-` crate except `core`, `proto` and `config` —
the rule that keeps the protocol adapters from reaching past the ports into implementations.

Three options: relax the architecture rule, build a second notification path inside the IMAP
adapter, or express subscription as a port.

**Decision.** Subscription became a port. `sandesh-core` gains `EventSource` (hands out
subscriptions) and `EventStream` (yields the next event), the exact mirror of the existing
`EventSink`. `sandesh-events::EventBus` implements `EventSource`; `Subscriber` implements
`EventStream`. The IMAP session holds an `Arc<dyn EventSource>` and knows nothing about the bus.

* **The architecture rule is unchanged and still passes.** `sandesh-imap`'s *runtime* closure gains
  nothing; the bus reaches it only as a `dyn` trait object the composition root injects.
  `sandesh-events` is a **dev**-dependency of `sandesh-imap`, for tests that need a real bus to
  publish through.
* **There is no second event system.** The bus already provides what `IDLE` needs and what a
  hand-rolled path would have had to reinvent: a total order under one lock, bounded per-subscriber
  queues, and an `emit` that never blocks — so a stalled IMAP client cannot throttle SMTP delivery,
  and an idling session cannot accumulate events without limit. A lagging subscriber is told how
  many it missed and stays usable; `EventStream::next` treats lag as a gap to log, not as an end of
  stream.
* **The subscription lives exactly as long as the `IDLE`.** Taken on entry — *before* the
  continuation is written, so a change landing between the two is not missed — and dropped on exit,
  however the `IDLE` ended. A session that never idles holds no subscription.
* **`IDLE` is advertised only when a source was injected.** `Capabilities::with_idle` defaults to
  false, so a deployment with no event source under-promises rather than telling clients to wait for
  updates that cannot arrive.

**Mapping happens in the adapter.** `crate::idle::Update::from_event` turns a `DomainEvent` into the
protocol's `* n EXISTS` / `* n EXPUNGE` / `* n FETCH (FLAGS …)`, and returns `None` for any event
naming a mailbox other than the selected one. That filter is the isolation guarantee: a session
idling on `INBOX` is never told that `Archive` changed, which would be both an information leak and
a protocol error, since the sequence numbers in such a response would address the wrong mailbox.

**Known limitation — `[APPENDUID]` cannot be distinguished from `UIDNEXT` by test.** The `APPEND`
handler reads the destination mailbox and then inserts, with nothing in between, so
`Mailbox::uid_next` and the UID storage assigns are always equal in a single-threaded test. A
mutation substituting one for the other therefore survives this suite. The handler reads
`entry.uid` — the value `insert` returned — because under real concurrency the two diverge and the
counter would name another client's message. `appenduid_is_the_uid_storage_assigned` says so in its
comment rather than pretending to prove it.

### ADR-054 — An unsolicited update changes the session's `SequenceMap` and its response line together

**Context.** ADR-046 fixed a selected session's snapshot at `SELECT` time; ADR-047 relaxed that for
flag changes the session itself made; ADR-049 relaxed it for expunges the session itself performed.
`IDLE` breaks the remaining assumption: a snapshot must now change because of something *another*
session did, while this session is not running a command.

That is the dangerous case. The client's view and the session's view are updated by two different
mechanisms — a line on the wire and a mutation in memory — and if they disagree, every subsequent
sequence number in the session refers to a different message than the client means.

**Decision.** `crate::idle::apply` performs the snapshot change and returns the response line, as
one call. There is no path that writes the line without making the change, or vice versa.

* **An arrival** inserts the placement into the map (in UID order — `SequenceMap::insert` places it
  rather than appending, because the map is UID-ordered by definition) and *then* reports
  `* <len> EXISTS`. The count is the snapshot's length, so the number the client is given is the
  number the session will honour. A UID already present inserts nothing and reports nothing: a
  duplicate would claim the mailbox grew when it did not.
* **A removal** resolves the sequence number *before* removing, then removes and reports it —
  the ADR-049 ordering, now driven by another session's action.
* **A flag change** records the flags and reports `* <seq> FETCH (UID <uid> FLAGS (…))`. RFC 9051
  §7.5.2 has no dedicated flag-change response: a flag update *is* an unrequested `FETCH` result.
  The `UID` item is included so the response is unambiguous even to a client whose own numbering is
  a step behind.
* **An update naming a message the snapshot cannot place produces no line at all** — an `EXPUNGE`
  for a UID already gone, a flag change for one never seen. Both are ordinary under concurrency, and
  inventing a sequence number for either would shift every message after it in the client's view.

**UIDs are never rewritten.** Positions move; identities do not. `UID FETCH`, `UID STORE` and
`UID MOVE` are unaffected by any renumbering an unsolicited update causes, which is what makes UID
addressing worth having.

**Only an idling session is updated.** A session sitting at the command prompt receives no
unsolicited lines: it is not reading for them, and injecting one mid-response would desynchronise
its parser. Such a session still learns about changes the way it always has — its own commands, and
re-selecting.

**Verified by mutation.** Dropping the `insert`, the `remove_uids` or the `set_flags` from `apply`
— while still emitting the line — fails between two and twelve tests each, including
`an_expunge_renumbers_the_idling_sessions_sequence_map`, which asserts that after `* 1 EXPUNGE` the
idling session's `FETCH 1` returns the *surviving* message.

### ADR-055 — `NAMESPACE` reports one personal namespace and `NIL` for the rest, because the model has nothing else

**Context.** RFC 2342 §5 lets a server describe three namespace classes: personal, other users',
and shared. A server that supports none of the latter two must still say something about them, and
the two available answers are not equivalent.

**Decision.** Report `* NAMESPACE (("" "/")) NIL NIL`, derived from the mailbox model rather than
configured.

**Why this shape is forced, not chosen.** `MailboxStore::list` takes an `AccountId` and returns that
account's mailboxes and nothing else. There is no query that reaches another account's mailboxes and
no name syntax that could address one, so every name a client can reach is personal and reachable at
no prefix at all — which is exactly one personal namespace with an empty prefix. The delimiter is
`sandesh_core::HIERARCHY_DELIMITER`, the same constant `LIST` reports, so the two responses cannot
drift; a client that learns `/` from `NAMESPACE` and then fails to parse a `LIST` line would be
broken by the disagreement rather than by either line alone.

**`NIL`, not `()`.** RFC 2342 §5 distinguishes them: `NIL` means "this server has no such
namespace", while an empty list means "it has some, but you may see none of them". Only `NIL` is
true here. Returning a fabricated `(("#shared" "/"))` would advertise a hierarchy that no `LIST`
could ever enumerate — the client would show the user an empty folder tree it could not explain.

**Consequences.** The response is a constant given the delimiter, so it needs no storage access and
is trivially deterministic. If the mailbox model ever grows genuine sharing, this ADR must be
revisited *before* the response changes — the response is a claim about the model, and the two have
to move together.

**Verified by mutation.** Fabricating a shared namespace, hard-coding the delimiter, and using a
non-empty personal prefix each fail between two and five tests, including the unmodified Python
`imaplib` interoperability test, whose own strict parser rejects the fabricated shape.

### ADR-056 — `ENABLE` is implemented as a command before any extension is enableable

**Context.** RFC 5161 `ENABLE` is the negotiation point for extensions that change response syntax.
The obvious sequencing is to implement it alongside the first such extension. But `CONDSTORE`, the
first candidate, is blocked on a storage change (a mailbox-scoped monotonic modification sequence
that `message_entries.mod_seq` does not provide), and clients ask for `ENABLE` on its own.

**Decision.** Implement and advertise `ENABLE` now, with an empty set of enableable extensions. It
parses, validates state, answers `OK`, and sends a truthful `* ENABLED` line — which is currently
always empty.

**Why this is not the capability dishonesty the project forbids.** Advertising `ENABLE` is a claim
about the *command*, and the command genuinely works. It would be dishonest to advertise `CONDSTORE`
beside it, and that is precisely what is not done: `Extension::is_implemented` returns `false` for
both `CONDSTORE` and `QRESYNC`, they are absent from `CAPABILITY`, and `ENABLE CONDSTORE` answers
`OK` with an empty `* ENABLED`. RFC 5161 §3.1 requires exactly that — an unimplemented capability is
ignored rather than rejected, and its absence from the report is the refusal the client is required
to read.

**Recognised is separate from implemented.** `Extension` lists the names a client realistically
sends; `is_implemented` is a second, independent question with its own test. Keeping them apart
makes the refusal deliberate and asserted rather than an accident of a missing match arm, and
reduces the eventual `CONDSTORE` work to flipping one arm once the storage semantics exist.

**Session-local by construction.** The enabled set lives in `Session`, so it dies with the
connection. Nothing else holds a reference, which makes cross-session leakage structurally
impossible rather than merely prohibited: one client enabling syntax-changing behaviour must not
change what another client — entitled to a parser that does not know the extension — is sent.

**Authenticated-state only.** Narrower than every other authenticated command, and refused in both
directions: before login, and again once a mailbox is selected (RFC 5161 §3.1), because enabling an
extension can change the syntax of untagged responses a selected mailbox is already streaming. This
required a distinct `Refusal::ForbiddenWhileSelected`, the mirror image of `NeedsSelectedMailbox`.

**A `cfg(test)` extension makes the machinery testable.** With nothing implemented, `enable` always
receives an empty set, so "the set is stored" and "the set is discarded" are the same program and no
honest test can distinguish them. Rather than assert a vacuous truth — or flip `CONDSTORE` to
implemented to make a test pass — a `cfg(test)`-only `Extension::Probe` gives the set one real
member to carry. It cannot be reached by a client and cannot appear in a capability list.

**Known mutation limitation.** With no shipping extension enableable, one mutation survives: making
the session's enabled set global instead of per-session. A shared empty set and a private empty set
are observationally identical, so this is structurally indistinguishable rather than untested — the
unit-level isolation test kills the equivalent mutation on `Enabled` itself, and the end-to-end
leakage test becomes able to kill the session-level one as soon as a shipping extension exists.
Recorded here rather than papered over with a test that appears to cover it.

### ADR-057 — `ESEARCH` reuses the `SEARCH` engine and refuses `SAVE`

**Context.** RFC 9051 §6.4.4 adds `SEARCH RETURN (...)`, answered with the
`* ESEARCH` response of §7.3.4. The tempting implementation is a second search
path specialised for the return options — `COUNT` in particular looks like it
wants a count query rather than a result list.

**Decision.** Parse `RETURN (...)` off the front of the criteria and hand the
remainder to the existing compiler unchanged. Matching is identical either way;
only the *formatting* of the result differs.

**Why one engine.** `MIN`, `MAX`, `ALL` and `COUNT` are all views of one result
list, so a second path could only ever agree with the first by coincidence. The
compiled query and sequence sets are asserted equal with and without a `RETURN`
clause, and `COUNT` is asserted equal to the classic `* SEARCH` line's length —
so an `ESEARCH` cannot disagree with the `SEARCH` it would have been.

**`ALL` uses the existing sequence-set representation.** `SequenceSet::from_numbers`
coalesces a result list into `1:4,9` form, and lives in `crate::sequence`
beside `parse` and `Display` because that module owns what a sequence set *is*.
Coalescing elsewhere would be a second notion of the same wire form, and the two
would eventually disagree about an edge — a singleton must render `5`, never
`5:5`.

**Empty results omit keys rather than sending empty ones.** RFC 9051 §7.3.4:
there is no minimum of nothing and no set naming no messages, so `MIN`, `MAX`
and `ALL` disappear when nothing matched. `COUNT 0` is still sent, because zero
is a correct count. Sending `MIN 0` would *name a message*, since 0 is not a
valid sequence number or UID.

**Unsupported return options are refused, not ignored.** The opposite of the
`ENABLE` rule (ADR-056), deliberately. `ENABLE` negotiates future syntax, so
ignoring a name costs the client nothing; a `RETURN` option names the answer the
client is waiting for, and silently dropping one returns a response that parses
but answers a different question.

**`SAVE` is not implemented.** RFC 9051 §6.4.4 `SAVE` stores the result in the
session's search-result variable for later `$` reference. That requires `$` to
resolve everywhere a sequence set is accepted — `FETCH`, `STORE`, `COPY`,
`MOVE` — and no such variable exists. Accepting `SAVE` while ignoring it would
leave a subsequent `FETCH $` addressing nothing, so it is refused as an
unsupported option.

**Interoperability limitation.** Python `imaplib`'s `uid("SEARCH", …)`
hard-codes `_untagged_response(…, "SEARCH")` and therefore never looks for an
`ESEARCH` response, returning `None` whatever the server sent. This is a
limitation of `imaplib`, and the server is deliberately not changed to
accommodate it: RFC 9051 §7.3.4 requires `ESEARCH` once `RETURN` is given, and
emitting `* SEARCH` instead would break every conforming client to please one.

### ADR-058 — `\HasChildren` is computed from the mailbox set, and special use is a closed vocabulary

**Context.** `LIST-EXTENDED` (RFC 5258) and `SPECIAL-USE` (RFC 6154) both add
name attributes to `LIST` output. They have opposite natures: a special use is
a *stored property of one mailbox*, while being a parent is a *fact about the
mailbox set*.

**Decision.** Persist special uses (they already were — `mailboxes.attributes`
carries them as CSV, and account provisioning assigns five of the six).
Compute `\HasChildren`/`\HasNoChildren` at listing time. No schema change was
needed for either.

**Why children are computed, not stored.** A stored flag would need updating on
every `CREATE`, `DELETE` and `RENAME` — including a `RENAME` that moves a whole
subtree — and each missed update is a client that stops descending a hierarchy
that still exists. Derivation has no such failure mode.

**Children are computed against the whole account, not the filtered result.**
`LIST "" Work` returns only `Work`, but `Work` still has a child. Computing from
the filtered list would report `\HasNoChildren` for a mailbox that plainly has
children — the client would show the hierarchy as ending there. The delimiter is
part of the child prefix, so `Workshop` is not a child of `Work`.

**Special use is a closed vocabulary, and `\Noselect` is not part of it.**
`SPECIAL_USE_ATTRIBUTES` in `sandesh-core` lists exactly the six names RFC 6154
§2 defines, and `MailboxAttributes::special_use` filters to them. Matching a
`LIST (SPECIAL-USE)` request against the *full* attribute list would let
`\Noselect` masquerade as a special use, and a client would present a container
mailbox as its archive.

**`\All` is recognised but never assigned.** This server provisions no all-mail
mailbox, so no mailbox bears it. Recognising a name a client may send in
`CREATE ... (USE (…))` is separate from creating a mailbox that has it.

**`CREATE ... (USE (…))` validates before creating.** An unrecognised use is
`NO [USEATTR]` (RFC 6154 §3) — a refusal of the *attribute*, not a syntax
error, so the client can retry without it — and nothing is created. Names are
matched case-insensitively but stored in the RFC's own spelling, so a client
matching `\Archive` exactly finds it.

**Deduplication lives in the type, not the parser.** `MailboxAttributes` is a
`BTreeSet`, so a duplicated `USE` cannot produce a duplicated attribute. The
equivalent check while parsing `CREATE` is defence in depth; a mutation removing
it is structurally indistinguishable at the protocol boundary, so the guarantee
is asserted in `sandesh-core` where it actually holds rather than with a
protocol test that would pass either way.

**`RETURN (STATUS (…))` reuses `StatusItem`.** The same parser and renderer the
`STATUS` command uses, reading the same `MailboxStore::counts` port — so the two
cannot report different numbers for one mailbox, and `RETURN (STATUS (RECENT))`
is refused for exactly the reason `STATUS RECENT` is (no `\Recent` state is
kept, and reporting a constant would be fabricated).

**`RECURSIVEMATCH` and `REMOTE` are refused.** Each changes *which* mailboxes
are listed, so ignoring either would answer a different question with a
well-formed response the client cannot audit. `CHILDREN` is accepted but sets
no flag: the attributes are computed for every listing anyway, which RFC 5258
§3.2 permits.

### ADR-059 — CONDSTORE: a mailbox-owned modification sequence

**Status:** Accepted. Supersedes the CONDSTORE half of ADR-056 and resolves OD-2.

RFC 7162 rests on one comparison: "give me everything that changed since `n`". That only works if
the value is monotonic **within the mailbox** and never re-issued.

**The sequence is owned by the mailbox, not the entry.** `0001` reserved
`message_entries.mod_seq` as a per-entry counter incremented in place. That is not a MODSEQ: two
entries changed at different moments both sit at 2, so "changed since 1" cannot distinguish them,
and nothing records a high-water mark for `HIGHESTMODSEQ`. Migration `0002` adds
`mailboxes.highest_mod_seq` and raises each mailbox's mark above any per-entry value already
present, so no issued value can be handed out twice. Existing `mod_seq` values are **left in
place** rather than rebased: they are already at or below the new mark, and inventing distinct
values would fabricate ordering information that was never recorded.

**Allocation mirrors `allocate_uid`.** `mailbox_repo::allocate_mod_seq` is a single
`UPDATE … RETURNING` taking the caller's transaction, exactly as UID allocation does. One atomic
statement is what stops two concurrent writers receiving the same value — a duplicate would make
`CHANGEDSINCE` skip a change and silently desynchronise a client. It returns the value it just
wrote, unlike `allocate_uid`, because `highest_mod_seq` *is* the newest issued sequence whereas
`uid_next` names a UID not yet used.

**A no-op allocates nothing.** RFC 7162 §3.1.11: advancing the sequence for a store that changed
nothing forces every other client to re-fetch an identical message. `update_flags_conditional`
compares the flags before and after rather than assuming the write changed something, and returns
`FlagChangeStatus::Unchanged` with the entry's existing sequence.

**The precondition lives inside the writing transaction.** Reading the modseq, deciding, then
writing would leave a window in which another session stores flags between the two — the exact
lost update `UNCHANGEDSINCE` exists to prevent, with the client told it succeeded. The comparison
is `current > limit`: equality means the client holds precisely this revision and may proceed.

**`BEGIN IMMEDIATE`, not the default deferred transaction.** `update_flags_conditional` is this
codebase's first read-decide-then-write transaction. A deferred transaction begins as a *reader*
and must upgrade at the `UPDATE`; SQLite refuses that upgrade with `SQLITE_BUSY` as soon as another
writer holds the lock, and `busy_timeout` does **not** wait it out, because two readers both
wanting to upgrade can never resolve by waiting. Taking the write lock up front turns a hard
failure into a queue. This was found by the concurrency tests, not by review.

**The event carries the modseq.** `EntryFlagsChanged` gained a `mod_seq` field so a CONDSTORE
session can send the unsolicited `FETCH … MODSEQ` of RFC 7162 §3.2 without a second read — a
re-read could observe a *later* change and report a sequence that does not correspond to the flags
being announced.

**Snapshot semantics: flags and modseq now travel together.** `SequenceMap::set_flags` previously
updated only the flags. A flag change *is* a modseq change, so recording one without the other
left the snapshot internally inconsistent and a following `FETCH MODSEQ` in the same session
reported the pre-change value — telling the client its own store had not happened. The snapshot's
*membership* and ordering remain fixed at selection time (ADR-046); only these two paired fields
move. This is the one snapshot-semantics change the RFC forced.

**`ENABLE CONDSTORE` gates unsolicited data only.** Per RFC 5161 §3 a client that did not enable an
extension is entitled to a parser that does not know its syntax, and an unsolicited response is
where an unexpected item would break one. Explicitly requested data is not gated: `FETCH 1
(MODSEQ)` is answered regardless, or the item would be unusable for the many clients that never
send `ENABLE`. `HIGHESTMODSEQ` on `SELECT` is likewise unconditional — it is an `OK` response code,
and RFC 9051 §7.1 requires clients to ignore codes they do not know.

**One search engine, one sequence-set formatter.** `MODSEQ` is a new `SearchQuery` variant
evaluated by the existing `matches`, not a second search path (ADR-057 still holds). The `MODIFIED`
response code coalesces its UIDs through `SequenceSet::from_numbers`, the same owner of all
coalescing.

**`MODSEQ "/flags/\Seen" all <n>` is refused.** RFC 7162 §3.1.5's long form names the flag whose
metadata is being asked about. This server keeps one sequence per placement, not per flag, so
accepting it would mean ignoring two of its three arguments and answering a different question.

**QRESYNC is still not implemented, and is not advertised.** It needs a durable record of expunged
UIDs so `VANISHED (EARLIER)` can tell a returning client what disappeared while it was away; an
expunged placement here is simply gone. Advertising it would strand a client waiting for a
`VANISHED` that never arrives. `ENABLE QRESYNC` is recognised and refused, and `VANISHED` as a
`FETCH` modifier is rejected rather than ignored for the same reason.

---

### ADR-060 — Advertise every implemented capability, and `LITERAL-` rather than `LITERAL+`

**Context.** The global `CAPABILITY` audit that `ROADMAP` and `CONTEXT` both listed as the next
Phase 5 step. `Capabilities::to_list` documents the rule it exists to enforce — "a capability this
server cannot honour is worse than a missing one" — but the audit found the list had drifted in the
*opposite* direction: three extensions were fully implemented and none were advertised.

- **`UNSELECT` (RFC 3691).** `CommandKind::Unselect` parses and dispatches alongside `CLOSE`,
  differing only in never expunging.
- **`SASL-IR` (RFC 4959).** `AUTHENTICATE` already reads an optional initial response from its
  second argument, held as a `Secret` from the first moment (ADR-035).
- **Non-synchronising literals (RFC 7888).** The parser accepts `{n+}` and the session suppresses
  the continuation request for it.

An unadvertised-but-implemented capability is a quieter fault than the reverse — nothing breaks —
but it is still a fault: a client that cannot see `SASL-IR` spends an extra round trip on every
login, and one that cannot see `UNSELECT` closes and reselects to deselect without expunging.

**Decision.** Advertise all three unconditionally, alongside the existing unconditional set. None
depends on configuration or an injected dependency, so none can become unanswerable mid-session.

**The name is `LITERAL-`, not `LITERAL+`.** RFC 7888 defines two capabilities over the same `{n+}`
syntax, differing in what they promise about *size*. `LITERAL+` promises to accept a
non-synchronising literal of any length; `LITERAL-` is the bounded form. This server enforces
`max_literal_size` (25 MB by default) and returns `LiteralTooLarge` above it, so `LITERAL+` would be
exactly the unhonourable capability the module warns against — a client would be promised something
the size cap can refuse. `LITERAL-` is the honest name for what is implemented.

**Consequences.** Two regression tests pin this. One asserts every unconditional capability appears,
so the list cannot silently drift again; the other asserts `LITERAL+` never does. Both compare exact
space-delimited tokens rather than substrings, because `contains("LITERAL+")` is fine but
`contains("LITERAL-")` would also match a hypothetical `LITERAL-FOO`, and — the live trap —
a substring test for `LITERAL+` would be unaffected by `LITERAL-` while a careless reverse test
would not be.

**Still deliberately absent.** `QRESYNC`, for the durable expunged-UID reason in ADR-059, and
modified-UTF-7, which is unimplemented rather than merely unadvertised.

---

### ADR-061 — Modified UTF-7 at the adapter boundary, decoded in and encoded out

**Context.** Found against a running server, not by review. The server accepted both
`CREATE "Wichtig-&APw-ber"` (a rev1 client's modified UTF-7) and `CREATE "Wichtig-über"` (a rev2
client's UTF-8) and **created two separate mailboxes for the same logical folder**, each
independently selectable. A user running one encoding client and one non-encoding client would
silently accumulate duplicate folders that diverge from each other.

`MailboxName`'s doc comment already described the intended design — names are stored as Unicode and
"modified-UTF-7 is a wire encoding the IMAP adapter applies only for older clients" — but the
adapter never applied it, so encoded bytes fell through as a literal name. The gap was between a
documented intent and an unwritten conversion, which is why no test caught it: every test used
ASCII names, where the two encodings coincide.

This exists at all because ADR-042 advertises the `IMAP4rev1` capability name for client
compatibility. A client that sees `IMAP4rev1` is entitled to encode, and to decode what it reads.

**Decision.** `crates/sandesh-imap/src/utf7.rs` implements RFC 3501 §5.1.3, wired at the two
chokepoints that already existed:

- **Inbound**, in `Session::mailbox_argument` — the single place a wire name becomes a
  `MailboxName`. Decode happens before validation, so the domain only ever sees Unicode.
- **Outbound**, in `quote_mailbox` — the single place a name is rendered onto the wire.

Core is untouched: encoding is not the domain's concern, and the architecture test still passes with
no new dependency.

**Decoding is lenient in exactly one respect.** Input that is not valid modified UTF-7 is returned
*unchanged* rather than rejected. A rev2 client may legally send a literal `&` in a UTF-8 name, and
that is indistinguishable from a malformed escape; only rev1 clients are obliged to encode.
Returning the input preserves the legal rev2 name, while a rev1 client that sends a broken escape
gets a mailbox named what it literally typed — visible and harmless. Rejecting would break the legal
case to tidy up the illegal one.

**Consequences.** Pure-ASCII names encode and decode to themselves, so rev2 clients, every existing
test and the entire on-the-wire surface are unchanged; that is why only one test expectation moved.
`quote_mailbox("Ärchiv")` now yields `&AMQ-rchiv` rather than a quoted raw-UTF-8 name — the earlier
expectation assumed a rev2-only reader, which ADR-042 makes untrue. Encoded output is pure ASCII and
so needs no quoting.

**Verification.** Unit tests cover the RFC 3501 reference vector (`~peter/mail/&U,BTFw-/&ZeVnLIqe-`),
surrogate pairs, the `,`-for-`/` substitution, malformed input and a never-panics sweep. An
`imaplib` interop test drives the duplicate-folder scenario through a parser we did not write, and
was confirmed to fail with the decode removed. The live server was re-checked afterwards: the second
`CREATE` is now refused as already existing, and `LIST` shows one folder.

---

### ADR-062 — `CHECK` is implemented as `NOOP`, and real clients are testable after all

**Context.** Two findings, one from running a real client binary.

The roadmap recorded Thunderbird/`mutt`/`mbsync` interop as blocked because the binaries were
unavailable. That was wrong, and the reasoning behind it was sloppy: `sudo` requires a password
here, from which it was inferred that nothing could be installed. In fact `apt-get download` plus
`dpkg-deb -x` into a scratch prefix needs no root at all. `mbsync` 1.4.4 and `mutt` 2.2.12 both run
this way (mutt needs `libgsasl18`, `libtokyocabinet9t64`, `libgssglue1` and `libidn12` alongside).

Running real `mbsync` immediately found what the scripted `imaplib` sync test could not: after
propagating flags, `mbsync` issues `CHECK`, and the server answered `BAD unrecognised command`.
`CHECK` is a mandatory Selected-state command (RFC 9051 §6.4.1) and was absent from `CommandKind`
entirely. A scripted test only asks what its author thought to ask; a real client asks what it
actually needs.

**Decision.** Implement `CHECK` as `NOOP`. RFC 9051 §6.4.1 describes it as a checkpoint — its
example is flushing in-memory state to disk — and states that a server with no such housekeeping
may treat it as equivalent to `NOOP`. Every mutation here commits inside its own transaction before
the tagged reply is written, so nothing is deferred and there is nothing to flush. Answering `OK`
is therefore truthful, not a stub.

It is grouped with `FETCH`/`SEARCH`/`COPY` in the state machine rather than with the mutating
commands: it requires a selection but changes nothing, so an `EXAMINE`d read-only selection may
issue it.

**Consequences.** `mbsync` now completes a full bidirectional sync with exit 0 and no protocol
errors: seven folders mirrored including a non-ASCII one, three messages downloaded with matching
UIDs, an incremental pass that fetched only the new message, and a local `\Seen` propagated back to
the server. That is the phase's headline acceptance criterion met for one of the three named
clients, by the real binary rather than a stand-in.

The `state_machine.rs` sweep has a hardcoded `COMMANDS` array length, which forced the new variant
to be acknowledged rather than silently missed from every permission invariant — the test doing
exactly its job.

**`mutt` is partially verified.** It connects and authenticates (confirmed in the server log), but
its browse and compose modes need a TTY and a local sendmail binary, neither of which exists here.
Its IMAP conversation produced no protocol errors. Thunderbird remains unverified: it is a GUI
client with no headless sync mode.

---

### ADR-063 — `BODYSTRUCTURE` is verified against the whole fixture corpus

**Context.** Phase 5's acceptance criteria require `BODYSTRUCTURE` to be "correct for every fixture
in the corpus". An audit of the criteria against the tests found it was asserted for **six** of the
**22** fixtures, all of them well-formed. The eleven fixtures no IMAP test touched at all were
mostly the malformed ones — `unterminated-boundary`, `truncated-base64`, `invalid-content-type`,
`no-headers`, `eight-bit-header`, and a zero-byte `empty.eml`.

That is the wrong half to leave untested. RFC 9051 §7.5.2 gives a server no way to say "this message
has no structure", so a message that cannot be parsed must still produce a well-formed response.
The failure modes are severe and outlive the bad message: a panic drops the connection, and
unbalanced parentheses desynchronise the client's parser for every response that follows.

**Decision.** Two corpus-wide tests in `fetch.rs`, alongside the existing per-fixture ones, which
are kept — they assert specific shapes that a generic test cannot.

- `bodystructure_is_well_formed_for_every_fixture_in_the_corpus` — every fixture answers, with
  balanced parentheses, a parenthesised list, and a media type; the session survives all 22 on one
  connection.
- `body_and_bodystructure_agree_across_the_corpus` — `BODYSTRUCTURE` is `BODY` plus extension data
  (§7.5.2), so it can never be shorter, and the two must agree on multipart-ness.

The corpus is **enumerated from disk**, not listed, so a fixture added later is covered without
anyone remembering to extend the test.

**The tests assert on the raw wire line, not only on the extracted value.** This was not the first
attempt. Both tests initially passed, and an injected fault — the closing parenthesis dropped from
the degenerate path — was caught by only one of them. The reason is a trap worth recording: the test
harness's `value()` walks parentheses to find an item's end, so it returns `None` for a *malformed*
structure exactly as it does for a *missing* one, and in the single-item `FETCH 1 BODYSTRUCTURE`
form the enclosing `FETCH (…)` parens can balance an omission inside. A test that only inspects what
the helper extracted therefore cannot see the fault that matters most. Checking the untagged line
itself is what a real client's parser consumes, and is the only place an imbalance is unambiguous.

**Consequences.** The criterion is now met as written. The paren scanner tracks quoting and
backslash escapes, because a parenthesis inside a quoted filename (`report(final).pdf`) is data, not
structure — counting it would produce false failures on legitimate attachments. Re-running the
injected fault after the fix failed both tests, with a message naming the fixture and printing the
malformed line.

---

### ADR-064 — The cross-protocol matrix and the IDLE latency budget are enforced

**Context.** The audit that produced ADR-063 found two more acceptance criteria asserted by
inspection rather than by test.

*Cross-protocol.* Phase 5 requires cases 1, 3, 4, 5, 10, 12 and 13 of the TESTING_STRATEGY §7
matrix. Cases 10, 12 and 13 were covered inside `sandesh-imap`'s own suite, which is adequate —
they are single-protocol behaviours. Cases 4 (IMAP `APPEND` → POP3 `RETR`) and 5 (IMAP `STORE
\Seen` → REST) had **no test at all**, and case 3 was covered only sequentially. Those three are
precisely the ones that cannot be tested inside one crate: each asserts that state written through
one adapter is visible through a *different* one, which needs all four listeners over a single
`Application`.

*IDLE latency.* "IDLE delivers `EXISTS` within 100ms of SMTP delivery" had tests proving the
notification arrives, and none proving when. The regression that would break it — a notification
parked behind a poll interval or a timeout instead of driven by the event bus — is invisible to a
test that waits five seconds for the line to appear.

**Decision.** Three tests added to `tests/cross_protocol.rs`, which already had the four-protocol
harness, and one to `idle.rs`.

- **Case 4** asserts a message that never touched SMTP is visible to POP3, and checks `APPENDUID`
  on the way (UIDPLUS, ADR-054). If anything about storage were special-cased on the SMTP path,
  this is where it would surface.
- **Case 5** asserts a flag set over IMAP appears in the REST listing. It guards against a flag
  cached in the acting session's snapshot and never committed — the client that set it would see
  its own change and nothing else would.
- **Case 3** holds a POP3 session open *while* driving IMAP, then uses the POP3 session again
  afterwards. ADR-006 is the claim under test: a POP3 session takes no exclusive lock. A sequential
  test would pass against a design that locks, so the overlap is the entire point.
- **Latency** measures from just before the delivery commits to the moment the line is read — the
  bus hop, the session wake-up and the socket write, which is everything between the two events the
  criterion names.

**Consequences.** Measured `EXISTS` latency is ~640µs against the 100ms budget: roughly 150x
headroom, so the assertion will not flake, and it fires correctly when the budget is lowered below
the real figure.

**Case 5's assertion was wrong on the first attempt, and passing.** It matched `"Seen"` anywhere in
the response body, which almost anything satisfies. Rewritten to parse the JSON and compare flag
values exactly, it then failed — because flags live on the per-mailbox `entries[]` object, not on
the message. That is not an inconvenience of the DTO but the shape case 10 exists to defend: one
message, several mailboxes, independent flags. Verified afterwards by storing `\Flagged` instead
and confirming the test distinguishes the two.

---

### ADR-065 — POP3 canonicalises line endings on the wire, and `RETR` announces no octet count

**Context.** Phase 6's acceptance criteria make two claims about sizes: that `RETR` returns
byte-identical content correctly dot-stuffed, and that `STAT` octet counts match `RFC822.SIZE`.
The pre-phase audit found the second asserted nowhere — every `STAT` test checked the message
*count* and ignored the octet total — and predicted the first was broken because `handle_retr`
announced the stored size while writing `dot_stuff()` output, which is a different length.

Writing the tests found the announced-count problem was the lesser half. `dot_stuff` repaired only
the *final* terminator: interior bare LFs passed through untouched. Storage keeps exact octets
(`CORE_SPEC` §3) and SMTP preserves a bare LF verbatim (Phase 2), so such messages reach POP3
routinely. Serving them raw sends a payload whose lines are not CRLF-terminated, which
desynchronises the client's line parsing — and, worse, a body line beginning with `.` that
followed a bare LF was never recognised as a line start, so it was **not dot-stuffed** and the
client reads it as the end-of-message terminator. That truncates the message silently.

**Decision.** Two changes, in opposite directions.

*Canonicalise line endings.* `dot_stuff` now emits CRLF for every LF not already preceded by CR,
across the whole payload rather than at the terminator alone. This is wire encoding, not mutation
of the stored message: RFC 1939 frames a multi-line response as CRLF-terminated lines, so a bare
LF cannot be represented on the wire at all. IMAP is unaffected — it serves bytes inside a
counted literal, where no framing question arises, which is why `FETCH BODY[]` could stay verbatim
while `RETR` cannot.

*Drop the octet count from `RETR`.* The greeting is now `+OK message follows`. RFC 1939 §5 makes
the count optional and clients read to the `.` terminator regardless. The only honest number would
be the post-stuffing wire length, which is neither the `RFC822.SIZE` that `LIST` and `STAT` report
nor a figure any client consumes — printing it would invite the reader to assume the three agree.
`LIST` and `STAT` keep the stored size, because that is what the acceptance criterion requires.

**Consequences.** A bare-LF message is now retrievable intact over POP3, and a leading-dot line in
one no longer truncates it. `RETR` no longer reports a size; a client that displays one will show
nothing rather than a wrong number. The stored bytes are untouched, so IMAP and the REST API still
return the original octets exactly.

**Alternatives rejected.** *Announce the post-stuffing length.* Accurate, but it would make `RETR`,
`LIST` and `STAT` report three different numbers for one message with no way for a reader to know
which is which. *Serve bare LF verbatim and accept the framing risk.* This is what the code did,
and it silently truncates messages. *Canonicalise at delivery instead.* That would violate the
Phase 2 criterion that line endings are preserved verbatim, and corrupt the IMAP and REST views to
fix a POP3 encoding problem.

---

### ADR-066 — Protocol traces live in a `connections` table, recorded at the `Connection` boundary

**Status.** Accepted. Implemented in Phase 7 increment A2.

**Context.** Phase 7 requires that "every connection to every protocol produces a retrievable
trace". Today `deliveries.trace` is the only place a transcript could go, and it is written `NULL`
by the only production call site (`crates/sandesh-smtp/src/session.rs`).

That column cannot satisfy the criterion, for a reason that is structural rather than incidental:
**a delivery is not a connection.** They do not correspond even for SMTP, and for the other two
protocols the mismatch is total.

- One SMTP connection can carry many transactions, so it can produce many deliveries. Hanging the
  transcript off a delivery row means either duplicating it per transaction or arbitrarily
  attributing a whole conversation to one of them.
- An SMTP connection that delivers nothing — a probe that greets and quits, a client rejected at
  `RCPT`, a failed `STARTTLS` — produces no delivery row at all, so its trace has nowhere to go.
  These are among the most diagnostically interesting connections; a viewer that cannot show a
  failed handshake is not answering the question a developer actually has.
- IMAP and POP3 never produce a delivery row at all: neither crate references `DeliveryStore`.
  `APPEND` does stamp `DeliverySource::ImapAppend`, but on the stored *message*, not a delivery
  record — so there is still no row to attach a transcript to. Under the current schema two of the
  three protocols could not be traced whatsoever, silently falling outside a criterion that names
  all of them.

Three further facts constrain the design, all verified in the code rather than assumed:

1. **All three protocols share one IO chokepoint.** SMTP, IMAP and POP3 each construct
   `sandesh_proto::Connection` and read and write exclusively through its methods
   (`read_line`, `read_line_owned`, `read_exact_owned`, `write`, `write_flush`). Nothing bypasses
   it, so one recorder there covers every protocol without touching three session loops.
2. **A `STARTTLS` upgrade builds a *second* `Connection` over the same TCP connection.** The
   plaintext session is deliberately discarded and a fresh one runs over the TLS stream
   (ADR-036, RFC 3207 §4.2). A recorder scoped to a `Connection` would therefore split one client
   conversation into two unrelated traces, which is precisely the case a developer debugging
   `STARTTLS` most needs to see whole.
3. **Traces are unbounded growth that nothing currently caps.** `STORAGE_DESIGN.md` §5 caps
   messages at 5000 by default specifically so a runaway test suite cannot fill a developer's
   disk. `deliveries` has no such cap today, and traces are larger and produced more often.

**Decision.**

*A `connections` table, not `deliveries.trace`.* One row per accepted connection, carrying the
protocol, peer, whether the transport was encrypted, open and close timestamps, how the connection
ended, and the transcript. `deliveries` gains a nullable `connection_id` so an SMTP delivery links
to the conversation that produced it, and the delivery view can offer the transcript alongside the
routing outcome. `deliveries.trace` is then dead and is dropped rather than left as a second,
divergent place a transcript might live.

*Recorded at the `Connection` boundary, but owned above it.* The recorder is a handle the listener
creates per **accepted socket** and passes into `Connection::new`; when `STARTTLS` builds the
second `Connection`, the same handle is passed again, so the upgrade appears as one continuous
transcript with the handshake marked in place. This is what fact 2 rules out doing per-`Connection`.

*Redaction happens at capture.* Confirmed by the Phase 7 scope decision and unchanged here:
command and response lines are recorded; `DATA` payloads are replaced with a byte count;
`AUTH`, `PASS` and `LOGIN` arguments never enter the buffer. There is no configuration flag to
capture bodies or credentials. `crates/sandesh-app/src/logging.rs` states why — this server holds
password-reset links and one-time codes, and a transcript is a log. A flag that can leak an OTP is
a flag that eventually will.

*Bounded twice.* Per connection, a line cap and a total byte cap, after which the transcript is
truncated with an explicit marker rather than growing without limit — a long-lived IMAP session
can otherwise run for days. Across the database, a connection-count cap in the same spirit as the
5000-message default, evicting oldest first.

**Consequences.** All three protocols become traceable through one recorder rather than three.
Connections that produce no mail — the failed handshakes and rejected probes — become visible,
which is most of the diagnostic value. The cost is a new table, a migration that drops
`deliveries.trace`, and a recorder in the hot path of every read and write; the caps bound its
memory, and the byte-count elision keeps `DATA` from dominating the buffer.

A trace is no longer byte-exact by construction. That is deliberate: an exact transcript is
precisely the thing the privacy stance forbids, and the raw message is already retrievable
deliberately through the API, where it belongs.

**Alternatives rejected.**

*Keep `deliveries.trace` and narrow the criterion to SMTP.* Cheapest, and it abandons two of the
three protocols the criterion names — while also missing every SMTP connection that delivers
nothing. The criterion is not the awkward part here; the schema is.

*A recorder per `Connection`, no shared handle.* Simpler, and it fragments every `STARTTLS`
session into two traces with nothing linking them.

*Record in each session loop instead of at the `Connection`.* Puts redaction in three places
instead of one, and a redaction rule that must be re-implemented per protocol is a redaction rule
that will eventually be missed in one of them.

*Traces in memory only, never persisted.* No schema change and no disk growth, but "historical"
is explicit in the deliverable, and an in-memory buffer loses exactly the trace a developer wants
after a crash.

### ADR-067 — A copy or move destination is checked against the source's account

**Context.** `POST /api/v1/entries/{id}/copy` and `/move` accept a destination in either of two
forms: `mailbox_name` or `target_mailbox_id`. The name form was resolved with
`get_by_name(source_mailbox.account_id, …)`, so it could only ever name a mailbox inside the
source entry's own account. The id form was not resolved at all — the caller's UUID was passed
straight to `MailboxId::from_uuid` and handed to the store.

Nothing downstream compensated. `MessageStore::copy_entry` and `move_entry`
(`crates/sandesh-storage/src/message_repo.rs`) read the source entry and allocate a UID on
whatever target they are given; neither compares accounts, and no schema constraint relates the
two. So a caller who supplied the id of a mailbox belonging to a *different* account had the mail
placed there, and the server answered `200 OK`. Verified against a running instance before the
fix: two accounts, a message delivered to the first, copied and moved into the second's mailbox,
both succeeding.

This is the account isolation the model exists to provide — the same property
`crates/sandesh-smtp/tests/isolation.rs` protects on the delivery side. IMAP cannot express the
mistake, because `resolve_destination` (`crates/sandesh-imap/src/session.rs`) takes the account
from the authenticated session, so the account is never a request parameter. Over HTTP it is.

The reason the gap survived is worth recording: **no test exercised REST copy or move at all**, so
a suite of ~1700 passing tests said nothing about it. It is a direct consequence of the missing
service layer (ROADMAP Phase 1) — the same operation implemented twice, once structurally safe and
once not.

**Decision.** One helper, `resolve_entry_destination`, resolves both destination forms and is the
only route to a target id for the single-entry copy, the single-entry move, and the bulk move. It
enforces three rules before anything is written:

*The destination must belong to the source's account.* Checked on the id form, which is the only
form that can name another account's mailbox.

*A destination in another account is reported as `404`, identically to one that does not exist.*
Deliberately indistinguishable: a distinct `403` would confirm that a given mailbox id exists in
an account the caller cannot otherwise see, turning the endpoint into an id oracle. The caller is
told the destination is not available to them, which is the whole of what they are entitled to
know.

*A destination equal to the source mailbox is refused with `409`.* This matches IMAP
(ADR-050/ADR-051) rather than letting the placement uniqueness index decide, so a client mistake
does not arrive as a `500`.

The check lives at the API boundary rather than in storage because storage has no notion of a
*request*: `copy_entry(entry, target)` is a legitimate operation between any two mailboxes, and the
constraint being enforced is about who asked, not about what the data permits. When
`MessageService` lands (ROADMAP Phase 1, in progress) this logic moves there and both surfaces
inherit it structurally; until then the API layer is the only place that sees both the source and
the requested destination.

**Consequences.** Cross-account placement over HTTP is refused and asserted by three tests that
check the *effect* as well as the status — that no placement appears in the other account's
mailbox, and that a refused move leaves the source intact rather than losing the mail in between.
Two further tests keep the fix honest: a copy by id *within* one account must still succeed, so the
guard cannot pass by rejecting the id form outright, and an unknown id must report `404` rather
than surfacing a foreign-key error. Reverting the account comparison fails exactly those three
cross-account tests and nothing else.

`resolve_mailbox_by_name` was removed; the new helper subsumes it.

The bulk path keeps reporting a refused destination as a per-entry `false` rather than failing the
batch, so `affected` now counts only placements actually made — previously it counted the
cross-account move it should not have performed.

### ADR-068 — Mailbox operations live in a domain service, and IMAP's semantics are canonical

**Context.** `ROADMAP.md` Phase 1 listed five domain services. Only `DeliveryService` was written,
and it lived in `sandesh-smtp` rather than the core; `MailboxService`, `MessageService`,
`AccountService` and `SearchService` never existed. Every caller therefore used the port traits
directly, so the meaning of an operation was re-decided at each surface.

The two surfaces drifted, and an audit found the divergences are not cosmetic:

*`INBOX` was protected unevenly.* IMAP refused to create it (RFC 9051 §6.3.4) and refused it as
either side of a `RENAME` (§6.3.6). The HTTP handlers refused neither creation nor a rename *onto*
`INBOX` — `PATCH` checked only the mailbox's current name. Both breach the one-`INBOX`-per-account
invariant that `inbox_of` treats as unrecoverable and that every delivery resolves through.

*A store failure read as "the name is free".* Six IMAP lookups matched `Ok(Some(_))` or used
`let Ok(Some(_)) = … else`, collapsing `Err` into absence. At `CREATE` and at `RENAME`'s
duplicate-target probe that inverts the check: a database that could not answer looked like a free
name, and the code went on to create or rename over it.

*A new mailbox's subscription depended on which protocol made it.* IMAP left it unsubscribed, HTTP
set it subscribed, so the same mailbox appeared in `LIST (SUBSCRIBED)` or not according to its
provenance.

*Four copies of the `Mailbox` constructor and four independent `INBOX`-by-name resolutions.*

**Decision.** `MailboxService` in `sandesh-core`, and every surface goes through it.

*IMAP's semantics are canonical where the two disagreed.* A new mailbox starts **unsubscribed**:
RFC 9051 treats subscription as a separate act, and the alternative would have changed behaviour
real clients already exercise. This is a deliberate behaviour change to the HTTP surface, and the
test that asserted the old default was updated rather than worked around.

*Every method returns `CoreResult`.* This is the structural fix for the `Err`-collapse: the
service cannot express "absent or broken, same thing", because `CoreError::NotFound` and
`CoreError::Storage` are separate variants and the caller must handle both. All six collapsing
lookups are gone; `grep` for `get_by_name` in `sandesh-imap` now returns nothing.

*The split follows `CORE_SPEC.md` §9.* In the service: existence, selectability, the `INBOX`
refusals, duplicate detection. Left in the adapters: modified UTF-7 decoding (a wire encoding),
the `[TRYCREATE]` response code (RFC 9051 §6.4.7), deselecting on a failed `SELECT` (§6.3.2), and
fixing up a selected mailbox's name after a rename — all session state or protocol rendering.
`resolve_destination` in the IMAP session was already this service method in the wrong place; its
own comment made the argument.

*`INBOX` refusals stay `400` over HTTP.* The service reports them as `CoreError::InvalidArgument`,
which the blanket mapping renders `422`. A one-line `mailbox_error` shim pins them back to `400`,
because moving where a *rule* lives must not silently change a documented status code. The
generated contract is unchanged, which is the evidence.

*The core still declares no runtime.* The service's tests need to drive `async fn`s, and
`tests/architecture.rs` pins the crate's declared dependencies — dev-dependencies included — to the
list `ARCHITECTURE.md` §3.2 fixes. Adding `tokio` as a dev-dependency to run fourteen tests would
have weakened the check that keeps infrastructure out of the domain, so the tests drive futures with
a small `block_on` over `Waker::noop` instead. The permitted-dependency test is untouched and green.

**Consequences.** One implementation of each mailbox operation, so the divergences cannot reappear
by drift. 1719 → 1723 tests. Fourteen unit tests cover the service directly, including the case a
live database cannot produce: a store that fails on demand, asserting a failed duplicate check
never reads as a free name and writes nothing. Four HTTP tests cover what had no coverage — creating
`INBOX` under three spellings, renaming onto `INBOX`, the unsubscribed default, and a duplicate name
answering `409` rather than reaching the unique index as a `500`. Reverting either `INBOX` guard
fails exactly the two tests that name it.

Migration was per-command with the IMAP suite (1007 tests) run after each step, which is how the one
real regression — the `subscribed` default — was caught as a failing assertion rather than found
later.

Still outstanding from the Phase 1 deliverable: `MessageService`, `AccountService`, and moving
`DeliveryService` into the core (which needs `UnknownRecipientPolicy` mirrored as a domain type,
since `sandesh-config` is not a permitted core dependency). `SearchService` is deliberately not
planned — see the `SearchIndex` note in `ROADMAP.md` Phase 1.

### ADR-069 — Placement operations move into `MessageService`, and Trash resolves from the placement's own account

**Context.** ADR-067 closed a cross-account isolation hole by checking the destination inside the
HTTP handlers. That was the right *fix* in the wrong *place*: the rule — mail may not be placed into
another account's mailbox — is a domain invariant, and leaving it at one boundary means the next
surface to grow a copy/move endpoint inherits nothing. ADR-068 had meanwhile established where such
rules belong.

Three further problems were in the same handlers.

*The Trash policy was written out three times* (`DELETE /entries/{id}`, the bulk delete, and the
single-entry move), and the copies disagreed. One resolved the Trash mailbox from
`AccountId::new()` — a **freshly generated identifier** — when the source mailbox could not be read.
No account has that id, so it owns no Trash, so the lookup could never succeed and the placement was
deleted outright instead of being recoverable. The other copy returned `404 Account` for the same
condition.

*A copy into the entry's own mailbox* reached the placement uniqueness index and surfaced as a `500`.
IMAP refuses it (ADR-050/ADR-051); HTTP did not.

*A `\Noselect` destination* was refused by IMAP and accepted by HTTP.

**Decision.** `MessageService` in `sandesh-core`, holding the placement and mailbox ports.

*One `usable_destination` check, three rules, before any write.* The destination must exist and be
able to hold mail; it must belong to the **same account** as the source; and it must not be the
mailbox the entry already occupies. Nothing below this layer can enforce the second:
`copy_entry`/`move_entry` read the source placement and allocate a UID on whatever target they are
handed, and no schema constraint relates the two accounts.

*A foreign account's mailbox is reported as `NotFound`, exactly as a nonexistent one is.* Carried
over from ADR-067 and now enforced in the domain: distinguishing them would confirm that a given
mailbox id exists somewhere the caller cannot see.

*`discard` resolves Trash from the placement's own account.* There is no fallback to "some account",
because there is no correct guess — the account is a property of the placement, reachable through its
mailbox, and if that mailbox cannot be read the operation fails rather than inventing an owner. A
placement already in Trash is removed rather than moved into itself, which is what makes a second
delete permanent; an account with no Trash mailbox gets the placement removed, since a client is free
to have renamed it.

*The IMAP planners stay put.* `copy.rs`, `move_entry.rs`, `expunge.rs`, `store.rs` and `append.rs`
hold no ports — they compute plans and `[COPYUID]` values — so they are correctly placed protocol
logic (CORE_SPEC §9) and were not touched.

**Consequences.** ADR-067's fix is now structural rather than a check at three call sites; the HTTP
handlers keep only the translation from two request shapes to a `MailboxId` and the rendering of the
domain's refusals as the statuses they already documented. The generated contract is unchanged. 1723
→ 1733 tests.

Ten unit tests cover the service, and the two that matter most assert an *effect* rather than a
status: a refused cross-account copy leaves the other account's mailbox empty, and a refused move
additionally leaves the source placement intact — the assertion that would catch a check performed
after the write. Discarding is tested with **two accounts each holding a Trash**, which is what
distinguishes resolving the right one from resolving any one; the `AccountId::new()` bug is
reproducible as a mutation and fails exactly that test.

`bulk_delete_entry` lost its `entry` parameter: the service resolves the placement itself, so the
handler no longer repeats the lookup.

Still outstanding from the Phase 1 deliverable: `AccountService`, and moving `DeliveryService` into
the core.

### ADR-070 — Account creation goes through `AccountService`, and an account must be usable

**Context.** Accounts were created from three places, each building the `Account` itself: SMTP's
`auto_create` policy (`sandesh-smtp/src/delivery.rs`), `POST /api/v1/accounts`, and first-run
bootstrap (`sandesh-app/src/bootstrap.rs`). The three disagreed about what makes an account valid,
and two of the disagreements let the HTTP surface create accounts that cannot work:

*An account with no address.* Mail is routed by identity (`CORE_SPEC` §7), so an account with no
identity can never receive anything. The handler looped over `payload.addresses` and skipped blank
entries, but nothing required the list to contain anything — `{"username": "x"}` succeeded and
produced something inert. Succeeding and producing a useless result is worse than refusing.

*A second catch-all.* `AccountStore::catch_all` answers with a single account, so a second makes
routing depend on which one the store happens to return. `ensure_catch_all` assumed at most one
existed; the API let a client add another.

*A blank username* was accepted after `trim()`, giving an account that cannot be logged into and
cannot be told apart from another such account.

The username rule also differed for no recorded reason: SMTP used `address.normalized()` with a
documented rationale (so `a@x.test` and `a@y.test` do not collide), the API used the raw `trim()`,
and bootstrap a literal `"catch-all"`.

**Decision.** `AccountService::create` in `sandesh-core`, taking a `NewAccount`, and all three
callers go through it.

*Three invariants, enforced before anything is written.* A non-empty trimmed username; at least one
address unless the account is the catch-all; and no second catch-all. A duplicate username is a
conflict checked up front rather than a storage-index violation surfaced as a `500`.

*The catch-all is the deliberate exception to the address rule.* It is reached precisely because no
identity matched, so requiring one would make it uncreatable. `ensure_catch_all` is a method on the
service, so first-run setup now satisfies the same invariants as any other creation instead of
assuming them.

*The username is not required to look like an address.* Both existing conventions are legitimate —
SMTP's full-address form avoids cross-domain collisions, bootstrap's plain name is not an address at
all — so the service validates that a username *exists*, not what shape it takes.

*Password hashing stays outside the core.* Argon2 is a `sandesh-proto` primitive (ADR-032) and the
core declares no cryptography beyond `sha2` (ARCHITECTURE §3.2), so callers hash first and hand over
a `Credential`. `NewAccount.credential` is documented as already-hashed.

*Blank address entries are dropped rather than rejected.* A UI sending an empty row should not fail
the request; the service still refuses an account left with no address at all, which is the rule
that matters.

*Default mailboxes stay in `AccountStore::create`.* That provisions them in the same transaction as
the account row, so an account without an `INBOX` is never observable. Moving it into the service
would trade a transactional guarantee for a caller's discipline.

**Consequences.** One definition of a valid account. 1733 → 1746 tests: nine unit tests on the
service and four over HTTP.

One honest limitation, recorded because the test looks stronger than it is: the second-catch-all
rule is defended **twice** — by the service and by `idx_accounts_catch_all`, a partial unique index
in migration 0001. Removing the service check still yields a `409` from storage, so the HTTP test
does not isolate it. That test is kept as a guard on the observable contract, and the service's own
check is isolated by the unit test, which runs against a double with no such index and does fail
when the check is removed. The no-address rule has no storage backstop and its HTTP test fails as
soon as the check is mutated away.

Separately, `sandesh-app` no longer declares a `[[bin]]`. Both it and the workspace-root package
defined a binary named `sandesh` from two-line, identical entry points, so cargo warned that they
collide on `target/debug/sandesh` and said it may become a hard error
(rust-lang/cargo#6313). The root package keeps the binary, because a bare `cargo run` from the
repository root is the command the README documents; nothing referenced the inner one.

This completes the Phase 1 service deliverable except for moving `DeliveryService` into the core,
which needs `UnknownRecipientPolicy` mirrored as a domain type — `sandesh-config` is not a permitted
core dependency.

### ADR-071 — `DeliveryService` moves into the core, and the routing policy is mirrored as a domain type

**Context.** `DeliveryService` was the only service that existed before this work, and it lived in
`sandesh-smtp`. Nothing in it is SMTP-shaped: `resolve` implements the `unknown_recipient` policy of
`CORE_SPEC.md` §7, `inbox_of` resolves a mailbox by name, and the placement deduplication is §7's
rule that two recipients landing in one mailbox produce one entry. Its own module doc called it "the
SMTP → core boundary", which was accurate about where it sat and wrong about what it contained.

The cost was not theoretical. Because it lived in a protocol crate, no other surface could reach it —
`sandesh-api` does not depend on `sandesh-smtp` — so the HTTP injection endpoint could not route a
message the way SMTP does without a second implementation of §7.

Moving it required resolving a dependency conflict. `DeliveryService::new` took
`sandesh_config::UnknownRecipientPolicy`, but `ARCHITECTURE.md` §3.2 fixes the core's dependencies
and configuration is not among them — `tests/architecture.rs` fails the build if it is. The reverse
is equally closed: `config_is_dependency_free` forbids `sandesh-config` from depending on
`sandesh-core`, because configuration is read by every layer and anything heavy there propagates
everywhere.

**Decision.** Mirror the enum, and convert at the composition root.

*`sandesh_core::UnknownRecipient` is a domain type.* `CORE_SPEC.md` §7 names these three outcomes
and states the routing rules in terms of them, so the choice is domain vocabulary that happened to
live in the config crate. `sandesh-config` keeps its own enum — it is what a configuration file
parses into — and `sandesh-app` converts between them in `routing_policy`. That function is the one
place in the tree that sees both crates, which is what a composition root is for. The `match` is
exhaustive, so a variant added on either side fails to compile rather than drifting.

Two definitions of three variants is duplication, and it is the lesser cost: the alternative is
either a core that knows about configuration or a config crate that pulls in the domain, and both
are rules the architecture test enforces for reasons that outlive this change.

*`credential_hash` now returns `CoreResult<Option<String>>`.* It previously logged a storage failure
and returned `None`. The core has no `tracing` dependency, so rather than lose the diagnostic the
service reports the failure and the SMTP session logs it. The behaviour is unchanged — a database
that cannot answer is not permission to proceed, and the adapter still treats the error as "no
credential" — but the adapter can now say so in the log, and fail-closed is visible in the type
rather than buried in a match arm.

*`sandesh-smtp` re-exports the moved types.* Existing callers and tests keep one import path, and
the SMTP session remains its principal user.

*The services are constructed once, at the composition root.* They were built per call site while
being introduced, which is cheap — each is an `Arc` clone — but lets two handlers assemble services
over different sets of ports, which is the drift the layer exists to prevent. `Application::start`
now builds a `DomainServices` and hands it to `ApiState`. `Repositories` was extracted at the same
time: `start` had grown past the 100-line lint, and the four repositories are always built together
over one pool and one event recorder, so grouping them is what the code wanted anyway.

**Consequences.** Every domain service now lives in `sandesh-core`, which completes the Phase 1
services deliverable. 1746 tests, unchanged by the move itself — the point of moving a type across a
crate boundary is that nothing observable changes, and the suite says nothing did.

`ApiState` gains three service fields alongside the ports. It keeps the ports because most handlers
read through one directly and only the operations carrying domain rules go through a service; its
module doc's claim to hold "the domain ports and the event bus — and nothing else" is updated
accordingly. The architecture test that forbids `sandesh-api` from reaching `sandesh-storage` is
unaffected, because the services are core types.

Three test harnesses build `DeliveryService` by hand and needed the same conversion; they share one
`routing_policy` helper in `sandesh-smtp/tests/common` rather than three copies.

### ADR-072 — The fixture corpus reaches its specified size, and the specification is now enforced

**Context.** `TESTING_STRATEGY.md` §4 names five fixture categories and `ROADMAP.md` Phase 1
requires "≥100 `.eml` fixtures across the five categories". The corpus held **22 across three**:
`adversarial/` and `real_world/` did not exist.

The shortfall mattered more than a missing deliverable, because several acceptance criteria are
phrased *"for every fixture in the corpus"*. Phase 5's `BODYSTRUCTURE` check is the clearest case:
it was true as written and ran against 22 messages, so the criterion was passing because the corpus
was small. The same applies to the byte-fidelity and never-panic assertions in
`crates/sandesh-storage/tests/raw_fidelity.rs`.

A second problem made the shortfall self-perpetuating. The two corpus walkers disagreed:
`sandesh-imap`'s recursed from `testdata/`, while `sandesh-storage`'s iterated a **hardcoded list of
three categories**. Fixtures added to a new directory would have been silently ignored by every
storage-level corpus test — the corpus could have grown without being tested.

**Decision.** Build the corpus to specification, and make the specification a test.

*101 fixtures across all five categories* — `valid/` 26, `malformed/` 29, `encodings/` 18,
`adversarial/` 16, `real_world/` 12. They were written to probe behaviour the code actually has
rather than to reach a number: `adversarial/` exercises the `MAX_DEPTH` guard at 64 and beyond,
boundary collisions, 500 sibling parts, a header-injection attempt in an encoded word, and a CID
reference cycle; `encodings/` completes the charset list §4 names (Shift_JIS, UTF-7, KOI8-R, Big5,
windows-1252) plus RFC 2047 B- and Q-words; `real_world/` carries the anonymised output shapes §4
asks for (Nodemailer, PHPMailer, Django, Rails, Outlook, Gmail, a bulk sender) and three shapes a
mail debugger meets constantly — a DSN bounce, a DKIM/SPF/DMARC-authenticated message, and a
SpamAssassin-scored one. The last two are fixtures Phase 7 A3 will need.

*`common::CATEGORIES` replaces the hardcoded list*, and is named rather than discovered: a category
deleted or renamed now fails the build instead of quietly shrinking every corpus assertion.

*A test asserts the contract.* `the_corpus_meets_the_size_and_shape_the_strategy_requires` checks the
≥100 count and that every category is represented. Without it this ADR closes a gap that can reopen
by attrition; dropping a category from `CATEGORIES` fails it, which is verified.

**Consequences.** 1746 → 1747 tests, and the corpus criteria now mean what they say.

**Expanding the corpus immediately found a real defect**, which is the argument for having done it.
`valid/attachment-filename-utf8.eml` has an RFC 2047 filename, which the server correctly emits as
an IMAP **literal** (`{n}CRLF` + payload) because it cannot be quoted. The
`bodystructure_is_well_formed_for_every_fixture_in_the_corpus` test took the untagged reply with
`.lines().find(...)`, so it kept only the text **before** the literal's CRLF — discarding the
payload and the closing parens — and reported a correct response as having four unclosed
parentheses. Two fixes: the test now takes the whole reply from `fetch_replies`, which already steps
over literal payloads, and `assert_balanced_parens` skips literal payloads itself, since a `(` in a
filename is data, not structure. The scanner still fails on genuine imbalance, verified by feeding
it an unbalanced reply.

That defect had been latent since Phase 5. No existing fixture produced a literal in a
`BODYSTRUCTURE`, so nothing exercised it — precisely the blind spot a 22-fixture corpus leaves.

One fixture was resized during this work: `valid/large-plain-body.eml` started at 332 KB, above the
256 KiB `INLINE_THRESHOLD`, so it needed a blob store the shared corpus harness deliberately does
not have. It is now ~200 KB — large enough to exercise a big inline body, below the threshold. The
above-threshold path already has its own test with a real `FilesystemBlobStore`.

### ADR-073 — The fuzz harness is built outside the workspace, and its assertions live outside the macro

**Context.** `TESTING_STRATEGY.md` §9 calls fuzzing "non-negotiable for components consuming
untrusted input" and names five targets. None existed: there was no `fuzz/` directory for any
parser, which `ROADMAP.md` carried as gap 3 across Phases 1, 2 and 5. Three separate notes in the
docs deferred it with the same reasoning — the harness is shared infrastructure, not one phase's
work — which was true and had stopped being a reason.

Two constraints shape the answer, and neither is about fuzzing:

*A fuzz crate inside the workspace would break an existing test.* `tests/architecture.rs` walks the
**resolved** dependency graph from `cargo metadata` rather than reading manifests, precisely so a
transitive edge cannot hide. A member crate depending on `libfuzzer-sys` and `arbitrary` puts both
into that graph, and the targets depend on `sandesh-core` directly.

*libFuzzer needs nightly.* The `rust` CI job runs `cargo check --workspace --all-targets` on stable,
which would try to build the targets and fail.

**Decision.** `fuzz/` is a standalone package, excluded from the workspace in both directions — an
empty `[workspace]` table in its own manifest and `exclude = ["fuzz"]` in the root — so the reason
is stated wherever a reader arrives.

*The five targets are `mime_parser`, `smtp_command`, `imap_command`, `pop3_command` and
`search_query`*, matching §9's table exactly.

*Each target's body is a function in `fuzz/src/lib.rs`, not code inside `fuzz_target!`.* This is the
decision worth the most. The binaries only compile on nightly, so assertions written inline in the
macro would be compiled by nothing in per-PR CI and would rot silently between nightly runs — the
same failure mode as the untested deliverables this whole audit found. As ordinary functions they
are typechecked on stable and, more usefully, replayable over a fixed corpus.

*`fuzz/tests/corpus.rs` does that replay on every commit*, wired into the existing `rust` job
because `cargo test --workspace` cannot reach an excluded package. It runs all 102 `testdata/`
fixtures through the MIME checks, a seed list of historically parser-breaking shapes through the
four command parsers, and **every committed crash reproducer** under `fuzz/artifacts/<target>/`.
That last part is the regression loop §9 asks for: a crash found at 3am becomes a committed
artifact, and from then on it is guarded by the ordinary stable test run rather than only by the
nightly schedule.

*Fuzzing itself runs on a schedule, not per-PR*, one matrix leg per target with `fail-fast: false`.
A time-boxed search is not a pass/fail gate on a fixed input; gating a merge on it would make merge
time depend on how long the fuzzer felt like looking. The corpus is cached between runs so coverage
accumulates.

**The targets assert more than "did not panic."** A target that calls a parser and drops the result
gets never-panics for free and finds only crashes. Each of these states the property §9 names for
it:

- **MIME** — "never loses bytes". The parser describes a message by `ByteRange` over the input, and
  the Web UI's MIME-tree ↔ raw linkage indexes with those ranges, so a range past the end of the
  input would not fail in the parser but later, in a slice, far from the input that caused it. The
  target checks every header and part range is `is_within` the input and actually slices it, and
  that no projected text, HTML or attachment path names a part that does not exist.
- **IMAP** — the literal path. `parse_line` is only half of it: a command carrying a literal stops
  and returns a `PendingLiteral` the session resumes after reading the octets, so a target stopping
  at `parse_line` would never reach `resume`, `push_token`, or the nesting and token bounds. The
  target resumes every pending literal, and asserts the declared length was bounded *before*
  allocation, including against a zero cap.
- **Search** — plan depth. `parse_filter_query` is infallible by design, because the UI's filter box
  must never refuse to search, so there is no `Err` for a bad input to produce. Unbounded nesting
  would therefore surface as a stack overflow in the evaluator's recursion or the generated `Drop`,
  which no `Result` can express. The target bounds the depth of the returned tree, computing that
  depth iteratively — recursing there would overflow on exactly the input the assertion exists to
  catch.

**Consequences.** Gap 3 closes. 1747 → 1750 tests (the three in `corpus.rs`). No crash was found in
the initial runs: 20,000 iterations against each command parser and a shorter run against the
heavier MIME target, all clean, which is the expected result for parsers that already had
property tests and is evidence the harness works rather than evidence it is unnecessary.

`crates/sandesh-pop3/tests/parser_robustness.rs` stays. Its 40,000 seeded cases are a floor that
runs on every commit with no nightly toolchain, and it explores only the shapes its generator was
written to produce, where the fuzzer is coverage-guided. Phase 6's deferral of a POP3 fuzz target
is withdrawn.

### ADR-074 — The search index is a prefilter that may never be wrong, and the specified tokenizer would have been

**Context.** `ROADMAP.md` gap 4 and `STORAGE_DESIGN.md` §7 called for an FTS5 index behind a
`SearchIndex` trait. Neither existed. The gap had stood with a recommendation to strike it — search
already works as a pure domain plan, and an index adds no capability — and the ruling was to build
it instead.

Building it surfaced a problem the design had not: **the specified tokenizer makes the index
unsound.** §7 called for `tokenize = 'unicode61'`, which splits text into words. RFC 9051 §6.4.4
defines the IMAP text keys as **substring** matches. Those are not the same question. The body
`xfoobar` contains the substring `foo`, but has no token beginning `foo`, so a word index excludes
it from `SEARCH BODY "foo"` while `SearchQuery::matches` includes it.

That is a **false negative**, and it is the only error class that matters here. A false positive
costs one wasted message load and is then discarded by `matches`. A false negative is mail that
exists and cannot be found — and it fails silently, because a search returning fewer results is
indistinguishable from a search whose criteria matched less. It would have passed every test that
did not specifically look for it, which is the failure mode this whole audit keeps finding.

**Decision.**

*`tokenize = 'trigram'`.* It indexes every three-character sequence and matches substrings
case-insensitively — precisely RFC 9051's semantics. Verified against the real SQLite before
committing to it (3.51.3, bundled by `sqlx`). Its costs are a larger index and a three-character
minimum, and queries shorter than that fall back to a scan.

*The trait returns `SearchCandidates`, not `Vec<MessageId>`.* Two variants: `Candidates(set)`, a
**superset** of the matches, and `Unsupported`, meaning "scan everything". This is the load-bearing
decision. A bare `Vec` would represent "nothing matched" and "I could not tell" with the same value,
so every future case the index cannot handle would degrade to an empty result rather than to a
scan — turning an optimisation into silent data loss. As an enum, the safe answer is the one the
type system makes easy: `Unsupported` is always correct, and `admits()` returns `true` for
everything when narrowing is impossible.

*The index declines wherever it cannot guarantee a superset:* values shorter than a trigram, fields
not stored in their own column (`Bcc`, `Header(_)`, `Text`), any `Not` — complementing a superset
yields a *subset*, exactly the forbidden direction — an `Or` with even one unnarrowable branch, and
any state where a message is missing from the index at all.

*`NullSearchIndex` ships alongside it,* implementing the trait by narrowing nothing. It is not a
stub: it is the proof that the index is optional, and it is what the in-memory doubles use.

*Maintenance happens inside the message's own transaction* (`index_in_tx` / `remove_in_tx`), so
"stored" and "indexed" are one event. Without that, every delivery would briefly leave an unindexed
message, and the "decline while anything is unindexed" rule would make the index permanently inert.

*Indexed text is the **decoded** body over every non-multipart part* — the same set and form
`search::body_contains` compares against. An earlier draft indexed raw byte ranges, which would have
stored base64 while `matches` read decoded text: sound-looking, and wrong for most real mail.

**Consequences.** 1747 → 1761 tests. The whole existing suite passing unchanged is the strongest
parity evidence available: every IMAP `SEARCH` conformance test and every REST search test now runs
with the index live and returns exactly what it did before.

The dedicated suite asserts the contract directly — parity between indexed and scanned results over
the fixture corpus across every field and several boolean shapes, the mid-token case specifically,
decoded-body indexing, automatic maintenance on delivery and orphan collection, and that a stale
index declines rather than answering from partial data. A cross-protocol test proves REST and IMAP
agree with the index live in the assembled application.

Two of those tests were verified by mutation rather than trusted: reverting the tokenizer to
`unicode61` fails `a_mid_token_substring_is_never_missed` and `boolean_combinations_keep_parity`
with the exact false negative described above, and indexing raw bytes instead of decoded ones fails
`an_encoded_body_is_indexed_as_decoded_text`. A test that also asserts the index *actually narrows*
guards the opposite failure: an index that returned `Unsupported` for everything would satisfy every
parity test while doing nothing.

**The honest summary of the benefit.** This removes the full-mailbox message load for text searches
that the index can narrow, which was the real cost (`GET /search` previously loaded every message in
every scoped mailbox). It does not make search asymptotically cheaper in general, because the
unnarrowable cases still scan. The gap was arguable as a strike for exactly that reason; built, it
is a genuine improvement with no semantic change, which is the only form it could safely take.

### ADR-075 — The API authenticates, and the middleware goes on the whole router rather than the routes

**Context.** `ROADMAP.md` gap 6, and Phase 4.5's one unstruck deliverable. The HTTP API was
unauthenticated and served over plaintext regardless of `SANDESH_SMTP_AUTH` — a setting whose name
invited the opposite conclusion, and which governs SMTP alone. Acceptable on loopback, where Sandesh
binds by default; a real hole the moment the listener is bound anywhere else.

Two constraints shaped the design before any code was written.

*`sandesh-api` may not depend on `sandesh-proto`.* `tests/architecture.rs` enforces it (§59: the API
and the mail protocols are siblings and must not see each other), and Argon2id verification lives in
`sandesh-proto`. The choice was to reimplement verification inside the API or to declare the
capability as a port. Reimplementing would have put a second password-checking path in the codebase,
which is how one of them ends up without the timing-safety property the other has.

*`axum::serve` owns its own TCP accept loop* and cannot wrap a stream in a TLS handshake, so HTTPS
meant writing the accept loop out.

**Decision.**

*Authentication is a `CredentialVerifier` port* in `sandesh-core`, implemented by `sandesh-app` — the
one crate permitted to see both sides. It carries `verify` **and** `hash`, together rather than in
two traits, because they must agree on algorithm and parameters.

*Two schemes.* Bearer against a config-supplied token (`SANDESH_API_TOKEN`); basic against the
`accounts` table through the port, which reaches `verify_optional` and therefore performs the hash
comparison even for an unknown user (ADR-032). Token comparison is a constant-time accumulate rather
than `==`, which short-circuits at the first differing byte.

*Off by default* (ADR-005), and `None` rather than a mode when disabled, so the middleware's first
check cannot misread a mode as permission. `/info` now reports `api_auth` and `api_tls` separately
from `smtp_auth`, because the conflation of the two was the original problem.

*Only `/api/v1/health` is exempt*, as an exact path rather than a prefix — a prefix would have
exempted `/health/ready`, which reports storage state, and any future `/health/*` before anyone
reviewed it. `/info` is not exempt either: it describes the deployment.

*HTTPS reuses the existing certificate* (ADR-034). `load_tls` now counts the HTTP listener, without
which an HTTPS-only configuration would have loaded no identity and been handed no acceptor. A TLS
mode with no usable identity fails startup rather than serving plaintext on a port the operator
believes is encrypted — the rule Phase 4.5 already applied to SMTP.

**Building it found two real defects, which is the argument for having built it rather than argued
about it.**

*The middleware was on `route_layer`, and the Web UI bypassed it entirely.* `route_layer` runs only
for requests that **matched a route**, and the SPA fallback matches nothing — so every path the API
does not claim, `/` included, skipped authentication. A server that returned 401 for `/api/v1/messages`
and 200 for the UI is not an authenticated server. `layer` fixes it, and
`the_web_ui_is_protected_too` is the test that found it and now pins it.

*`sandesh-api` already had its own Argon2 configuration.* `hash_account_password` in `routes.rs`
constructed `Params::new(19 * 1024, 2, 1, None)` by hand — the canonical constants from
`sandesh-proto`, copied by value. They matched, so nothing was broken today; they would have stopped
matching the first time the canonical ones were retuned, silently, because nothing compared them.
This is precisely the "second security implementation" the architecture rules exist to prevent, and
it had been sitting there since account creation was built. It now goes through the port, `argon2`
is no longer a dependency of `sandesh-api`, and a new test —
`the_api_does_not_duplicate_credential_verification` — fails if it comes back.

**Consequences.** 1766 → 1779 tests. Gap 6 closes, and Phase 4.5 is complete by deliverables as well
as by acceptance criteria. Phase 7 A6 (log stream) was gated on this and is now merely unstarted.

`tests/api_auth.rs` drives a real application over real sockets, because the subject is a middleware
layer and a TLS accept loop — a handler-level test would exercise neither. The assertions are
deliberately about what an *unauthenticated* caller gets: a suite that only proved the right
credential works would pass against a server that accepts everything.

Two were verified by mutation. Making the middleware allow everything fails three tests; accepting a
token prefix, as a naive `starts_with` would, fails `a_wrong_or_malformed_credential_is_refused`.

**Not built, deliberately.** No client-certificate verification, and no CLI or UI for managing
account passwords — one is still set through `POST /api/v1/accounts`. No token lifecycle: tokens come
from configuration, so there is no migration, no storage and no revocation surface. Issued tokens
would be a larger deliverable than any phase asks for, and the config token plus per-user basic auth
covers both audiences the API has.

### ADR-076 — `--no-web` is struck rather than built

**Context.** `ROADMAP.md` Phase 3 listed "`--no-web` runs API-only" as an acceptance criterion, and
the audit behind §Outstanding carried it as gap 5. It was never built. Unlike the other five gaps it
was recorded with a recommendation to strike rather than a plan to close, and that recommendation
sat unresolved while gaps 1 to 4 and 6 were built.

Leaving it unresolved was itself the problem: an open row in that table reads as work someone will
eventually do, and it was the only thing standing between the table and being finished.

**Decision.** Struck. No flag, no configuration setting, no API-only mode.

Three reasons, in the order they matter:

*No consumer.* Nothing and nobody has asked for it. The criterion was written when the UI's cost was
hypothetical; it is now known, and it is nil.

*The UI costs nothing to leave in place.* It is embedded at compile time by `rust-embed` and served
only from the router's **fallback** — the path taken when nothing else matched. An API-only
deployment already gets API-only behaviour from every request it actually makes. Since ADR-075 the
fallback is also behind authentication, so "the UI is reachable" no longer even means "unauthenticated
HTML is reachable".

*A literal flag would cost more than it looks.* `crates/sandesh-app/src/cli.rs` parses **no command
line arguments at all** — its own module documentation calls it "deliberately thin", and
configuration arrives through the environment (OD-24). Adding `--no-web` means introducing argument
parsing: a parser or a `clap` dependency, `--help`, unknown-argument handling, and a precedence rule
for when a flag and an environment variable disagree. That is a second configuration system, and the
next setting would want a flag too.

**The cheap version was measured, not guessed.** `SANDESH_WEB_ENABLED=false` would follow the
existing `http_enabled` pattern exactly — four touch points, one conditional on the single
`.fallback(assets::serve)` line, and roughly 80 lines with tests and documentation. It was the
smallest item left in the table by a wide margin. It is struck despite being cheap, because cheap is
not a reason to build something nothing needs, and because it would not have satisfied the
criterion's actual wording anyway.

**Consequences.** §Outstanding before Phase 7 is fully resolved: five gaps built, one struck. No code
changed. If an API-only consumer ever appears, the criterion is reinstated and the environment
variable is an hour's work — this ADR is the record of why it is absent, so its absence is not
rediscovered as an oversight.

### ADR-077 — Traces are retrievable, and "tracing is off" is not an empty list

**Context.** Phase 7 A2's first half captured protocol transcripts for SMTP, IMAP and POP3
(ADR-066) and stored them. Nothing could read them back: `sandesh-api` had no route, and `ApiState`
did not even hold the `ConnectionStore`. The phase's acceptance criterion says every connection
produces a **retrievable** trace, and it was passing on the strength of
`every_protocol_records_a_connection_trace` — a test that opens the SQLite file directly. That test
proves capture. It cannot prove retrieval, because it bypasses the only surface a user has.

**Decision.**

*Two endpoints,* `GET /api/v1/connections` and `/connections/{id}`, mirroring `/deliveries`: newest
first, a limit, no cursor. `ConnectionStore::recent` takes a limit and offers no cursor, so
advertising one would be a promise the store cannot keep.

*The transcript ships with the list row,* not behind a second fetch. It is the only field anyone
opens the view for, and it is already capped at capture time.

*`?protocol=` filters at the store,* and an unrecognised value is a `400`. Returning an empty list
for `?protocol=smtpp` would read as "no SMTP connections yet" to someone who simply mistyped.

*Tracing disabled is `503`, not `200 []`.* This is the decision worth the most. The store is
`Option` — `SANDESH_TRACING_ENABLED=false` is a supported configuration — and an empty list would be
indistinguishable from "tracing is on and nothing has connected yet". Those need different answers,
because the person reading is usually asking *why their trace is missing*. The `503` carries the
variable to set.

*No redaction on the read path.* Capture already strips credentials and reduces message bodies to a
byte count (ADR-066). Redacting again here would move the guarantee to the read path, where a second
reader — a future export, the A6 log stream — would not inherit it. A cross-protocol test asserts
the envelope survives the trip out and the body does not.

**The view renders the transcript as a directional exchange.** Capture writes `C:`, `S:` and `*`
markers, so direction is recorded data rather than something the UI infers. The markers are stripped
into a gutter and the client lines are tinted, which makes an unanswered command — the shape of most
protocol bugs — visible as a break in the alternation rather than something to read for. Everything
else in the view is deliberately quiet and built from the existing tokens: this is a new view inside
a mature system, so the work is coherence, not a new identity.

**Two defects were found by looking at it rather than by reasoning about it.**

*The panes did not fill their container.* `.shell__main` is a flex row, and `.traces` sized to
content, so the split collapsed and the transcript scrolled inside a letterbox. Visible in one
screenshot; invisible in every test.

*"Tracing is off" fell through to "No connections yet".* The first draft detected the 503 with
`error.message.includes('503')`, which never matched — `ApiError` carries a `status` field and puts
the server's prose in `message`. The view therefore told a developer with tracing disabled to "send
mail or connect a client", advice that would never produce a trace. Now read from `ApiError.status`,
with the server's own `detail` shown rather than a paraphrase that could drift. The regression test
fails if the string-matching version returns.

**Consequences.** 1780 → 1781 Rust tests and 118 → 123 frontend tests. A2 is complete and the Phase 7
criterion is met end to end: one test reads the store, a second fetches the same traces over HTTP.

One thing found and deliberately not fixed: `shell__header` overflows to 532px at a 375px viewport,
on **every** route including ones this work never touched. Pre-existing, out of A2's scope, and
recorded here so it is not rediscovered as something this increment introduced.

---

### ADR-078 — Header analysis parses and attributes, and never verifies

**Status.** Accepted. Phase 7 A3.

**Context.** The Phase 7 criterion asks that "DKIM signatures are parsed and displayed with their
fields; verification is explicitly not claimed". The second clause is the harder one. A developer
reading `dkim=pass` in a mail tool will reasonably conclude their production mail will pass, and
Sandesh has no basis for that claim: it holds no public keys, performs no canonicalisation, and
checks no policy. A header analyser that looked like a verifier would be actively misleading.

**Decision.** `sandesh-core::analysis` turns the flat header list into a `Received:` hop timeline
with per-hop delay, parsed DKIM tags, `Authentication-Results` entries, `Received-SPF` and
`X-Spam-*`, exposed at `GET /api/v1/messages/{id}/headers/analysis`. The flat `/headers` endpoint is
untouched.

Nothing is verified, and three separate mechanisms keep it that way rather than one comment:

1. **The types are named for what they hold, not for a verdict.** `AuthResult::result` is the
   literal text an upstream server wrote, and the field documentation says so.
2. **The disclaimer is part of the payload.** Any client rendering these fields carries the caveat
   with them; it is not something the UI may forget to add. A test strips the disclaimer and asserts
   no remaining byte of the response claims `verified`, `authentic` or `trusted`.
3. **The UI never colours a result.** No green, no red, and the wording keeps the speaker visible —
   "mx.example reported pass", not "pass". A green tick is the single most misleading thing this
   panel could show, so the panel has no success state to show.

**Analysis is total.** Every input produces an analysis; a malformed `Received:` yields a hop
carrying its raw text rather than a dropped hop, because the hop that defeats the parser is usually
the interesting one. A test runs the whole 101-fixture corpus — the malformed and adversarial
messages included — and asserts no panic and no delay without the timestamps that define it.

**A negative hop delay is reported, not clamped.** Relay clocks disagree, and that is exactly the
kind of fault a hop timeline exists to surface.

**Three real defects were found while building it, none of them in A3's own scope.**

*Every `Received:` header decoded to the empty string.* `mail-parser` classifies `Received:` as a
structured value, and `decode_header_value` mapped that arm to `String::new()`. The flat headers
view had therefore been showing a **blank value for every hop** since it was written, unless the
reader ticked "show raw values". Nothing failed, because no test asserted that a header's decoded
value was non-empty. The unfolded raw bytes are now used for any header the parser gives no text.

*The Content-Security-Policy script hash was a wrong literal, and the entire UI could not boot.*
`script-src` carried a hand-copied `sha256-...` that did not match the bundle, so the browser
refused to execute Vite's inline bootstrap script and the console mounted nothing at all. A literal
cannot survive a frontend rebuild. The hash is now derived from the embedded `index.html`, and a
test hashes the file independently — deliberately not by calling the same function, which would let
a bug agree with itself — and asserts the policy allows it.

*`useKeyboardShortcuts` looped until React stopped rendering.* Callers pass an array literal, so its
identity changed every render, while `registerShortcut` publishes provider state: register,
re-render, register again. React aborted with "Maximum update depth exceeded" and stopped
committing, so **clicking any mailbox changed the URL while the view never updated**. The effect now
keys on a content signature of the shortcut descriptors and dispatches handlers through a ref, so a
handler is never stale and its identity never re-registers.

**Consequences.** 1804 → 1808 Rust tests, 145 → 148 frontend tests. One fixture added
(`malformed/broken-received-chain.eml`), so the corpus is 102. A3 is complete and its criterion is
met: the fields are displayed, and no response, label or colour claims verification.

### ADR-079 — Metrics are derived from storage, and the scrape is authenticated

**Status.** Accepted. Phase 7 A4.

**Context.** The roadmap lists a Prometheus metrics endpoint as a Phase 7 deliverable. It carries no
acceptance criterion of its own, and the Phase 7 plan marks it "thin by intent: the counters the
deliverable implies, not a general instrumentation layer". Two questions had to be settled before
writing any of it: where the numbers come from, and who may read them.

**Decision — the numbers are read from storage, not accumulated in memory.** No client library and
no global registry. A registry is process-global mutable state reachable from every crate, which is
the coupling `ARCHITECTURE` §12 exists to prevent; worse, the figures a scrape reports are already
in the database, so in-memory counters would be a *second source of truth* that can disagree with
the first — and after a restart, silently would. A new `sandesh-core` port, `MetricsSource`,
returns one `MetricsSnapshot`; `SqliteMetricsSource` answers it with aggregate SQL.

One port rather than count methods added to `MessageStore`, `MailboxStore`, `DeliveryStore` and
`ConnectionStore`. Those four describe how a *single* entity is read and written; "how many of
everything are there" is a different question, and answering it through them would mean changing
four traits and every test double that implements them to serve one endpoint. The alternative — the
API iterating mailboxes and summing — would make a scrape O(mailboxes), reintroducing the linear
scan A1 had just removed from the delivery lookup.

The snapshot is read inside one transaction. Four independent queries can interleave with a
delivery in flight and report a message count that never existed at any instant.

**Decision — `/metrics` is authenticated, with no exemption.** It sits at the bare path scrapers
default to, outside `/api/v1`, beside `/api/openapi.json`. But it is *not* added to
`EXEMPT_PATHS`. ADR-075 already declined to exempt `/info` and `/health/ready` because they
disclose deployment state; a scrape discloses strictly more — message counts, mailbox counts,
delivery volumes and connection activity. Liveness is exempt because a container orchestrator has
nowhere to put a credential and discloses only that the process runs. A Prometheus scraper is not
in that position: it carries a bearer token in its scrape configuration, so protecting this costs
an operator one line. A test asserts the refusal, and fails if `/metrics` is ever added to the
exemption list.

**Decision — seconds, not microseconds.** `duration_us` is stored in microseconds; the histogram
reports seconds, because Prometheus convention is base units and a dashboard that must know this
one server disagrees is a dashboard that will eventually be wrong. Bounds are rendered at fixed
six-decimal precision: a scraper matches a bucket bound as *text*, so a bound that formats
differently between scrapes reads as a different bucket.

**A defect this found before it shipped.** The first draft counted client commands by counting
occurrences of the `C:` marker in a stored transcript. `Transcript::line` renders the client's own
bytes *after* the marker, so `MAIL FROM:<user@C:host>` would have reported two commands where one
was sent — a wrong number that parses perfectly and is invisible until someone acts on it. The
count now keys on the marker's position at the start of a line, and the test that pins it uses
precisely that address.

**Consequences.** 1807 → 1824 Rust tests. Every series is emitted even when empty, so a dashboard
built against a busy instance does not break against a fresh one, and `rate()` does not see a
series spring into existence mid-scrape. The endpoint is absent from `docs/openapi.json` by
design: it is outside the versioned prefix and returns Prometheus text, not JSON. Verified against
a running server over real SMTP, IMAP and POP3 traffic. Per-endpoint HTTP histograms and
event-bus lag gauges remain Pass B.

### ADR-080 — The API console is built, not imported, because of the CSP

**Status.** Accepted. Phase 7 A5.

**Context.** The Phase 7 criterion is "the API console can execute every documented endpoint".
the Phase 7 plan left the library open: "needs a dependency choice (Scalar / Redoc / Swagger UI)
checked against the frontend CSP before committing to one". That check is what decided it.

**The CSP rules out the obvious answers.** `crates/sandesh-api/src/assets.rs` serves
`default-src 'self'` with no CDN allowance and `script-src 'self'` with no `unsafe-eval` — and a
test asserts `unsafe-eval` never appears, because this origin holds real mail and an injected
script that can `eval` is the difference between a sandbox and a foothold.

- **Swagger UI requires `unsafe-eval`.** Excluded outright; adopting it means deleting a security
  test to accommodate a developer convenience.
- **Any CDN-loaded bundle is blocked** by `default-src 'self'`. Allowing one would mean trusting a
  third-party origin with script execution next to the mail store.
- **Redoc's try-it console is a paid feature**, so it renders a reference and leaves the criterion
  unmet — a separate console would be needed regardless.
- **Scalar bundled into Vite** was viable but adds ~1–2MB to an already-873kB bundle and still
  needs its own CSP review for workers and styles.

**Decision.** The console is built from the design system already present: `web/src/api/openapi.ts`
reads the contract, `web/src/routes/ApiRoute.tsx` renders it. No new dependency, no CSP change.
Verified in a real browser against a running server with **zero console errors and no CSP
violations**.

**The operation list is derived, never written down.** It is built by flattening the document
fetched from `/api/openapi.json` *on the running server*, not the repository copy. CI pins the two
together, but only one of them is what the server in front of you is serving, and a console that
documents a different build than it calls is confidently wrong. An endpoint added to the Rust API
appears in the console with no edit to the frontend — which is the only way "every documented
endpoint" stays true after today. All 40 operations were listed and executed in a live browser.

**The bearer token is held in memory and never persisted.** The UI shares an origin with the API,
so a token in `localStorage` is a credential at rest readable by anything running on that origin.
The field is `type="password"`, so it is not readable over a shoulder or a screen share, and a
test fails if the value ever reaches browser storage. Re-typing after a reload is the right price.

**Two defects found while building it, both caught by tests rather than by eye.**

*Every nullable field would have been mislabelled.* OpenAPI 3.1 is JSON Schema 2020-12, where a
nullable string is `type: ["string", "null"]` rather than a `nullable` flag. 63 schemas in this
contract use that spelling, and reading `type` as a bare string labelled every one of them `any`
and seeded it `null` instead of `""`. A test asserting no contract field resolves to `any` caught
it, then caught a second spelling: a nullable *reference* is a `oneOf` of null and a `$ref`
(`MessageSummary.from`), which needed following too.

*Descriptions rendered as raw markdown.* The contract's descriptions are Rust doc comments, so 36
of 40 carry `# Errors` headings and backticked status codes. Rendered verbatim they showed a
literal `#`. `describe()` handles exactly those two constructs and leaves everything else as
plain text — unstyled prose reads fine, a half-rendered table does not, and a markdown library
would add a dependency and a sanitisation surface for two constructs.

**Consequences.** 148 → 191 frontend tests. No new dependency and no CSP change, so the security
posture is exactly what it was. The criterion is met: every documented endpoint is listed and
executable, demonstrated live by creating a message through `POST /api/v1/send` (201, two
placements) and fetching it back through `GET /api/v1/messages/{id}` (200).

### ADR-081 — The log stream copies the log rather than becoming it

**Status.** Accepted. Phase 7 A6. Completes Pass A.

**Context.** The last Phase 7 deliverable: a log stream with filtering. It was flagged early as
needing care, because streaming logs to a browser widens who can read them — a concern ADR-075
resolved by making the API authenticable, which is why A6 was unblocked rather than dropped.

**Decision — the capture layer is additive.** A `tracing` `Layer` copies each event into a bounded
ring buffer *alongside* the existing fmt-to-stdout layer, rather than replacing it. A developer
running the binary sees exactly what they saw before; A6 adds a second reader, it does not move
the log somewhere else.

**This adds no new disclosure, and that is the whole safety argument.** Message bodies, headers and
credentials never reach a log line in the first place (`logging.rs`, `SMTP_DESIGN.md`), so they
cannot reach the buffer. The guarantee is at the source, not on the read path — which matters
because a filter on the read path protects only the reader that implements it. A cross-protocol
test delivers a message with distinctive subject and body strings and fails if either reaches
`/api/v1/logs`; reverting the redaction (logging `content` at the delivery site) makes it fail,
which is how the assertion was shown to be load-bearing rather than decorative.

What does change is *who* can read the log: stdout needs the terminal, this needs an HTTP request.
So the endpoints sit behind the same authentication as everything else, with no exemption — the
stance A4 took for `/metrics` and ADR-075 took for `/info`.

**Decision — the buffer lives in `sandesh-events`.** `sandesh-app` installs the layer and
`sandesh-api` serves it, and neither may depend on the other: `sandesh-app` is the composition
root. `sandesh-events` already owns the bounded-broadcast primitive both need and is already
shared by exactly these two crates, so the buffer sits beside `EventBus` with the same properties
— a bounded ring, a per-subscriber queue, and a producer that never blocks. That last one is
load-bearing here for the same reason it is on the bus: capture runs inside `tracing`'s emit path,
which the SMTP path calls, so a capture that could block would let a stalled browser tab slow down
mail delivery.

The buffer is a process global, reached through `logging::buffer()`. `tracing` installs one
subscriber per process, so the layer is installed once and must outlive any single `Application`;
threading it through `Application::start` would have changed a signature twenty-one call sites use
in order to hand every one of them the same value.

**Decision — SSE, not `WebSocket`.** The stream is one-directional, so the client never needs to
send anything, and this mirrors the existing `/api/v1/events/sse` rather than adding a second
`WebSocket` surface — one whose resume story (OD-19) is still unbuilt. `GET /api/v1/logs` serves
the bounded backlog beside it, so opening the view shows history rather than an empty pane that
fills only when something next happens; an empty pane reads as "logging is broken".

**Filtering is done by the server**, on both endpoints. Filtering in the browser would look
identical on screen while streaming every line over the wire, which is the bandwidth the filter
exists to save. A level filter means *at or above* that severity, because that is what a reader
means by asking for warnings; an unrecognised level sorts highest rather than lowest, so a line
nobody can classify is shown rather than silently dropped.

**A simplification found by the linter.** The first draft hand-rolled the civil-date arithmetic to
avoid a dependency for one timestamp format. Clippy's pedantic cast warnings drew attention to it,
and the workspace already depends on `time` with formatting enabled — which `sandesh-api` already
uses for exactly this. Deleting the hand-rolled calendar removed about forty lines and one thing
that could disagree with the rest of the API about a leap year.

**Consequences.** 1824 → 1836 Rust tests, 191 → 202 frontend tests. The contract grows to 42
operations, so the A5 console lists and executes the new endpoints with no frontend change — the
derived-list design paying for itself one increment later. Verified against a running server:
lines captured from real startup and delivery, live SSE appending in the browser with no refresh,
filters narrowing correctly, no CSP violations, and neither probe subject anywhere in the buffer.
**Phase 7 Pass A is complete** and every acceptance criterion is met.

### ADR-082 — The coverage floor is measured and enforced, not merely written down

**Status.** Accepted. Closes Definition-of-Done items 4 and 5 for Phase 7.

**Context.** The ROADMAP's "Definition of done, every phase" has seven items. Phase 7 Pass A met
five of them on the strength of the increments themselves; two were outstanding and neither was
specific to Phase 7.

**Item 4 — coverage thresholds.** `TESTING_STRATEGY.md` has specified `cargo-llvm-cov` and a
**≥85% floor on `sandesh-core` and the protocol crates** since Phase 0. Nothing ever ran it. No
coverage tooling appeared in CI, in the workspace manifest, or anywhere else — so for seven
phases the phrase "coverage thresholds held" was asserted at each phase boundary and could not
have failed.

It was not a formality. The first measurement put **`sandesh-pop3` at 84.15%**, below its own
documented floor. The gap was concentrated in `session.rs` (77.25%), and the largest single piece
of it was `handle_auth` — the SASL failure paths: an unsupported mechanism, the cleartext refusal,
a wrong password, an unknown account, a cancellation, a malformed base64 payload. All ordinary
reachable behaviour a client can trigger, none of it defensive code worth excluding. Seven tests
now cover them and the crate is at 85.22%. Every gated crate passes; the workspace is at 89.82%.

Writing those tests turned up the ordering rule they had to respect: with authentication enabled,
`AUTH` is refused on a cleartext connection *before any credential is examined*. Three of the
tests were written against a cleartext socket and failed with "authentication not permitted on
cleartext connection" — the server being right and the test being wrong. That refusal is now
pinned by its own test, since it was an uncovered branch in its own right.

**Decision — enforce it in CI.** `scripts/coverage_gate.py` rolls `cargo llvm-cov`'s per-*file*
report up to the per-*crate* level the floor is written in, and fails naming **every** crate
below it rather than the first. It also fails on an empty or unparseable report: a gate that
reads a broken report as "no failures" is worse than no gate. All three failure modes are
verified by mutating the input rather than assumed.

The CI step runs under `set -euo pipefail`, because `cargo llvm-cov | tee` otherwise reports
`tee`'s exit code — a coverage run that failed outright would hand the gate an empty file instead
of failing the step. Confirmed by observing the pipeline return 0 for a failing left-hand side
before the fix.

**`sandesh-api` (82.90%) and `sandesh-app` (72.88%) are below 85% and deliberately outside the
gate.** The documented floor names the core and the protocol crates, and widening it here would be
inventing a requirement rather than enforcing one. `sandesh-app` is the composition root: much of
it is startup wiring exercised by the integration suite over real sockets, which line coverage
attributes to the test binary rather than to unit tests. Raising either is a reasonable future
decision, not a silent one to make while closing a checklist.

**Item 5 — documentation.** The README's status section still described **Phase 4**. It claimed
IMAP was "partway built" with "`EXPUNGE`, `COPY` and `MOVE` … the next increment, so mail cannot
yet be deleted over IMAP", and that "POP3 is Phase 6" — all three false since Phase 5 and Phase 6
respectively. The fixture-corpus row still said 22 fixtures against an actual 102 (ADR-072), and
no row existed for any Phase 7 diagnostic. The status table now carries the six Phase 7 areas,
both stale rows are corrected, and "Core capabilities — planned for v1.0" is corrected to say
what is built. `ARCHITECTURE.md`'s status line moved from "Phase 7 in progress" to Pass A
complete.

That the README lagged three phases is the same failure as the unmeasured threshold: item 5 was
ticked at each phase boundary by assertion. The coverage gate is now mechanical; the README is
not, and remains a discipline.

**Consequences.** 1836 → 1843 Rust tests. CI gains a coverage stage; expect it to be among the
slower ones, since instrumented builds do not share the ordinary test cache. Phase 7 Pass A now
satisfies all seven Definition-of-Done items.

**Amendment — the first CI run of this step failed, and not on coverage.** It died linking
`sandesh-app`'s `rest` test with `collect2: fatal error: ld terminated with signal 7 [Bus error]`
and an invitation to file an LLVM bug. It was a resource limit, not a compiler defect: the
workspace links 53 test binaries, an instrumented one is ~250MB with full debug info, and a hosted
runner has ~14GB of free disk. Three changes fix it, each measured rather than guessed:
`CARGO_PROFILE_DEV_DEBUG=line-tables-only` (248MB → 76MB per binary, and coverage needs line
mappings rather than full DWARF), one codegen unit per crate, and reclaiming the runner's
preinstalled toolchains for ~25GB. Verified from a clean instrumented build: peak RSS 841MB, 2m32s,
gate passing.

The same arithmetic explains local disk. Cargo never prunes old artifacts, so a long-lived
`target/` accumulates stale copies of every relink — 63 of one test binary here, 98GB in total
against 13GB for a complete fresh build. `cargo clean` reclaims it; nothing under `target/` is
tracked, so the repository stays ~18MB.

### ADR-083 — Live tailing by flushing the transcript, and shared states across the diagnostics views

**Status.** Accepted. Phase 7 Pass B, scoped to two items.

**Context.** Pass A met every Phase 7 acceptance criterion, but the trace-viewer *deliverable* reads
"per-connection transcripts, **live and historical**". Historical worked; live did not. Pass B's
candidate list (§4 of the Phase 7 plan) held six items; after building all six increments the honest
read was that four had no consumer asking for them — ARC headers, per-endpoint HTTP histograms,
event-bus lag gauges and log field search — the same reasoning that struck `--no-web` in ADR-076.
Two were worth doing: the one closing a deliverable's wording, and the state handling the new views
had quietly skipped.

**Why live tailing did not work.** `OpenConnection::persist_open` wrote a row on accept — it has to,
because `deliveries.connection_id` is a foreign key onto it — but with an **empty transcript**. The
lines lived only in `sandesh-proto`'s in-memory `Transcript` and reached storage once, at `close`.
So an in-flight conversation was a row with nothing in it.

**Decision — flush periodically rather than add a port.** The recorder upserts the in-progress
transcript every `TRANSCRIPT_FLUSH_INTERVAL` (500ms) while the socket is open. The alternatives
were both worse: a `LiveTranscripts` port in `sandesh-core` implemented by `sandesh-proto` would
add a trait, an implementation and composition wiring for one view, and lose everything on a crash;
pushing transcript lines onto the event bus contradicts `EVENT_MODEL.md` §1, which says events
carry identifiers rather than content, and IMAP `IDLE` consumes that same bus.

Flushing needs no new crate edge — `sandesh-api` still reads through `ConnectionStore`, and the
`ConnectionDto` already reported `closed_at: null` and `duration_us: null` correctly, so **no API
change was required at all**. It also improves crash behaviour: a session that dies mid-conversation
now leaves a partial transcript where it previously left an empty one.

A timer rather than a write per line, because an IMAP sync issues thousands of commands and a
database write per line would make tracing cost more than the protocol work it observes. 500ms is
chosen against what the view is for — watching a conversation unfold — not a latency budget.

**The ordering hazard, and the guard.** A flush writes `closed_at: None`. One landing *after* the
final write would resurrect a finished connection as permanently open — a row the viewer shows as
in-flight forever. `close` therefore takes the `FlushGuard`, stops the flusher and yields before
snapshotting. A guard rather than a bare token so an early return cannot leak the task. Verified by
mutation: leaking the guard with `mem::forget` makes
`a_flush_never_lands_after_the_connection_closes` fail.

**It broke two existing tests, correctly.** `sandesh-smtp/tests/tracing.rs` polled for "the most
recent record", which before this change could only be the closed one. Now the open row appears
first, so both tests caught an in-flight record and failed on `closed_at`. The tests' assumption had
become wrong, not the code; they now poll for a *closed* record specifically.

**Decision — the diagnostics views use the shared state components.** `TracesRoute`, `ApiRoute` and
`LogsRoute` each hand-rolled their loading, empty and failure states as plain paragraphs, so a
failed request carried no `role="alert"`, a load in progress carried no `role="status"`, and none
offered a retry — the only recovery from a failed contract or backlog fetch was a page reload.
They now use `States.tsx` like the rest of the console.

Two distinctions came out of doing it. **Tracing being off is an empty state, not an error**:
offering "try again" would invite retrying something that cannot succeed until the server restarts.
And **"nothing logged yet" is not "nothing matches this filter"** — conflating them sends someone
hunting a capture bug when they have simply filtered everything out, which is the same mistake A2
corrected for the traces view.

**Consequences.** 1843 → 1847 Rust tests, 206 → 216 frontend tests. Coverage holds, with
`sandesh-proto` rising to 93.08%. Verified in a browser against a running server: an open IMAP
connection shows a `LIVE` badge and a banner, reports `open` rather than a fabricated duration, and
its transcript grew from two commands to five in place with no navigation. The four remaining
Pass B candidates are deferred to post-v1 rather than dropped.

### ADR-084 — Fault injection is runtime-mutable, in-memory, and deliberately narrow

**Status.** Accepted. Phase 8 A1, before implementation.

**Context.** Phase 8's first deliverable is configurable SMTP fault injection, and its acceptance
criterion reads "each fault mode is triggerable by **configuration** and observable by the client".
Taken literally against the configuration model Sandesh has, that criterion cannot be met. ADR-031
settled that configuration is `SANDESH_*` environment variables read once in the composition root,
and the Phase 8 audit confirmed it holds without exception: no `RwLock<Config>`, no `ArcSwap`, no
`watch` channel and no settings endpoint exists anywhere in `crates/`. Every field of `SmtpConfig`
is immutable for the process lifetime.

A fault configured that way would need a **server restart to change**. The consumer for this
feature is a CI suite asserting application resilience — turn a `RCPT` rejection on for one test,
off for the next, in-process. Restart-per-scenario defeats the deliverable.

**Decision — a runtime-mutable fault registry, and nothing else runtime-mutable.** Fault rules live
in a `FaultRegistry` in `sandesh-core`, held as `Arc<FaultRegistry>` with an interior `RwLock`,
cloned into the SMTP server and the API router. Rules are set, inspected and cleared through
`POST`/`GET`/`DELETE /api/v1/faults`.

The narrowness is the decision, not an omission. Ports, paths, TLS settings and every other
`SmtpConfig` field stay startup-read and immutable. They have no test-scenario use, and mutating
them at runtime raises rebind-and-restart semantics that Phase 8 has no reason to answer. This ADR
does **not** open general runtime configuration; a later consumer wanting that must argue for it
separately.

`Arc` with an interior lock rather than `ArcSwap`: it matches the existing `Arc<DeliveryService>`
idiom the session already carries, needs no new dependency, and makes adding a single rule a plain
write instead of a read-modify-swap of the whole set. Reads are per-command, but a fault check is
a short read lock over a near-always-empty vector, against a command that has just done socket I/O.

**Decision — rules do not survive a restart.** No table, no migration, no reload. This follows from
ADR-005's off-by-default stance: the failure mode of fault injection is a developer losing an
afternoon to a "broken" mail server that is behaving exactly as they configured it an hour earlier.
Keeping the registry in memory makes **restart the reliable escape hatch** from that state. A
persisted fault would survive the one action everybody takes when a server behaves inexplicably,
and would need its own clear-on-startup flag to undo — a mechanism whose only purpose is to escape
the persistence we chose to add.

CI suites are unaffected: a run configures its faults explicitly, which also means nothing leaks
between runs.

**Faults are loudly visible.** Active faults appear in `/info` and as a persistent indicator in the
UI. A fault that can be forgotten is a fault that will be, and the diagnostics work of Phase 7
exists precisely to stop a developer guessing at unexplained behaviour.

**Off by default, and provably inert.** With no rule configured the SMTP path must behave
byte-identically to Phase 7. That is asserted by the existing SMTP suite continuing to pass
untouched, rather than by a new test claiming it.

### ADR-085 — Outbound HTTP lives in a new `sandesh-webhook` adapter crate

**Status.** Accepted. Phase 8 A2, before implementation. Resolves open question 2 of the Phase 8 plan.

**Context.** A2 delivers webhooks on `MessageReceived` with retry and a durable outbox. That makes
Sandesh an **outbound** HTTP client for the first time. Every HTTP dependency in the workspace
today is inbound — `axum`, `hyper` and `tower` in `sandesh-api`, serving requests. Nothing dials
out, and no crate outside `dev-dependencies` carries a client.

Three placements were available, and the architecture test constrains the choice before taste does.
`tests/architecture.rs` bans `axum`, `hyper`, `tower` and `tokio` from `sandesh-core`'s **runtime
closure**, and `core_declares_only_its_permitted_direct_dependencies` pins core's direct
dependencies to six crates. So core cannot own the client, transitively or directly — that much is
mechanical, not a judgement call.

**Decision — a new `sandesh-webhook` crate owns the client, the retry policy and the worker.** It
depends on `sandesh-core` for the `OutboxStore` port and the domain types, and on the HTTP client
and `tokio`. It is wired at the composition root like every other adapter.

This matches the shape the workspace already has: one crate per adapter concern, each owning
exactly the infrastructure its concern needs — `sandesh-storage` owns `sqlx`, `sandesh-smtp` owns
the SMTP codec, `sandesh-api` owns `axum`. Webhook delivery is an adapter concern of precisely that
kind, and giving it a crate keeps the ban lists able to say something true about it. A crate
boundary is also the cheapest place to assert the invariant that matters here: `sandesh-webhook`
must reach `sandesh-core` and nothing else `sandesh-*`, the same rule `sandesh-storage` is held to.

**Rejected — `sandesh-app`.** Cheapest in lines, and the composition root already owns `tokio` and
the lifecycle. But `sandesh-app` is wiring: it constructs adapters and starts them. A retry policy,
a backoff schedule and a payload format are domain-adjacent logic with their own tests, and putting
them there makes the composition root the one crate whose contents no boundary test can describe.
The first exception is the one that makes the rule advisory.

**Rejected — `sandesh-api`.** Tempting because `hyper` is already in its tree, so the marginal
dependency cost is near zero. But the cost that matters is not the dependency, it is the direction:
`sandesh-api` is the inbound surface, and a crate that both serves and dials is a crate whose
failure modes no longer separate. A webhook endpoint timing out and an API request timing out would
share a crate, a test suite, and eventually a connection pool.

**The architecture test is extended, not merely left to pass.** A new case asserts
`sandesh-webhook`'s `sandesh-*` closure is exactly `sandesh-core`, and the existing core ban list
gains the HTTP client crate. Without that, the invariant this ADR claims would hold only by
accident.

*Added after implementation:* it needed **two** cases, not one. A ban list says only that nobody
reaches the client — it is satisfied just as well by the feature not existing. A second case
asserts `sandesh-webhook` actually has it in its closure, so the pair states the real invariant:
exactly one crate dials out, and it is this one.

*The client, which this ADR deliberately left open:* **hyper**, via `hyper-util`'s legacy client.
It is already in the tree beneath axum, so outbound delivery costs two feature flags rather than a
second HTTP stack with its own TLS backend and connection pool. A webhook delivery is one POST with
a timeout; `reqwest`'s conveniences buy nothing at that size.

### ADR-086 — The outbox worker is a single polling task, owned by the application lifecycle

**Status.** Accepted. Phase 8 A2, before implementation. Resolves open question 3 of the Phase 8 plan.

**Context.** The A2 acceptance criterion is "webhooks retry on failure **and survive a restart**".
Restart survival is named explicitly, which rules out an in-memory queue and forces a durable
outbox: a SQLite table whose rows carry endpoint, payload, attempt count, `next_attempt_at` and a
terminal status. The open question was what drains it. This is the first background task in the
codebase with a lifecycle of its own — the protocol listeners are long-lived, but they are driven
by inbound connections rather than by their own schedule.

**Decision — one `tokio` task, polling for due rows on an interval.** It selects rows whose
`next_attempt_at` has passed, attempts delivery, and on failure writes back an incremented attempt
count and a backoff-computed next attempt. After a maximum attempt count the row is
**terminal-failed and visible**, never silently dropped — a webhook that stopped being delivered
without saying so is the failure this feature exists to make diagnosable.

Polling is chosen because the criterion is about durability, and polling makes durability
structural rather than asserted. The worker holds no state: everything it needs to resume is in the
table, so a restart is indistinguishable from a slow tick. The restart test the plan calls for —
enqueue, stop the worker, restart it, assert the pending row is still delivered — is then testing
the real mechanism rather than a reload path written specially for it.

**Rejected — event-driven wake with a poll fallback.** Notifying the worker on enqueue lowers
happy-path latency from half a tick to near zero. But retries still need the poll, so the fallback
is not optional, and the design becomes two paths to the same drain with the shutdown interaction
between them to get right. Latency is not in the criterion, and a test suite asserting a webhook
arrived can wait a tick. Reinstate this if a consumer reports the delay.

**Rejected — a worker pool.** Concurrency needs row leasing to avoid two drainers delivering the
same row twice, which is real complexity for throughput no criterion asks for. A single drainer
gives at-most-one-in-flight per row for free.

**The worker starts and stops with the application, like the listeners.** It is spawned at the
composition root and takes the same shutdown signal, draining in flight work before exit rather
than being dropped mid-request. A task that outlives `Engine` shutdown would make the graceful
shutdown Phase 2 delivered untrue.

**Enqueue happens in the delivery transaction; sending never does.** `MessageReceived` fires after
commit, a Phase 1 invariant. The outbox row is written inside the delivery transaction so "stored"
and "will be notified" are one event, and the HTTP request happens later in the worker. An SMTP
client must never wait on a consumer's slow webhook endpoint, and an endpoint being down must never
fail a delivery.

*Two consequences settled during implementation, recorded here because both are binding:*

**One outbox entry per placement, not per message.** `MessageReceived` is emitted once per
placement, so a message delivered to three accounts produces three events. Collapsing them into a
single notification would invent an aggregate event the bus does not have, and would force a
consumer that cares about one account to re-derive what it had already been told. The cost is N
requests for an N-recipient message, which is the honest shape of the fact being reported.

**Endpoints persist; the outbox persists with them.** This is the opposite of ADR-084's stance on
fault rules, and deliberately so: the reasoning there was that a forgotten fault makes a server
look broken, so restart must be the escape hatch. A forgotten webhook endpoint is inert by
comparison — it fails its own notifications and nothing else. More decisively, an outbox row that
survived a restart pointing at an endpoint that did not would be durable in name only, and the
criterion names durability of the delivery, which includes its destination.

### ADR-087 — Message rules are persisted, narrow, and record why they fired

**Status.** Accepted. Phase 8 A3, before implementation.

**Context.** A3's acceptance criterion is "rules apply at delivery **and are visible in the delivery
record**". The second clause is the harder half and shapes everything else: a rule that silently
adds a keyword is exactly the opacity Phase 7 was built to remove, and a developer who finds
`$Invoice` on a message with no way to learn which rule put it there is worse off than one who
finds no tag at all.

The substrate is in place. `Keyword` and `MessageFlags` already exist and IMAP `STORE` already
writes keywords, so the *target* of a tagging rule is built; nothing yet evaluates a condition.
`DeliveryService::deliver` builds each `NewPlacement` with `MessageFlags::empty()`
(`service/delivery.rs:227`), which is the one place where a rule's effect can reach storage inside
the delivery transaction.

**Decision — rules persist, in a table, unlike faults.** Phase 8 has now made this choice three
times and given three different answers, so the reasoning matters more than the outcome:

| Feature | Lifetime | Why |
| --- | --- | --- |
| Fault rules (ADR-084) | In-memory, cleared by restart | A forgotten fault makes a server look broken; restart must be the escape hatch |
| Webhook endpoints (ADR-086) | Persisted | An outbox row outliving its destination would be durable in name only |
| **Message rules** | **Persisted** | Configuration a developer curates over a session, with no hazard to escape from |

A tagging rule is not dangerous the way a fault is. The worst it does is put a keyword on a message,
which is visible, reversible through `STORE`, and explained by the delivery record this ADR
requires. So the ADR-084 reasoning does not transfer, and losing a curated rule set on every restart
would make the feature useless on a long-running development server.

**Decision — conditions stay narrow in Pass A: a header substring, the envelope sender, the
envelope recipient.** Deliberately not an expression language. Sandesh already has one query
grammar — `SearchQuery` (ADR-048) — and a second, subtly different one would be a parser to write,
a grammar to document and a fuzz target to maintain, for a criterion that asks only that rules
apply and are explained. Body matching is excluded for a narrower reason: it would make every
delivery scan the decoded body, which is real per-message cost on the hot path for a capability
nobody has asked for. Both are Pass B candidates if a consumer appears.

Matching is case-insensitive substring, because the alternative — anchored or regex — invites the
expression language this ADR is declining to build.

**Decision — the outcome is recorded in a child table, not folded into an existing field.**
`delivery_rules` is keyed by delivery and names the rule that matched and the keyword it added,
mirroring `delivery_recipients`. Two rejected alternatives, each for a specific reason:

- **A JSON column on `deliveries`** would be fewer moving parts, but it is unqueryable — "which
  messages did this rule touch?" becomes a table scan with a string search — and it breaks the
  pattern every other delivery detail already follows.
- **Appending to the per-recipient `reason` string** needs no migration at all, but it conflates
  *why this landed here* with *why this got tagged*. Those are different questions, and the Phase 7
  work exists because conflating explanations is how a diagnostic surface stops being diagnostic.

The recorded row carries the rule's **name and the keyword applied**, not a foreign key alone: a
rule deleted or edited after the fact must not rewrite history, and a delivery record that renders
as "rule 019a… matched" once its rule is gone explains nothing. This is the same reasoning ADR-086
applied to the webhook payload, rendered at enqueue time rather than at send time.

**Rules apply inside the delivery transaction.** Evaluation happens in `sandesh-core` against the
parsed message, and the resulting keywords reach storage as part of the `NewPlacement` that the
same transaction inserts. A rule that "applied" but whose tag was written afterwards would be a
second write that could fail on its own, leaving a message the delivery record claims was tagged
and which is not. That is the rule ADR-074 established for the search index and ADR-086 for the
outbox: the fact and the record of the fact commit together.

**Evaluation is pure and total.** `RuleSet::evaluate` takes the parsed message and the envelope and
returns the matches; it performs no I/O, cannot fail, and cannot reject a delivery. A malformed
message must still be deliverable — storing malformed mail is the product's purpose — so a rule
that cannot be evaluated against it simply does not match.

### ADR-088 — `BDAT` is a second content path, and the absent-capability test is inverted deliberately

**Status.** Accepted. Phase 8 A4, before implementation.

**Context.** `BDAT`/CHUNKING (RFC 3030) is a Phase 8 roadmap deliverable with no acceptance
criterion of its own. It exists because `DATA`'s framing is hostile to binary content: the sender
must scan every line for a leading dot and stuff it, and the receiver must unstuff, which is
per-byte work on both sides and the single most common source of corrupted attachments. `BDAT`
replaces the terminator with a declared length, so content is copied verbatim.

The Phase 8 audit found `BDAT` was not merely unimplemented but **parsed and refused**:
`command.rs:235` groups it with `ETRN`/`TURN`/`ATRN` as `Command::Unimplemented`, which earns a
truthful `502`. That grouping is correct today and must be undone as part of implementing it.

**Decision — `BDAT` is a real command with its own state, not a variant of `DATA`.** The session
gains `State::ReceivingChunks`, distinct from `ReceivingData`. They cannot be merged, because their
framing is incompatible in a way that matters:

| | `DATA` | `BDAT` |
| --- | --- | --- |
| Ends at | `<CRLF>.<CRLF>` | a declared octet count, with `LAST` |
| Dot-stuffing | applied and reversed | **none** |
| Reply | one, after the terminator | **one per chunk** |

A single collector switching on a flag would have to be right about all three on every path. Two
explicit states make the incompatibility visible to the type system instead.

**Decision — the two paths must produce byte-identical stored content, and that is the test.** The
Phase 2 raw-fidelity guarantee (ADR-003) is the thing most at risk here: a second ingestion path is
a second chance to corrupt bytes. So the test is not "BDAT stores something plausible" but "the same
message sent by `DATA` and by `BDAT` produces identical stored bytes", compared directly. That
framing is also the cheapest way to catch a dot-unstuffing bug leaking into the chunked path, where
it must not happen at all — a body line `..hidden` arrives over `BDAT` as literally `..hidden`.

**Decision — the absent-capability assertion is inverted, and the rule it protects is kept.**
`crates/sandesh-smtp/tests/protocol.rs:37` asserts that `CHUNKING` does **not** appear in the EHLO
response, alongside `AUTH`, `STARTTLS`, `SMTPUTF8` and `DSN`. That test encodes the rule ADR-060
established for IMAP: *never advertise what is not implemented*. Implementing `BDAT` therefore
requires moving `CHUNKING` from the absent list to the present list — visible in the diff as a
deliberate reversal rather than a deletion.

The rule itself is unchanged and still enforced: `SMTPUTF8` and `DSN` remain in the absent list and
are still unimplemented, and `AUTH`/`STARTTLS` keep their existing conditional treatment. Only the
one capability whose implementation this ADR delivers moves. An inverted assertion with no ADR
would read, correctly, as someone weakening a test to make their change pass.

**Interleaving `BDAT` and `DATA` is refused (RFC 3030 §3).** A transaction commits to one content
mechanism. `DATA` after a `BDAT` chunk, or `BDAT` after `DATA` has begun, is a `503` — not a
best-effort merge. The two framings cannot be concatenated coherently, and a server that tried
would produce a message neither client meant to send.

**A chunk size is bounded before it is allocated.** `BDAT <n>` is a client-supplied length, and
reading it as an allocation size is the obvious denial-of-service. The declared size is checked
against `max_message_size` *before* any buffer is reserved, which is the same rule
`sandesh-imap`'s literal handling already follows (ADR-040). An oversize transaction is still read
to its end where possible so the connection stays synchronised, matching what `DATA` already does
for a `552`.

### ADR-089 — Replay records provenance twice, and `mboxrd` framing carries an exact length

*Phase 8 A5. Supersedes nothing. Closes question 4 of the Phase 8 plan.*

Two decisions, taken together because the acceptance criterion — "exported messages re-import
byte-identically" — is the only Phase 8 criterion that can fail *silently*, and both halves of A5
are shaped by that.

**Decision — a replayed message records its source in both the bytes and the delivery record.**
§5 question 4 asked whether provenance should be a header, a delivery-record field, or both. It is
both, and the duplication is deliberate because the two answer different questions:

- `X-Sandesh-Replay-Of: <original-message-id>` is written into the replayed message's own header
  block, so the link **travels with the message**: it survives `GET /messages/{id}/raw`, an IMAP
  `FETCH`, and an export to mbox. A CI suite asserting that its application handled a replay sees
  it without a second request.
- `deliveries.replay_of_message_id` makes the same link **queryable**. A header is content, and
  content is searched, not joined; "which messages are replays of this one?" is a question about
  structure. Answering it by scanning bodies is what `0003_deliveries_by_message.sql` was written
  to stop doing.

Neither alone is sufficient, and the failure modes are not symmetric. A message with no
`Message-ID` — `testdata/malformed/` has several — cannot carry a meaningful provenance header at
all, and the field is what holds the link in that case. The column is the authority for the API;
the header is the disclosure to whatever reads the mail.

**A replay is a new message, not a second reference to the old one.** It gets a fresh
`Message-ID`, and the original's is removed rather than left in place: reusing it would make the
two indistinguishable to every downstream consumer that deduplicates on it, which is most of them,
and carrying both would give the message two `Message-ID` headers, which RFC 5322 forbids and
parsers resolve inconsistently.

**Decision — replay, import and injection share one placement path.** `place_raw_message` is the
single implementation; `POST /messages`, `POST /messages/{id}/replay` and `POST /import` all call
it. "A replayed message is placed exactly as an injected one is" is therefore structural rather
than three routing rules kept in agreement by hand — the same argument ADR-088 made for `DATA` and
`BDAT` sharing a tail. The path now also writes a delivery record for every API placement, not
only replays, because without one there is nowhere to hang `replay_of`.

**Decision — export uses `mboxrd`, and the reason is that `mboxo` cannot meet the criterion.**
mbox has no length prefix: a message ends where the next `From `-at-start-of-line begins, so a
body line of that shape must be escaped. The variants differ only in how:

| | Writes | Reads | Round trip |
| --- | --- | --- | --- |
| `mboxo` | `From ` → `>From ` | strips nothing | **lossy** — each trip adds a `>` |
| `mboxrd` | `>*From ` → `>` + it | strips one `>` | invertible |

`mboxrd` quotes the *already-quoted* forms too, which is what makes the transform invertible. The
asymmetric variant is the bug this decision exists to not have, and `from_quoting_survives_the_round_trip`
pins it directly rather than trusting the corpus to happen to contain such a line.

**A message that does not end at a line boundary needs an exact length, or it cannot round-trip.**
This is the defect the corpus test found that unit tests had not. mbox requires the next separator
to start a line, so a message with no trailing newline forces a synthetic terminator — and a
synthetic terminator is a byte the message did not have. Four `testdata/malformed/` fixtures are
this shape (a truncated message is exactly it), plus a zero-byte one, so it is a real case.

Such an entry therefore carries `Content-Length:` immediately after the separator, stating the true
octet count, and `parse` truncates back to it. Only the line *directly* after the separator is read
as framing, so a message carrying its own `Content-Length` header keeps it. A reader that ignores
the header still sees a well-formed mbox — one byte long in that message — which is the failure
this format has always had and not one this decision introduces.

**A zero-byte message is imported, not skipped.** The first implementation skipped empty framed
messages and the corpus test caught it: 101 of 102 fixtures imported, and nothing else would have
reported the loss. Phase 2's rule is that malformed mail is *stored*, not rejected, and
`testdata/malformed/empty.eml` is a deliberate fixture. Skipping it would make a corpus round trip
quietly lose a message, which is precisely the silent failure this increment's test exists to catch.

**The round-trip criterion is asserted over the corpus enumerated from disk**, as ADR-063
established, so it grows with the fixtures rather than against a number written into the test.

---

### ADR-090 — Link extraction is a scanner, and resolution is not a feature that was omitted

**Status.** Accepted. Phase 8 A6.

**Context.** The roadmap's last Phase 8 deliverable is "link and image checking in HTML messages".
The word *checking* invites an implementation that fetches each URL and reports a status code, and
that implementation must not be built.

**Decision 1 — nothing is resolved, and the stance is structural rather than a default.**
`sandesh-core::links` takes bytes and returns structs; it has no client, no I/O and no configuration
that could introduce either. §2 of the Phase 8 plan argued this as a policy; this ADR makes it a
property of the code.

A development mail server that fetches the URLs in mail it received is:

- an **SSRF vector** aimed at whatever a message chooses, from inside the developer's network, and
- a **confirmation to the sender** that the message was processed — which is the entire purpose of
  a tracking pixel, and it would be Sandesh firing it rather than the reader.

A one-time link followed by the mail server is also a one-time link *spent* by the mail server.

The test for this half of the deliverable is an absence, so it is asserted as one:
`nothing_is_resolved_over_the_network` binds a listener the test owns, points a delivered message's
`href`, `img src` and `iframe src` at it, and fails if a single connection arrives. A bug that made
extraction fetch would satisfy every assertion about the *list* it returns, so no test of the
payload could catch it — and asserting it against a real socket keeps the property true through a
future change that adds an HTTP client for some other reason.

**Decision 2 — a hand-written scanner, not an HTML parser.** A conforming parser builds a tree,
which means deciding what to discard. Mail HTML is generated by template engines, mangled by
clients and truncated by gateways, and the malformed message is the one a developer opened Sandesh
to look at. The scanner walks tags and attributes directly, so an unterminated tag still yields its
`href` (`unterminated_tag_still_yields_its_href`). This is the same reasoning ADR-078 applied to
`Received:` parsing: report the unparseable input rather than dropping it.

It also means no new dependency for a crate the architecture test keeps thin.

**Decision 3 — extract from the *decoded* part.** Scanning stored bytes would report `?a=3D1` for
every quoted-printable message, a URL the message never contained — and quoted-printable is what
real clients send, so that is the common case rather than an edge one. `part_body_decoded` already
existed for the UI; this reuses it. Entity decoding is the matching half: `&amp;` is mandatory in
HTML, so a correct query string contains it, and reporting it verbatim would show a URL the
template did not emit.

**Decision 4 — classified by the element's claim, reported without a verdict.** A URL's kind is
what the markup asks the client to *do* with it: an `<img src>` pointing at an HTML page is still an
image. Beyond that, the payload states facts and scores nothing — `insecure` means the scheme is
`http`, not that the link is dangerous, and plenty of development mail legitimately points at
`http://localhost`. The UI follows: no success green, no danger red, the disclaimer above the list,
and the URL rendered as **text rather than an anchor**, so a panel whose premise is that Sandesh
made no request does not offer one click that makes it.

**Consequences.** Opt-in resolution stays a Pass B candidate and remains gated on this stance; if it
is ever built it belongs behind an explicit per-request action, never a configuration default.
Extraction covers HTML parts only — a URL in a plain-text body is not a reference a client acts on,
and reporting it would blur the distinction the panel exists to draw.

---

### ADR-091 — The message-list cursor walks down, and virtualisation is what makes the criterion reachable

**Status.** Accepted. Phase 4 completion.

**Context.** Phase 4's last unmet acceptance criterion was "the list stays smooth at 10,000
messages". Two of the three gaps ROADMAP listed alongside it — search and bulk actions — turned out
to be **already built**; only this one was real. Building it found a defect underneath it that no
amount of frontend work could have fixed.

**The defect: `GET /api/v1/messages` paginated in the wrong direction.** The list is ordered
newest-first, and UIDs ascend with age, so the cursor must walk *down*. The filter read
`entry.uid > after`. Following `next_cursor` therefore returned the rows the client had already
seen, and `has_more` was then false — so **every mailbox truncated at 99 messages**, whatever its
size, and reported itself complete.

Two things made this invisible:

- Within-page ordering was correct, and a second page did exist, so the bug looked like working
  pagination from any single request.
- **Nothing in the workspace walked a cursor to the end.** `next_cursor` appeared in `dto.rs` and
  `routes.rs` and in no test at all, while Phase 3 listed "cursor pagination is stable under
  concurrent insertion" as a *met* criterion. A criterion with no test said nothing, exactly as the
  Outstanding-before-Phase-7 audit found for six earlier deliverables.

`tests/pagination.rs` asserts the property that actually matters — **walking every page yields each
message exactly once and reaches all of them** — rather than "a second page exists", which the
broken code satisfied. Reverting the comparison fails three of its four cases.

The cursor is now the *minimum* UID on the page rather than the first entry's, because a message
placed in several mailboxes carries several UIDs and the next page must start below all of them.

**Decision — virtualise, rather than raise the page size.** Ten thousand rows is ~60,000 DOM nodes
and no row-level tuning makes that scroll. Only the visible window is rendered, inside a spacer
holding the full height so the scrollbar still describes the whole list. Measured in a browser
against 10,000 real messages: ~21 rows and ~457 DOM nodes at the last page, identical to the first,
and one frame at 60fps per scroll. Rendering cost is flat in list size, which is what "stays smooth"
means.

`@tanstack/react-virtual` rather than a hand-written windower: it is headless, already shares an
author and release cadence with the `@tanstack/react-query` this app depends on, and row measurement
is the part a hand-written version gets wrong — rows here have variable height (a long subject, a
multi-placement chip wrap to a third line).

**Accessibility is not a detail here; virtualisation breaks two things unless handled.**

1. `aria-activedescendant` must name an element that **exists**. Keyboard navigation to an
   off-screen row therefore scrolls it into view first and lets the virtualiser mount it. Without
   this the attribute points at an id that is not in the DOM — screen-reader announcement breaks
   silently while the screen looks correct. Verified in a browser: `End` in a 10,000-message list
   moves to item 10000, the row mounts, and the attribute resolves.
2. Each row carries `aria-setsize` and `aria-posinset`. A listbox holding twenty mounted children
   otherwise announces "3 of 20" in a list of ten thousand.

**Consequences.** jsdom reports every element as 0×0 and implements no layout, so the virtualiser
correctly concludes nothing is visible and renders nothing — which failed 20 existing tests. The fix
is to give jsdom real measurements in `src/test/setup.ts`, **not** to stub the virtualiser away: a
stub would mean the suite asserts against a list that does not ship. The tests now exercise the real
windowing code.

Paging is driven by the last *rendered* row rather than a scroll offset, which stays correct when
row heights vary. It also deliberately ignores the client-side filter: a filter matching nothing on
the loaded page must still fetch further pages, or the UI would report "no matches" for a mailbox
that has them further down.

---

### ADR-092 — The desktop shell consumes the composition root; it does not become a second one

**Status.** Accepted. Phase 9 A1.

**Context.** ROADMAP frames Phase 9 as *"the audit of every earlier phase's boundary discipline"*,
with the criterion that it *"requires zero changes to the engine or UI — if changes are needed, an
earlier phase violated ARCHITECTURE §11."*

**The audit result: the boundary held.** `crates/sandesh-desktop` starts
`sandesh_app::Application::start(desktop_config(..))`, reads `addresses().http`, and opens a WebView
on it. No engine file changed, and no UI file changed. The lifecycle §11 asked for in Phase 0 —
construct-and-bind, serve, cancel — is the one eight integration-test files already drive, so the
desktop path was load-bearing before it had a consumer.

**Decision 1 — the WebView loads `http://127.0.0.1:<port>`; Tauri bundles no assets.** The engine
already serves `web/dist` through `rust-embed`, so pointing the WebView at the engine means there is
**one** asset build rather than two. "The React build is byte-identical to the web build" is then
structural, not a check that could drift.

It also settles the §11 requirement that *"the UI must reach its backend through a configurable base
URL, never a hardcoded origin"*, which `client.ts` does not meet. That requirement was written for a
shell that bundles assets and calls the engine cross-origin. Under the loopback transport the
same-origin assumption is correct rather than limiting, so **no UI change is needed** — and the
`/api/v1` relative base, the CSP, and ADR-030's sandboxed preview all keep the origin semantics they
were designed against. If a consumer ever needs the bundled shape, the configurable base URL becomes
genuinely necessary and gets its own decision.

**Decision 2 — the shell is a separate crate, and the architecture test is amended to say so.**
`the_app_is_the_only_composition_root` asserts nothing depends on `sandesh-app`. A desktop shell must,
so the test now names `sandesh-desktop` as the one permitted exception — the precedent ADR-085 set
when `sandesh-webhook` added a boundary: amend the test to describe the architecture, never let a
new crate pass by accident.

Auditing that test found it **weaker than it read**. It checked a hand-written list of crate names,
so `sandesh-desktop` was admitted silently — a new crate could have depended on the composition root
with no test noticing. It now enumerates `sandesh-*` from the resolved graph, so the rule covers
crates added after it was written, and asserts:

1. every `sandesh-*` crate except the listed exception is free of `sandesh-app`;
2. the exception is **not stale** — a listed crate that stops depending on the app fails, so the
   rule cannot quietly widen;
3. `sandesh-desktop` depends on **no** crate the app wires (`sandesh-storage`, `sandesh-api`, the
   protocol crates). A shell that constructed its own storage would be forked wiring, which is
   exactly what "no forked code" forbids.

Both new rules are mutation-tested: adding `sandesh-storage` to the shell, and removing
`sandesh-app` from it, each fail with the intended message.

**Decision 3 — ephemeral loopback ports, and a platform data directory.** A desktop app must not
fail to launch because the user already runs Sandesh, and must not silently take that server's port;
it must never bind `0.0.0.0`, because installing a GUI must not put a mail server on the network;
and its mail must not follow the working directory, which is arbitrary for an app launched from an
icon. `tests/desktop.rs` asserts all three, including that two instances start simultaneously.

**Consequences, and what is *not* claimed.** The window was verified on this machine: it opens, it
renders the identical UI, it reports the engine's real ephemeral ports, mail delivered over SMTP
appears live in it, and closing it stops the listeners. Driving it further was defeated by the
sandbox's WebKit renderer going unresponsive — an environmental limit, disclosed the way Phase 5
disclosed Thunderbird and `mutt`, not a defect in the shell. The engine kept serving throughout,
which is how the two were told apart.

**`Engine` was not renamed.** §11 and ROADMAP name an `Engine` type; the workspace has
`Application`, with §11's exact shape. That is a documentation defect, and the documents are
corrected rather than renaming a type the CLI and eight test files use — the requirement §11 cared
about is embeddability, and it is met.

macOS `.dmg` and Windows `.msi` are **not built**: they need those operating systems and signing
certificates, neither available here. Linux `deb`/AppImage targets are configured.

---

### ADR-093 — A notification says what the event knows, and a burst is one toast

**Status.** Accepted. Phase 9 A2.

**Context.** ROADMAP asks for "native menus, tray icon, notifications on new mail". The notification
is the interesting one: it is the first feature that wants message *content* in a place the event
bus deliberately does not carry any.

**Decision 1 — the notification reports the event, and does not resolve it.**
`EventPayload::MessageReceived` carries identifiers and a size, never a subject or sender, because
`EVENT_MODEL.md` §6 makes events references rather than bodies — that is what keeps a thousand
queued events bounded however large the mail is.

A toast reading "from alice@example.test — Invoice #42" would need either a reach past
`sandesh-app` into the message store, which is the forked wiring ADR-092's architecture test
forbids, or an HTTP request to the shell's own embedded server on every delivery. Neither is worth a
sender's name. The notification therefore says what arrived and how large it was, and the window —
one click away on the tray — says everything else.

This is the same discipline ADR-078 applied to header analysis: report what is actually known,
rather than manufacturing a richer-looking claim.

**Decision 2 — deliveries inside a window coalesce into one notification.** A CI suite delivering a
thousand messages must not raise a thousand toasts; that is not a notification, it is a denial of
service against the user's own desktop. Deliveries within 1.2 seconds become one notification that
counts them. Verified live: four messages delivered in a burst produced exactly one toast reading
`count=4`, and a single message still produced `count=1` immediately.

The window is short enough that one message feels instant and long enough that a batch is one
summary — the behaviour a mail client has when a sync brings in a batch.

**Decision 3 — closing the window hides it; only Quit stops the engine.** This *changes* A1, where
window destruction cancelled the engine. That was right while the window was the only interface,
but a tray that outlived its server would offer a menu of dead items. Quit — chosen explicitly —
cancels the shutdown token *before* Tauri tears the event loop down, so the listeners release their
sockets promptly rather than whenever the runtime finishes dropping. A mail server that cannot be
restarted immediately after quitting looks broken.

Verified live: closing the window removed it from the window list while `/api/v1/health` kept
answering `200`, and mail delivered with the window hidden still notified.

**Decision 4 — `notify-rust` with zbus, not Tauri's notification plugin.** The plugin exists to let
*frontend JavaScript* raise notifications across the IPC bridge; this shell raises them from Rust on
a bus subscription, so the bridge, its permissions file and its capability wiring would be machinery
in service of nothing. `zbus` over `libdbus` because it is pure Rust: no `libdbus-1-dev` on the
build machine, no matching shared library on the user's.

**Consequences.** A missing notification daemon is logged at debug and ignored — plenty of Linux
setups have none, and a mail server that refused to run because nothing was listening for toasts
would be absurd. Likewise a missing system tray: the app continues without one, since the window is
still reachable. The tray's SMTP address is copied through the WebView's clipboard rather than a
second clipboard integration, and the address is interpolated into that script through a JSON
string escaper rather than `format!`, because a value interpolated into JavaScript is exactly where
"it can only ever be digits and dots" stops being true one refactor later.

---

### ADR-094 — Linux packaging is built and verified; macOS and Windows installers are disclosed, not faked

**Status.** Accepted. Phase 9 A3.

**Context.** Phase 9's last criterion asks for "installers for macOS (`.dmg`) and Windows (`.msi`)".
Neither is reachable from this machine: `.dmg` requires macOS, `.msi` requires Windows, and both
require signing certificates. Producing unverified configuration for them and calling the criterion
met would be the failure mode this project has caught repeatedly — a document claiming what the
code does not do.

**Decision — build and *verify* what this machine can, and disclose the rest.** `.deb` and
AppImage are produced by `cargo tauri build`, and both were run end to end: the binary extracted
from the `.deb` bound all four listeners, opened its window, and accepted mail over SMTP that then
appeared in the UI; the AppImage did the same. Verification is the point — a bundle that exists is
not a bundle that works.

macOS and Windows are recorded as an environmental gap in the same terms Phase 5 used for
Thunderbird and `mutt`: the configuration is *not* written, because untested packaging
configuration is a liability that looks like progress.

**Building it found two real defects, neither visible without inspecting the package:**

1. **No icon was installed.** The generated `.desktop` entry declared `Icon=sandesh-desktop` while
   the package shipped no icon at all, so every launcher would have shown a blank tile. Tauri only
   installs icons listed in `bundle.icon`, and that key was absent. Four sizes are now generated
   from one 512px master and land in the freedesktop hicolor theme.

2. **The declared GTK dependency is named for an older Debian.** The bundler emits
   `Depends: libgtk-3-0`, but Ubuntu 24.04 and later renamed that package `libgtk-3-0t64` in the
   64-bit `time_t` transition. `apt` resolves it through `Provides:`, so installation works — but
   the name is worth knowing before someone reports it as a broken dependency. Left as the bundler
   emits it rather than hand-patched: overriding it would break the older distributions where
   `libgtk-3-0` is still the real package name.

**Consequences.** The `.deb` is 13 MB and depends on the system WebKit; the AppImage is 85 MB and
carries its own, which is the trade each format exists to make. Neither is signed — there is no
signing identity here, and an unsigned artifact that says so is more honest than a self-signed one
that implies provenance it does not have.

---

### ADR-095 — The container is `scratch` plus one static binary, and the smoke test runs it

**Status.** Accepted. Phase 10, first increment.

**Context.** Phase 10 asks for multi-arch images "on a minimal base", and states three criteria the
image must meet: it starts, accepts mail, serves the UI, and reaches a working state in under 60
seconds.

**Decision 1 — `scratch`, not Alpine, and therefore a static musl binary.** The runtime image
contains the binary, the certificate roots and nothing else: no shell, no package manager, no libc.
A process compromised through a mail parser has nothing to escalate with and nothing to fetch more
with. The image is **20.6 MB** for a complete SMTP/IMAP/POP3/HTTP server with the UI embedded.

The cost is that the binary must be statically linked, which is why the build stage is
`rust:1-alpine` rather than the Debian default. That is a real constraint rather than a preference:
a glibc binary on `scratch` fails at exec with no diagnostic worth reading.

**Decision 2 — the frontend builds first, because `rust-embed` demands it.** `web/dist` is compiled
*into* the binary (`assets.rs`), so a Rust-first Dockerfile would embed whichever `web/dist`
happened to exist in the build context — or fail outright when none does. The multi-stage order is
forced by that, not chosen for tidiness.

Building it found a non-obvious coupling: `.dockerignore` excluded `docs/`, and the frontend build
then failed because `web/src/api/openapi.test.ts` imports `docs/openapi.json` and `tsc -b`
type-checks tests. **The generated contract is a build input**, whatever its path suggests, so it is
explicitly un-ignored and copied into the web stage.

**Decision 3 — `0.0.0.0` inside the container, which is the one place it is correct.** The
container's network namespace *is* the boundary, and a loopback bind would make every published
port unreachable. This is the exact opposite of the desktop shell's rule (ADR-092), and for the
same underlying reason: bind to the edge of whatever isolation already exists.

**Decision 4 — no `HEALTHCHECK`.** It would need a shell or a `curl` binary, and adding either costs
the property `scratch` exists for. Orchestrators probe `GET /api/v1/health`, which ADR-075 exempted
from authentication precisely so a liveness check has somewhere to point.

**The criteria are asserted by running the image, not by inspecting the Dockerfile.**
`scripts/docker_smoke.py` starts the container, waits for health, and then checks the embedded UI is
served, mail is accepted over SMTP and readable over the API, IMAP and POP3 serve the same message,
and mail survives a restart. A Dockerfile that builds proves only that the image exists; that the
static binary links correctly, that the embedded assets survived the multi-stage copy, and that
every listener comes up have **no other test**.

Measured start: **0.75 s** against the 60-second budget. The smoke test is mutation-checked — it
fails, naming the criterion, when pointed at an image that serves nothing.

**Consequences.** Multi-arch (`linux/arm64`) is not yet built: it needs QEMU emulation or a native
runner, and cross-compiling musl for aarch64 is a separate piece of work from making the image
correct on one architecture. Homebrew, Scoop and `.rpm` packaging, and release automation with
signatures, remain outstanding — they are listed under Phase 10 deliverables and none is closed by
this increment.

### ADR-096 — The release binary is the tested image's binary, and reproducibility is the gzip header

**Status.** Accepted. Phase 10, second increment.

**Context.** Phase 10 asks for "static binaries: Linux (x86_64, aarch64, musl), macOS (x86_64,
aarch64), Windows (x86_64)", "release automation with checksums and signatures", and sets two
criteria: "every target builds in CI and runs a smoke test", and "reproducible checksums published
per release". ADR-095 delivered the container image; none of the above existed. The 60-second
criterion had a second half — "or a downloaded binary" — that could not be tested because there were
no binaries to download.

**Decision 1 — the Linux targets are built by the Dockerfile's own build stage, not a second
`cargo build --target`.** `scripts/release.py` builds the `runtime` stage and copies `/sandesh` out
of it. The obvious alternative — invoking `cargo build --target x86_64-unknown-linux-musl` directly
— would be a *second description of the same build*, pinning the musl toolchain, the C dependencies
`ring` and `sqlx` need, and the frontend ordering all over again, and free to drift from the one
`docker_smoke.py` actually asserts against. Under this decision the shipped binary and the
smoke-tested image hold the same bytes rather than being two builds of the same source.

macOS and Windows have no such stage and are built with `cargo` on their own runners: cross-linking
them needs a platform SDK that cannot be redistributed. `release.py` accepts those targets so the CI
matrix can call it uniformly, and **refuses by name** on a host that cannot build them (exit 2)
rather than emitting something untested.

**Decision 2 — the smoke test runs the binary outside a container, and that is the whole point.**
`scripts/binary_smoke.py` verifies the published checksum, unpacks the archive, and runs the binary
on the host — whose libc is not the one it was linked against. `docker_smoke.py` structurally cannot
show this: it starts the binary in the same `scratch` filesystem it was built for, so a stray dynamic
link against something Alpine happens to provide would pass there and fail on a user's machine.
Running it on a glibc host is what turns "static binary" into a claim with a test behind it.

Verified here: the extracted binary is `static-pie linked`, and on this glibc host it reached
`/api/v1/health` in **0.02–0.27 s** against the 60 s budget, served the embedded UI, accepted SMTP
mail that became queryable over the API, and stopped on `SIGTERM`. Both assertions are
mutation-checked — a wrong checksum and an absent UI marker each fail the script with exit 1.

**Decision 3 — reproducibility failed on the gzip header, and the measurement is what found it.**
The first two releases produced different checksums despite normalising every tar entry's mtime,
uid, gid and owner name. Unpacking both showed the binary was already bit-identical **and so was the
tar layer**: the entire difference was gzip's own header timestamp, which `tarfile.open(mode="w:gz")`
stamps with the current time and offers no way to set. Writing an explicit
`gzip.GzipFile(mtime=0)` around a plain tar stream fixes it, and two independent builds now produce
byte-identical archives and one checksum.

This is the ROADMAP's "a rebuild did produce an identical image digest, which is a promising sign
rather than a claim" made into a claim — and it is worth noting the promising sign was *wrong about
the packaging*: the image digest was stable while the tarball around the same binary was not.

**Decision 4 — `--version` and `--help`, because a downloaded binary must say what it is.** The CLI
read no arguments at all: `sandesh --version` started a mail server and tried to bind port 8080.
That barely showed when the only entry point was `cargo run` from a checkout; from a release archive
it is the first impression, and the first thing anyone does with an unknown executable is ask it
what it is. Hand-matched rather than adding an argument parser for two strings, and both paths exit
before anything is opened — `tests/cli_flags.rs` asserts `--version` creates no file in its working
directory, which is what makes it safe to run on an installed binary.

Writing it produced a defect worth recording: the first version returned from inside the argument
loop, so `sandesh --version --nonsense` printed the version and exited 0 without ever examining the
second argument. Clippy's `never_loop` caught it — the lint was pointing at a real bug rather than a
style preference — and every argument is now validated before any is acted on. The regression has
its own test.

**Consequences.** The checksum format is `sha256sum -c`'s, so a user verifies a download with a tool
already on their machine. `aarch64-unknown-linux-musl` builds under QEMU in CI but is **not**
smoke-tested and was **not** verified locally: this host has no arm64 binfmt registered, and an
x86_64 runner cannot execute the binary it produces — disclosed rather than asserted, as Phase 9
disclosed `.dmg` and `.msi`. Signatures are still outstanding: checksums establish integrity against
corruption, not provenance against a malicious mirror, and signing needs a key and a policy for
holding it. Homebrew, Scoop, `.deb`/`.rpm` and `cargo install` remain outstanding, though they now
have a versioned archive and checksum to point at, which is what they were waiting for.

### ADR-097 — The server `.deb` is not the desktop `.deb`, and `apt install` is the test

**Status.** Accepted. Phase 10, third increment.

**Context.** Phase 10 lists "package distribution: Homebrew, `cargo install`, `.deb`/`.rpm`, Scoop".
Phase 9 already ships a `.deb` (ADR-094), which invites the assumption that this line is half done.
It is not the same artifact: that one is Tauri's bundle of the **desktop application**, which depends
on GTK and WebKit and installs a `.desktop` entry for a GUI. A machine with no display cannot use it.

**Decision 1 — a separate headless server package, with a systemd unit.** `scripts/package_deb.py`
installs `/usr/bin/sandesh`, a unit, and `/etc/sandesh/sandesh.env`, and declares **no dependencies
at all** — which is not a shortcut but the musl static build's whole payoff (ADR-096). ADR-094 found
the opposite error in the Tauri bundle, where a wrongly-named GTK dependency made the package
uninstallable; the risk here is the reverse, so it is tested by installing on a distribution the
binary was never built on.

The service runs as a system user with `ProtectSystem=strict` and the rest of the hardening set,
verified with `systemd-analyze verify` rather than by inspection. It is **not enabled or started on
install**: a mail server that begins listening the moment it is unpacked is a surprise, and the
defaults bind loopback only (ADR-005) so an operator must make a deliberate choice either way.

**Decision 2 — built by hand with `dpkg-deb`, not `cargo-deb`.** `cargo-deb` would read the manifest
and do most of this, at the cost of a build dependency that must be installed before a release can
be cut, and of describing the package's contents in a metadata table no other part of the build
reads. `dpkg-deb --build` takes a directory tree; the tree is the thing being shipped, written out
literally. The binary is unpacked from the release archive rather than recompiled, so the package and
the published tarball hold the same bytes rather than being a third artifact nothing smoke-tested.

**Decision 3 — the package is asserted by `apt install` in a real container.** `scripts/deb_smoke.py`
installs it on `debian:stable-slim`, then checks the files, the service user, the data directory's
ownership and mode, that the binary runs *as the unprivileged user from its installed path*, and that
`remove` keeps mail while `purge` removes it.

This earned its keep on the first run. The postinst used Debian's `adduser`, which is a Perl script
absent from minimal images: the install failed with `adduser: not found` and exit 127. `dpkg-deb
--info` would never have shown it, and neither would building the package on a full desktop system.
`useradd`/`groupadd` from `passwd` are used instead. Two further defects were found by inspecting
the built package rather than the script: every directory shipped 0775 from the build user's umask —
a group-writable directory in a package means any group member can replace the binary — and
`sandesh.env` shipped world-readable although it is where `SANDESH_API_TOKEN` goes. Both are now
normalised explicitly, and the data-preservation assertion is mutation-checked: a postrm that
destroys mail on `remove` fails the test.

**Consequences.** `.rpm` is **not built**. `rpmbuild` is unavailable here, and a spec file written
without ever running it is exactly the untested artifact this phase keeps declining to ship —
disclosed as Phase 9 disclosed `.dmg` and `.msi`. Homebrew, Scoop and `cargo install` remain
outstanding; the first two need a published release to point a formula at, which is release
automation's job rather than packaging's. `arm64` is supported by `--arch arm64` but is unverified
for the same reason its release target is (ADR-096): no arm64 emulation here.

### ADR-098 — The `.rpm` is built by `rpmbuild` in a container, which withdraws ADR-097's disclosure

**Status.** Accepted. Phase 10, fourth increment. **Supersedes ADR-097's `.rpm` consequence.**

**Context.** ADR-097 declined to ship a `.rpm`: `rpmbuild` is not installed here, and a spec file
written without ever running it is exactly the untested artifact this phase keeps refusing. That
reasoning was sound but the conclusion was premature — it treated "not on this host" as "not
reachable", when the `.deb` had just been verified by installing it in a container.

**Decision — run `rpmbuild` inside `fedora:41`.** The spec is executed by the real tool, and
`scripts/rpm_smoke.py` then installs the result on a clean Fedora and runs it. The host needs only
Docker, which the container image and `.deb` smoke tests already require. The reason for the earlier
refusal is removed rather than worked around, so the disclosure is withdrawn rather than restated.

The binary is unpacked from the release archive rather than recompiled, so the `.rpm`, the `.deb`
and the published tarball all carry the same bytes.

**What installing it found, none of which `rpm -qip` would have shown:**

- **A false glibc dependency.** rpm's ELF scanner saw an executable and emitted `rtld(GNU_HASH)`,
  which resolves to glibc — on a static-pie musl binary that is simply wrong, and it would drag
  glibc onto a system that has none. `AutoReqProv: no` with `__requires_exclude_from` removes it.
  The smoke test now fails if any `rtld`/`glibc`/`libc.so` requirement reappears, and that assertion
  is mutation-checked.
- **`systemd` as a hard requirement made the package uninstallable in a container**, which is a
  normal way to run a development mail server. `%{?systemd_requires}` is replaced by
  `Recommends: systemd`, and the scriptlets are guarded on `systemctl` existing.
- **`rpmbuild` runs as root**, so its build tree and output landed on the host owned by root, where
  the script could not clean them up. `_topdir` now lives inside the container and the result is
  `chown`ed back.

**A correction, recorded because the test was wrong rather than the package.** The first smoke test
asserted the config file always survives `rpm -e`. It failed — and the package was right. RPM
removes an *unmodified* config on erase, since it is byte-identical to what shipped and nothing is
lost, while a *modified* one is preserved as `.rpmsave`. The test now asserts that guarantee instead.

**An honest gap.** `%config(noreplace)` is **not pinned by any test.** It diverges from plain
`%config` only when the shipped config *differs between versions*, which reinstalling a package over
itself cannot produce: both spellings pass the upgrade check. Verified directly with a pair of
throwaway packages whose configs differed — `noreplace` kept the operator's file and parked the new
one as `.rpmnew`, while plain `%config` overwrote the live file and moved the edit to `.rpmsave`,
silently ending its effect. That difference matters, so the directive stays; pinning it needs two
real releases and waits for the second rather than being faked with a synthetic version.

**Consequences.** `aarch64` is supported by `--arch aarch64` and unverified, for the same reason as
every other arm64 target here (ADR-096). The package is unsigned: RPM supports GPG signatures
natively, so this is the same outstanding signing work as everywhere else, not an RPM-specific gap.
Homebrew, Scoop and `cargo install` remain outstanding.

### ADR-099 — The supply-chain policy becomes a gate, and the licence stops being an open question

**Status.** Accepted. Phase 10, fifth increment. **Resolves OD-9.**

**Context.** A production-readiness audit found the repository claiming two things it had not done.
`DEPENDENCIES §8` has specified a `cargo-deny` gate — licence allowlist, advisories, bans, sources —
since Phase 1, and nothing ran it. Every manifest declared `license = "Apache-2.0"` while the README
said the licence was undecided and pointed at OD-9. Both were paperwork gaps rather than engineering
ones, which is precisely why neither had been noticed: no test fails because a `LICENSE` file is
absent.

**Decision — Apache-2.0, with the canonical text committed.** OD-9 had leaned this way since Phase 1
for the patent grant, the deadline was "before first public release", and the manifests had already
committed to it. Writing the file is the resolution; the appendix carries `Copyright 2026 Heliconia
Solutions Pvt. Ltd.`

**Decision — four `cargo-deny` checks in CI, plus `npm audit` split by what ships.** The npm gate
fails only on `--omit=dev`: a vulnerability in vitest or the OpenAPI generator is worth fixing but is
not a vulnerability in Sandesh, and failing the build on one trains everyone to ignore the job. Dev
findings are reported without failing.

**What running it found, none of which reading the manifests would have shown:**

- **A live rustls vulnerability.** RUSTSEC-2026-0285: TLS 1.3 handshake messages accepted across
  encryption-level boundaries, against the 0.23.43 in the lockfile. Fixed in 0.23.45. The gate's
  first run was not a formality.
- **`rustls-pemfile` is unmaintained** (RUSTSEC-2025-0134, archived August 2025). Rather than record
  an `ignore`, the two call sites in `sandesh-proto::tls` moved to the `PemObject` trait in
  `rustls-pki-types`, which was already in the tree beneath `rustls`. The dependency is gone from
  `Cargo.lock` entirely — an advisory removed rather than suppressed.
- **`lazy_static` cannot be banned**, though DEPENDENCIES §7 rejects it. It arrives transitively
  via `tracing-subscriber` → `sharded-slab`. §7's rule governs our own manifests; encoding it as a
  ban would fail the build on someone else's dependency choice.
- **`wildcards = "deny"` is unusable here.** Internal `path` dependencies count as wildcards, and
  `allow-wildcard-paths` exempts them only for crates marked unpublished. The check is off rather
  than carrying a permanent exception list.

**MPL-2.0 is allowed, and the reasoning is recorded rather than assumed.** Four crates carry it —
`cssparser`, `selectors`, `dtoa-short`, `option-ext`. All arrive through proc-macro and codegen
paths (Tauri's `tauri-utils`, `rust-embed`'s `shellexpand`), so none links into a shipped binary;
verified with `cargo tree -i`. MPL-2.0 is file-level copyleft and obliges us to publish changes to
those files, of which we make none.

**Consequences.** Advisory checks now run against a database that moves independently of this
repository, so a pull request can begin failing on an advisory published after it was opened. That
is the intended behaviour, and the nightly schedule is what catches one published after a merge.
Dependabot was added alongside this gate and later removed deliberately: update PRs are not
automated, and the gate is what surfaces an advisory. Release provenance is covered by ADR-103;
key-based signing is still outstanding and is unaffected by any of this.

---

### ADR-100 — WebSocket `resume` gets a bounded replay window, and OD-19's leaning is overturned

**Status.** Accepted. **Resolves OD-19**, against its recorded leaning.

**Context.** `API_DESIGN §5` and `EVENT_MODEL §13` have both described a `resume`/`after_seq` frame
since Phase 3, and `crates/sandesh-api/src/ws.rs` had no handling for either. OD-19's leaning was
that it was not worth building: a client must handle `lagged` by refetching anyway, so it already
owns a recovery path, and a replay buffer adds a second retention question.

That reasoning holds for a *lagging* client and does not hold for a *disconnected* one. `lagged`
arrives on a live connection, where the client is present to act on it. A dropped WebSocket — a
laptop sleeping, a proxy idling out, a redeploy — produces no frame at all, and the client cannot
distinguish "nothing happened" from "everything happened while I was away". Its only safe move is to
refetch every view on every reconnect, which is the cost OD-19 assumed it was avoiding.

**Decision — a fixed-size window in `EventBus`, not durability.** `DEFAULT_REPLAY_CAPACITY` (1024)
events are retained in a `VecDeque` alongside the broadcast sender. The append happens inside the
existing `seq` critical section, so the window's order *is* the total order by construction rather
than by a second mechanism that could disagree with it. The window is read under its own mutex, so a
resuming connection walking it cannot stall a publisher.

This deliberately does **not** make the bus durable. It survives no restart, and ADR-085/086's
position stands: durability belongs to the one consumer that needs it — the webhook outbox — not to
the in-process bus. A window is a cache of recent history; a log is a different thing with a
different retention policy, and conflating them is what OD-19 was right to resist.

**Decision — a gap is reported, never papered over.** If `after_seq` is older than the oldest
retained event, the client gets a single `resume_gap` frame naming `oldest_seq`, and no events. The
alternative — replaying whatever survives — is worse than sending nothing: a client handed a partial
history has no way to know it is incomplete and will believe it is current. `resume_gap` puts it back
on the refetch path it already implements for `lagged`.

**Decision — `after_seq` is a `bus_seq`, not the per-connection `seq`.** The frames carry both. The
per-connection counter restarts at 1 on every connection, so it cannot identify a position across the
reconnect that is the entire point of resume. Replayed frames are additionally marked
`"replayed": true`, so a client can distinguish history from live traffic without inferring it from
timing.

**Tests.** The window's semantics are asserted directly in `sandesh-events`: replay after a given
seq, the empty replay of an already-current caller (which must not be reported as a gap), the
eviction boundary where the oldest retained event is exactly the next one needed, bounded growth
under 50 events in a 3-slot window, and that replay order equals the gapless total order. The
transport's own tests cover filter application during replay, continuation of the connection
sequence, and that a gap yields exactly one frame carrying no events.

**Cost.** 1024 events of identifiers and minimal context — events never carry message bodies
(`EVENT_MODEL §1`), so the window is bounded regardless of message size. Retention can be disabled
entirely with `EventBus::with_capacities(_, 0)`, which restores the pre-ADR-100 behaviour.

**The client half.** `web/src/api/events.ts` tracks the highest `bus_seq` it has processed and sends
`resume` on every reopen after the first. Three details that are easy to get wrong, each pinned by a
test:

- The mark advances with `Math.max`, never assignment. Replayed and live frames share one code path,
  and browser arrival order is not a promise (§28), so a late or overlapping frame must not move the
  resume point backwards.
- It is **not** reset by `disconnect()`. A hidden tab closes the stream and a shown one reopens it,
  which is exactly when resuming beats refetching.
- `resume_gap` does **not** advance the mark. The server sent no events, so the next reconnect asks
  from the same point and succeeds if the window has meanwhile caught up. Advancing it would skip
  the events permanently.

`resume_gap` is delivered through the same listener as `lagged`, because both mean "refetch" and a
consumer should not need a second code path for a state it already handles. The listener now carries
a `ResyncNotice` naming which of the two occurred, rather than a bare count that a gap cannot supply.

**Why an integration test exists for this.** The Rust unit tests drive the bus directly and the
TypeScript ones drive a stub socket, so neither can show the two halves agree on the wire — which is
the only thing that makes resume work. `websocket_resume_replays_events_missed_while_disconnected`
in `tests/cross_protocol.rs` delivers mail over real SMTP while nobody is listening, reconnects a
real WebSocket, and asserts the missed event is replayed and names the right message. Writing it
immediately found that one delivery publishes several events, so resuming from "the first frame that
arrives" points into the middle of a delivery — a mistake the unit tests on both sides were shaped
not to notice.

---

### ADR-101 — Status colours are tuned against the surface they are used on, and the browser asserts it

**Status.** Accepted.

**Context.** Phase 4 listed three accessibility criteria — axe clean, AA contrast in both themes,
usable at 375px — and none had a test. Closing that gap was expected to be paperwork. Two of the
three failed on the first run.

**Decision — split the suite by what each environment can honestly answer.** Structural rules
(roles, names, labels, landmarks, ARIA validity) run under jsdom in `web/src/test/a11y.test.tsx`,
where they are fast and cover every principal view. `color-contrast` and `target-size` are
**disabled there** and run in `web/e2e/a11y.spec.ts` against a real engine instead. jsdom resolves
neither the cascade nor layout, so those two rules there return "incomplete" — which reads as a pass
and would have hidden exactly the defects below. A gate that cannot fail is worse than no gate,
because it is credited as coverage.

**Decision — status colours are tuned against their own tint over a pane, not against white.** The
light theme's `--accent`, `--ok`, `--warn`, `--error` and `--info` were each set for AA on white.
They are almost never used on white: `.connection`, log level badges and callouts paint the colour
*on* its own `--*-muted` tint, and that tint sits on `--bg-raised` rather than on the page. Measured
where they are actually used, four of the five were below AA — accent 4.17, ok 4.23, warn 4.28 and
error 3.89:1. All five were re-derived to clear **4.6:1** against that worst case, a margin chosen so
rounding cannot flip one back under. This is the same worst-case rule `--text-tertiary` had already
been given; it had simply never been applied to the status colours.

The failures were 4.43 and 4.49:1 against a 4.5 threshold. That is the interesting part: they are
invisible to inspection, survive any amount of design review, and are only ever found by measurement.

**Decision — the 375px criterion asserts reachability, not just the absence of a scrollbar.** The
header's status cluster is `flex: none` by deliberate design — the controls and the connection state
must never be compressed away — which makes it a place where anything added must earn its width. The
workspace switcher added in `bf1f369` carried a `max-width: 200px` trigger at every breakpoint and
pushed the theme toggle 25px off a 375px viewport. Hiding the switcher would have stranded the
workspace it names, so it shrinks below 480px instead. The suite additionally opens the nav drawer
and asserts every destination is listed, because the regression this criterion exists to catch —
`.shell__nav` once being `display: none` with nothing replacing it — produces no overflow at all.

**Decision — the browser suite pins a 1440px viewport.** Playwright's default is exactly 1280px, and
`@media (max-width: 1280px)` is inclusive, so the default viewport sat *inside* the drawer
breakpoint. Seven `mail.spec.ts` tests were failing on "element is outside of the viewport" — a
property of the harness, not of the app, and each one burning a 30s timeout. Pinning the viewport
fixed them and took the suite from 3.7 minutes to 45 seconds. Specs wanting a narrow layout opt in
with their own `test.use({ viewport })`.

**Decision — the e2e ports are overridable.** `SANDESH_E2E_HTTP_PORT` and `SANDESH_E2E_SMTP_PORT`
default to 8080 and 1025. 8080 is a popular port, and a developer with something else on it could
not run the suite at all; the failure ("Address already in use") reads as a broken test rather than
an occupied port. CI leaves them unset.

**Both gates were mutation-checked.** The jsdom suite was made to fail with an unlabelled input; the
contrast gate by restoring one old token (4.40:1); the viewport gate by restoring the 200px trigger
(9px of overflow). A test that has never been seen to fail has not been shown to test anything.

---

### ADR-102 — Webhook payloads are signed with HMAC-SHA256, and the secret is shown once

**Status.** Accepted. Phase 8 Pass B.

**Context.** A webhook is an unauthenticated POST arriving at a URL the receiver exposed. Without a
signature the receiver cannot distinguish a Sandesh notification from anyone else's POST to the same
address, so the only safe thing it can do with the payload is ignore it and re-fetch over the API —
which defeats the point of being pushed to. The Phase 8 plan listed this as the Pass B candidate with
a real argument behind it: signing matters if anyone points a webhook at anything real.

**Decision — signing lives in `sandesh-core`, not in `sandesh-webhook`.** Computing an HMAC is a pure
function over bytes: no I/O, no runtime, no transport. It belongs beside `RetryPolicy::schedule`,
which is in core for the same reason. This also keeps the wire format in one place — a transport that
derived its own signature would be a second implementation that could drift from the first. The cost
is three new direct dependencies for core (`hmac`, `hex`, `subtle`), which `ARCHITECTURE §3.2` and
`tests/architecture.rs` are updated for together, as that test's comment requires.

`sandesh-webhook` may depend on `sandesh-core` and nothing else in the project (ADR-085), so reusing
`sandesh_proto::Secret` was not available. `WebhookSecret` is a separate newtype in core with the
same property: `Debug` and `Display` render `[redacted]`, so the key cannot reach a log through a
struct someone derived without thinking about it.

**Decision — the timestamp is inside the signed material.** The header is
`X-Sandesh-Signature: t=<unix>,v1=<hex>`, signing `"<t>.<body>"`. Signing the body alone — GitHub's
scheme — is simpler to verify, but it lets anyone who captured one delivery replay it verbatim
forever. Binding the timestamp lets a receiver reject anything outside its tolerated clock skew, and
a receiver that does not care can ignore `t` at no cost. `v1` names the scheme so a second one can be
added without a receiver guessing which it is looking at.

The signature is computed **per attempt**, not per entry: a retry carries the time it was actually
sent. Reusing the first attempt's signature would hand a skew-checking receiver a payload that looks
stale precisely when the system is already struggling.

**Decision — the secret is stored in clear, and returned exactly once.** This is the one credential in
Sandesh that cannot be hashed at rest. Account passwords keep an Argon2id hash because verification
only ever needs a comparison; HMAC needs the key itself, so a one-way hash would leave nothing to
sign with. The compensating controls are the ones Stripe, GitHub and Shopify settled on: `POST
/api/v1/webhooks` returns `secret` in its response and no other endpoint ever will, reads report
`has_secret: bool`, and the value never reaches a log.

`CreatedWebhookDto` is a separate type from `WebhookEndpointDto` for that reason. Giving the read type
an optional `secret` populated on exactly one path would make every consumer handle a value they
cannot otherwise obtain, and would put the key one forgotten `null` check away from a listing.

**Decision — unsigned stays the default.** `sign` is opt-in. An endpoint pointed at a local test
receiver has nothing to spoof, and requiring a key there would add a step to the case Sandesh exists
to serve. A caller-supplied secret is accepted, for pointing Sandesh at a receiver that already knows
one; an empty string is rejected rather than silently signing with a zero-length key.

**Tests.** The algorithm is pinned against an **independent implementation** — a vector produced by
Python's `hmac`, which is what a consumer is most likely to verify with. A signer that only ever
agrees with its own verifier can be consistently wrong while every round-trip test passes. Beyond
that: tampering with the payload, the secret or the timestamp each break verification; malformed
headers are rejected rather than panicking, since a verifier is reachable from untrusted input;
comparison is constant-time via `subtle`, because a byte-by-byte `==` on a digest leaks through
timing how much of a forgery was correct; and three delivery tests assert against a real HTTP server
that the header arrives, verifies against the body actually sent, is absent when unsigned, and does
not verify under a key that did not sign it.

---

### ADR-103 — Sandesh is published as open source, with releases driven by a tag

**Status.** Accepted. Phase 10, release automation increment.

**Context.** Heliconia approved publishing Sandesh as open source. An audit
found the code ready but the project around it not: no `NOTICE`,
no third-party licence texts in any shipped artifact although every MIT, BSD and Apache dependency
requires them, an `.rpm` declaring `License: MIT`, no contribution terms, and CI that built every
artifact but published none. The private history also held captured mail blobs and internal files.

**Decision — a fresh public history.** The public repository starts from one commit; the private
history is archived as `HeliconiaIO/Sandesh-Internal`. A rewrite was not enough, because GitHub keeps
every pull request's commits reachable through `refs/pull/*`, which only GitHub can delete.
Contributors before the release are credited in `CREDITS.md`.

**Decision — every artifact carries its licences.** `scripts/third_party_notices.py` generates
`THIRD_PARTY_LICENSES.txt` from `cargo about` (crates), `web/node_modules` (the npm packages in the
bundle) and a fixed section for what neither graph sees: musl libc, the Rust standard library,
SQLite and the container's Mozilla CA bundle. It ships with `LICENSE` and `NOTICE` in every archive,
in the `.deb` and `.rpm`, and under `/licenses` in the image. Its `--check` mode is the npm
counterpart of cargo-deny's licence gate, using the same allowlist. Contributions are accepted under
the DCO, enforced on pull requests.

**Decision — releases are a tag.** Pushing `vX.Y.Z` runs `.github/workflows/release.yml`: it refuses
a tag that disagrees with `Cargo.toml` or has no `CHANGELOG.md` section, then publishes all five
binaries, the `.deb` and `.rpm`, a multi-arch image to `ghcr.io/heliconiaio/sandesh`, CycloneDX
SBOMs, `SHA256SUMS` and a build-provenance attestation per artifact, with the changelog section as
the release notes. Attestation answers "who built this, from which commit" without a long-lived
signing key, which is why key-based signing stays open rather than blocking the first release.
`RELEASING.md` holds the procedure.

**Decision — CI matches what it tests.** The first public CI run found what the private repository's
blocked runner never had: a real `mail-parser` panic (guarded in `sandesh-core`, reproducer under
`fuzz/artifacts/`), a desktop crate that needed a built UI to compile, a retired `macos-13` runner,
and a smoke test that stripped Windows' `SYSTEMROOT`. The fix that changes how the project works is
`rust-toolchain.toml`: CI had tracked the newest stable, so Rust 1.99's new `assert_is_empty` lint
failed a build no commit had changed. The toolchain is now pinned and upgraded deliberately. arm64
moved from QEMU to native runners, which cut those jobs from over half an hour to minutes and made
the arm64 binary smoke-testable, closing ADR-096's disclosed gap.

**Consequences.** `main` is public and permanent: history is no longer rewritten. Each Rust upgrade
is a commit that bumps `rust-toolchain.toml` and fixes what the new clippy finds. Outstanding:
Homebrew, Scoop, `cargo install`, key-based signing, and pinning actions to commit SHAs.
