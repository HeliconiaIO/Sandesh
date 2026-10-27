# Sandesh — Dependency Evaluation

**Status:** Adopted. The workspace manifests are the live record of what is actually depended
on; this document records *why* each choice was made. `Cargo.toml` wins on any disagreement.

Metadata verified against the crates.io API on **2026-09-01**. Toolchain: Rust 1.97.1.

---

## 1. Selection policy

1. A dependency must earn its place. "It exists" is not a reason.
2. Prefer crates with recent releases, real adoption, and permissive licences (MIT/Apache-2.0).
3. Prefer pure-Rust over C bindings — static single-binary distribution across five platforms is a
   product requirement, and every C dependency threatens it.
4. Prefer narrow, composable crates over frameworks.
5. For the product's core competency (protocol servers), an unmaintained dependency is a *larger*
   risk than writing the code ourselves.
6. Every direct dependency is listed here with its reasoning. CI enforces this with `cargo-deny`.

## 2. The central finding

**Server-side Rust crates for SMTP, IMAP and POP3 do not exist at production quality.**

| Protocol | Candidate | Latest | Last release | Downloads | Assessment |
| --- | --- | --- | --- | --- | --- |
| SMTP | `mailin` / `mailin-embedded` | 0.6.5 / 0.8.3 | 2024-08-31 | 140k / 51k | Only 3 reverse deps. Sync-first, callback design. Thin AUTH/TLS story. Would need forking |
| SMTP | `samotop` | 0.13.2 | **2022-01-30** | 42k | Unmaintained for 4+ years. Built on `async-std` — wrong runtime |
| SMTP | `smtp-server` (kannader) | 0.1.0 | 2022-10-09 | 1.6k | Experimental, part of an unfinished MTA project |
| IMAP | — | — | — | — | **No server crate exists.** `imap`, `async-imap` are clients |
| POP3 | — | — | — | — | **No server crate exists at all** |

The layer *below* the servers, however, is in excellent shape — `imap-codec` and `mail-parser` are
both actively maintained, well-designed, and permissively licensed.

**Decision (ADR-002): implement the protocol servers ourselves**, on shared transport scaffolding
(`sandesh-proto`), using `imap-codec` for IMAP grammar and hand-written parsers for SMTP and POP3.

Against the four options in the brief:

| Option | Verdict |
| --- | --- |
| 1. Implement ourselves | **Chosen.** Full control; the state machine *is* the product |
| 2. Wrap an existing library | Rejected for SMTP (only `mailin`, whose design we would fight); impossible for IMAP/POP3 |
| 3. Adapt another implementation | Rejected. Porting GreenMail (Java) or Mailpit (Go) means inheriting foreign design plus licence entanglement |
| 4. Defer the capability | Rejected. IMAP is a headline capability, not an optional extra |

This is not an aesthetic preference for writing code. SMTP's server-side verb set is roughly a dozen
commands and POP3's is eleven — both are small, stable, well-specified line protocols. IMAP is the
genuinely large one, which is precisely why we take a mature codec for it rather than writing a parser.
And a *development* mail server has requirements a general-purpose MTA library actively opposes: it
must accept malformed mail rather than reject it, must never relay, and must expose full protocol
traces to the UI.

**Risk:** protocol implementation is the project's largest effort and its largest correctness risk.
Mitigated by the codec choice for IMAP, by phasing (SMTP → IMAP → POP3), and by testing real mail
clients against the server (TESTING_STRATEGY §6).

## 3. Protocol and mail crates

### `mail-parser` — **ADOPT**

| Field | Value |
| --- | --- |
| Purpose | RFC 5322 / MIME parsing |
| Version | 0.11.8 (2026-08-22) |
| Licence | Apache-2.0 OR MIT |
| Maintenance | Active; Stalwart Labs (a production mail server) |
| Adoption | 3.29M downloads |
| Coverage | RFC 5322, 2045–2049, 2047, 2231; 41 charsets including UTF-7 |
| Security | 100% safe Rust, **zero external dependencies** |
| Alternatives | `mailparse` (simpler, weaker charset support); `email-parser`; hand-rolled (unjustifiable) |

**Decision: adopt.** Zero dependencies for a parser handling untrusted input is a strong security
property. Byte-range access to parts is exactly what our raw-authoritative model needs — we can slice
the original bytes rather than re-serialise. The RFC 8621 body-structure model matches how the
inspector presents messages. Battle-tested inside a production mail server.

*Note:* it is the one third-party crate permitted in `sandesh-core`. MIME parsing is domain logic and
the crate has no I/O or runtime; justified in ARCHITECTURE §3.2.

### `imap-codec` + `imap-types` — **ADOPT**

| Field | Value |
| --- | --- |
| Purpose | IMAP4rev1/rev2 command and response parsing/serialisation |
| Version | 2.0.0-alpha.9 / 2.0.0-alpha.7 (2026-07-19) |
| Licence | MIT OR Apache-2.0 |
| Maintenance | Active |
| Adoption | 97k / 96k downloads |
| Coverage | IMAP4rev1, IMAP4rev2 and many extensions; misuse-resistant types |
| MSRV | 1.85 (we are on 1.99.1, pinned in `rust-toolchain.toml`) |

**Decision: adopt, pinned to an exact alpha version.**

The alpha status is the one real concern, and it is accepted deliberately: writing an IMAP parser is
a multi-month project with a long correctness tail, and this crate is the most complete IMAP
implementation in Rust. "Misuse-resistant types" means the type system prevents emitting a malformed
response — extremely valuable for a server whose whole purpose is protocol fidelity.

Mitigation: pin the exact version; confine all `imap-codec` types to `sandesh-imap`'s codec module so
a breaking alpha bump touches one module; contribute fixes upstream. Re-evaluate at Phase 5 start.
Alternatives: `imap-proto` (client-oriented, less complete) or writing our own (rejected — this is
where a mature dependency earns its keep most).

**`imap-next`** (0.3.4, 2026-02-18) — the sans-I/O flow layer above `imap-codec` — is **evaluated at
Phase 5, not committed now.** It may replace part of `sandesh-proto`'s IMAP session handling. Deferred
because the decision needs the real state machine in front of us.

### SMTP / POP3 codecs — **BUILD**

Hand-written parsers in `sandesh-smtp` and `sandesh-pop3`. Both grammars are line-oriented, small and
stable. Using `nom` was considered and rejected: the parsers are simple enough that hand-written code
is clearer, and it avoids a dependency for marginal benefit.

### `mail-builder` — **DEFER**

0.5.0, actively maintained. No need in v1: Sandesh receives mail, it does not compose it. Reconsider
if relay or a "compose test message" feature is added.

## 4. Runtime and infrastructure

| Crate | Version | Licence | Purpose | Decision |
| --- | --- | --- | --- | --- |
| `tokio` | 1.x | MIT | Async runtime | **Adopt.** The de facto standard; required by axum, sqlx, rustls |
| `axum` | 0.8.9 | MIT | HTTP routing | **Adopt.** 448M downloads, tokio-team maintained, plain-function handlers, integrates with tower |
| `tower-http` | 0.7.1 | MIT | Middleware: CORS, compression, tracing, static files | **Adopt** |
| `sqlx` | 0.9.0 | MIT/Apache-2.0 | SQLite access | **Adopt.** Compile-time-checked SQL against the real schema; async; pure-Rust SQLite driver — no C toolchain, which matters for cross-compilation |
| `rustls` + `tokio-rustls` | 0.23.43 / 0.26.4 | Apache/ISC/MIT | TLS | **Adopt.** Pure Rust; avoids OpenSSL, which would break static single-binary distribution |
| `serde` / `serde_json` | 1.0.229 | MIT/Apache-2.0 | Serialisation | **Adopt.** Unavoidable and correct |
| `tracing` + `tracing-subscriber` | 0.1.44 | MIT | Structured logging | **Adopt.** Async-aware spans; log crates cannot express per-connection protocol context |
| `thiserror` | 2.x | MIT/Apache-2.0 | Error derives in libraries | **Adopt** |
| `anyhow` | 1.x | MIT/Apache-2.0 | Error handling in `sandesh-app` only | **Adopt**, binary crate only |
| `uuid` (v7) | 1.x | MIT/Apache-2.0 | Identifiers | **Adopt.** v7 is time-ordered — better index locality than v4 |
| `time` | 0.3 | MIT/Apache-2.0 | Timestamps | **Adopt** over `chrono`: smaller surface, no long-standing CVE history, adequate RFC 3339/2822 support |
| `bytes` | 1.x | MIT | Zero-copy byte buffers | **Adopt.** Central to slicing raw messages without copying |
| `argon2` | 0.5 | MIT/Apache-2.0 | Password hashing | **Adopt** (Phase 7). RustCrypto; current best practice |
| `base64` | 0.22 | MIT/Apache-2.0 | SASL, MIME encoding | **Adopt** |
| `sha2` | 0.10 | MIT/Apache-2.0 | Message digests | **Adopt** |
| `async-trait` | 0.1 | MIT/Apache-2.0 | Async traits in ports | **Adopt**; revisit — native AFIT may make it removable |
| `tokio-util` | 0.7 | MIT | `CancellationToken`, codecs | **Adopt.** Graceful shutdown across all listeners |

### Database driver: SQLx vs alternatives

| Option | Assessment |
| --- | --- |
| **`sqlx`** | **Chosen.** Compile-time query verification against the real schema; async-native; pure-Rust SQLite driver enables clean cross-compilation |
| `rusqlite` | Mature and fast, but synchronous (needs `spawn_blocking` throughout) and bundles C SQLite |
| `diesel` | Excellent, but ORM/DSL abstraction fights a schema whose shape is dictated by IMAP semantics |
| `sea-orm` | Additional abstraction over sqlx; unnecessary |

sqlx 0.9.0 requires Rust 1.94; the pinned toolchain is 1.99.1.

### WebSocket

| Option | Decision |
| --- | --- |
| **`axum::extract::ws`** | **Chosen.** Built into axum, already a dependency, shares the router and middleware |
| `tokio-tungstenite` | Rejected as a *direct* dependency — axum uses it transitively. No reason to add a second WebSocket surface |

### Configuration

| Option | Assessment |
| --- | --- |
| **`figment` + `clap` + `serde`** | **Chosen.** figment layers defaults → file → env → CLI cleanly; clap is the standard for CLI |
| `config` | Comparable; figment's provider model composes better with clap |

`figment` 0.10.19 last released 2024-05-17. Acceptable: a mature, stable, feature-complete config
library needs no frequent releases, and the risk of a stale config crate is low and easily replaced.

### Static asset embedding

`rust-embed` — **Adopted in Phase 4** (8.x). Embeds the built React app so distribution stays one
file. During frontend development the Vite dev server is used instead, proxying `/api` and the
WebSocket to a running Sandesh, so there is no second asset-serving mode to maintain. `mime_guess`
accompanies it to map extensions to content types.

### API documentation and types

| Crate | Purpose | Decision |
| --- | --- | --- |
| `utoipa` | OpenAPI 3.1 generation from Rust types | **Adopted (Phase 4).** Keeps docs from drifting. See ADR-026 |
| `ts-rs` | TypeScript type generation from Rust types | **Rejected (Phase 4).** Superseded by generating TypeScript from the `OpenAPI` document. See ADR-027 |

**Deviation from the Phase 0 plan.** Both were originally to be adopted. Running both would put two
derive stacks on every DTO and two drift checks over the same types, with the possibility of the two
disagreeing — the exact failure generation exists to prevent. `openapi-typescript` produces the
frontend types from the document `utoipa` already emits, so one chain covers both needs.

### Authentication and TLS (Phase 4.5)

| Crate | Purpose | Decision |
| --- | --- | --- |
| `argon2` | Argon2id password hashing | **Adopted.** OWASP's recommended function; memory-hard, so a GPU gains far less than against PBKDF2. Brings `password-hash`, whose verifier is constant-time |
| `subtle` | Constant-time comparison | Adopted. For the comparisons that are not password hashes |
| `base64` | SASL payload encoding | Adopted. RFC 4954 exchanges are base64 |
| `rustls` + `tokio-rustls` | TLS | **Adopted.** No C toolchain, no `unsafe` in the handshake. OpenSSL would add a system dependency to a tool whose premise is that `cargo run` works |
| `rustls-pemfile` | PEM parsing | Adopted |
| `rcgen` | Self-signed certificate generation | **Adopted.** Nobody has a CA-signed certificate for `localhost`; see ADR-034 |

**Not adopted: any CRAM-MD5 or HMAC-MD5 crate.** The mechanism requires the server to hold a
recoverable secret, which is the exposure Phase 4.5 exists to remove. See ADR-032.

## 5. Testing dependencies

| Crate | Purpose | Decision |
| --- | --- | --- |
| `tokio-test` | Async test utilities | Adopt |
| `insta` | Snapshot testing for protocol transcripts and parsed structures | **Adopt.** Protocol responses are exactly what snapshots are good at |
| `proptest` | Property tests (UID monotonicity, flag ops, round-tripping) | Adopt |
| `cargo-fuzz` / `arbitrary` | Fuzz the MIME parser and protocol codecs | **Adopt.** Non-negotiable: they consume untrusted input, and "never panic" is a stated contract |
| `wiremock` | HTTP mocking for future webhooks | Defer to Phase 9 |
| `lettre` | **SMTP client** for cross-protocol tests | **Adopt (dev-dependency).** Testing our server with an independent client implementation is the point; using our own code both ends would prove nothing |
| `async-imap` | **IMAP client** for cross-protocol tests | **Adopt (dev-dependency).** Same reasoning. 1.4M downloads, active |
| `testcontainers` | Container-based integration tests | **Reject.** Sandesh has no external service dependencies. Adding Docker to the test loop is pure cost |

Using independent client crates as the test drivers is a deliberate correctness strategy: it makes
the cross-protocol test an *interoperability* test rather than a self-consistency test.

## 6. Frontend dependencies (Phase 4)

| Package | Purpose | Decision |
| --- | --- | --- |
| React 19 + TypeScript | UI framework | Adopt — specified by the brief; also the right Tauri-compatible choice |
| Vite | Build tool | Adopt. Fast, standard, simple static output for embedding |
| TanStack Query | Server state, caching, invalidation | Adopt. Removes hand-written cache logic that WebSocket invalidation would otherwise require |
| React Router | Routing | **Adopted (Phase 4), resolving OD-10.** Chosen over TanStack Router: the routing need is four routes deep-linkable by URL, and React Router covers it with the smaller API surface and no code generation step |
| `lucide-react` | Icons | Adopted. One consistent 16px set, tree-shaken |
| Tailwind CSS | Styling | **Not adopted.** See below |
| Radix UI primitives | Accessible unstyled components | **Not adopted in Phase 4.** No dialog, menu or popover is yet needed; the interactive surfaces built (listbox, tree, tabs) are implemented directly against the ARIA patterns and tested for role and selection state. Revisit when a genuine overlay appears |
| `dompurify` | Sanitise HTML previews | **Not adopted.** See below |
| A component framework (MUI, Chakra, …) | — | **Rejected.** Heavy, opinionated, fights a custom visual identity |
| `openapi-typescript` | Generate frontend types from the contract | Adopted (dev-dependency). See ADR-027 |
| Vitest + Testing Library | Component tests | Adopted (dev-dependency) |
| Playwright | Browser tests | Adopted (dev-dependency). Drives a real binary over real SMTP |

**Deviation — Tailwind.** `UI_UX_DIRECTION` §10 specifies a token system (spacing, type scale,
surfaces, semantic colour) that must be defined once and themed. That is expressed directly as CSS
custom properties in `web/src/styles/tokens.css`, which is where both themes are defined. Adding
Tailwind on top would mean two vocabularies for the same values and a build step that earns nothing
here; the styles are component-scoped and total ~25 KB.

**Deviation — DOMPurify.** Not used, and the sandbox is *stricter* than planned. The Phase 0 plan
paired DOMPurify with `sandbox="allow-same-origin"`. That combination is weaker than it appears:
`allow-same-origin` puts the message inside Sandesh's own origin, so it is the sanitiser alone
standing between attacker-controlled markup and the API, the event stream and local storage — and a
sanitiser is a bypass away from failing open.

Phase 4 instead uses an **empty** `sandbox` attribute, which denies every capability the
specification can deny, including script execution and same-origin access, plus a
`default-src 'none'` CSP inside the frame. Nothing needs to be sanitised because nothing can
execute, and the failure mode is a message that renders inertly rather than one that escapes. See
ADR-030; verified by a browser test that delivers a message attempting to reach `window.parent`,
rewrite the application's title, and write to its `localStorage`.

## 7. Deliberately rejected

| Crate/approach | Reason |
| --- | --- |
| `samotop` | Unmaintained since 2022; wrong async runtime |
| `mailin-embedded` | Low adoption, sync-first, would require forking |
| `openssl` | C dependency; breaks clean static cross-compilation. `rustls` is sufficient |
| `diesel` / `sea-orm` | ORM abstraction over a protocol-dictated schema |
| `actix-web` | Fine framework; axum aligns better with our tokio/tower stack |
| `chrono` | `time` is a smaller, cleaner surface for our needs |
| `lazy_static` | Superseded by `std::sync::OnceLock` |
| `testcontainers` | No external services to containerise |
| Any GraphQL crate | A second API contract to maintain, for no clear gain |

## 8. Supply-chain policy

- `cargo-deny` in CI (`deny.toml`, the `supply-chain` job): licence allowlist, RustSec advisories,
  duplicate-version detection, and **the dependency bans that enforce §7** — this is how the
  rejected-dependency list becomes mechanical rather than aspirational. Implemented in Phase 10
  (ADR-099); specified here from Phase 1 and unenforced until then, which is how a live `rustls`
  advisory and an unmaintained `rustls-pemfile` both sat in the lockfile unnoticed.
  - The allowlist is wider than the line above once claimed, because it is now derived from the
    resolved graph rather than from memory: it also admits `Zlib`, `0BSD`, `BSL-1.0`, `CC0-1.0`,
    `MIT-0`, `Unlicense` and `MPL-2.0`. Every one is permissive; `deny.toml` records why MPL-2.0 is
    acceptable here.
  - `lazy_static` is *not* banned despite §7 rejecting it: it arrives transitively via
    `tracing-subscriber`. §7 governs our own manifests.
- RustSec is checked on every pull request and nightly, by the same `cargo-deny` job — a separate
  `cargo-audit` would be a second tool reading the same database.
- `npm audit --omit=dev` gates the frontend; dev-tooling advisories are reported without failing,
  since they do not reach the shipped bundle. Both trees currently report **zero** advisories:
  wiring the gate up cleared four, the last by taking `vitest` from 3.2.7 to 5.0.1. That upgrade
  needed no test changes, but it removed a transitive `@types/node` that one component had been
  leaning on for `NodeJS.Timeout` in browser code — a latent error the gate exposed rather than
  caused.
- `Cargo.lock` committed (this is an application).
- Workspace-level `[workspace.dependencies]` so versions are declared once.
- `unsafe` forbidden workspace-wide (`#![forbid(unsafe_code)]`) unless a specific need is documented
  here first. None is currently anticipated.

## 9. Open decisions

- **OD-9** Project licence. Apache-2.0 (patent grant, matches GreenMail) vs MIT (simpler) vs AGPL.
  All chosen dependencies are permissive, so any is available. *Leaning: Apache-2.0.*
- **OD-10** Frontend router — Phase 4.
- **OD-11** Whether `imap-next` replaces part of `sandesh-proto`'s IMAP session layer — Phase 5.
- **OD-12** Whether `imap-codec` 2.0 leaves alpha before Phase 5. If not, re-confirm the pin and the
  isolation strategy.

---

## 10. Implementation status (Phase 1)

Versions resolved on 2026-09-01 against Rust 1.97.1. `Cargo.lock` is committed.

### `sandesh-core` — six dependencies, exactly as ARCHITECTURE §3.2 permits

| Crate | Resolved | Role |
| --- | --- | --- |
| `mail-parser` | 1.0.0 | MIME parsing; byte offsets drive the whole range-based model |
| `thiserror` | 2.0.20 | Error derives |
| `uuid` | 1.26.0 (v7) | Time-ordered identifiers |
| `time` | 0.3.55 | Timestamps |
| `sha2` | 0.10.9 | Message digests |
| `async-trait` | 0.1.92 | Port traits (ADR-011) — no runtime |

`tests/architecture.rs` asserts this list exactly: adding a seventh dependency
fails the build until ARCHITECTURE §3.2 and the test are updated together.

### `sandesh-storage`

| Crate | Resolved | Note |
| --- | --- | --- |
| `sqlx` | **0.9.0** | As specified. Features: `runtime-tokio`, `sqlite`, `migrate`, `macros` |
| `tokio` | 1.53.1 | `rt`, `sync`, `fs`, `macros` |
| `sha2`, `uuid`, `time`, `thiserror`, `async-trait` | — | Shared with the core |
| `tempfile` | 3.27.0 | dev-dependency |

**MSRV raised to 1.94.** sqlx 0.9.0 requires it. The alternative was pinning
sqlx 0.8.6 and diverging from the Phase 0 decision; raising the MSRV is the
smaller deviation. The toolchain in use is pinned in `rust-toolchain.toml` (1.99.1 since ADR-103).

**MSRV raised to 1.98.** mail-parser 1.0.0 requires it. 0.11.9 panicked on
malformed nested multiparts the `mime_parser` fuzz target kept finding; the
panic was caught, but each such message was stored unparsed, and the nightly
fuzz job failed on it. 1.0 parses them; the reproducers are in
`fuzz/artifacts/mime_parser/`.

### Not yet added

`figment`, `clap`, `axum`, `rustls`, `argon2`, `rust-embed`, `utoipa`, `ts-rs`,
`insta`, `proptest`, `lettre`, `async-imap` — all belong to later phases. None
was added speculatively.

`serde` is deliberately **absent from `sandesh-core`**: domain types are not
DTOs, and a test asserts the manifest stays free of it.

### Workspace lint policy

`unsafe_code = "forbid"` workspace-wide; no `unsafe` appears anywhere.
`clippy::pedantic` plus denied `unwrap_used`, `expect_used`, `panic` and `todo`
in production paths. Test modules carry narrow `allow`s, since panicking is how
a test reports failure.

---

## 11. Phase 2 dependencies

No new third-party crate was introduced for the SMTP server itself: ADR-002's decision to implement
the protocol in-house held, and the verb set is small enough that a parser dependency would have
added surface without removing work.

### New direct dependencies

| Crate | Version | Licence | Used by | Why |
| --- | --- | --- | --- | --- |
| `tokio-util` | 0.7.17 | MIT | `sandesh-proto`, `sandesh-smtp` | `CancellationToken` for graceful shutdown. Already a transitive dependency via `sqlx`; promoting it to direct adds nothing to the tree |
| `tracing` | 0.1.44 | MIT | `sandesh-smtp` | Structured connection and delivery logging. Recorded in §4 as adopted; Phase 2 is where it first appears |

Both were already evaluated in the Phase 0 table. Nothing unevaluated was added.

### Crate dependency matrix

| Crate | Depends on | Notably absent |
| --- | --- | --- |
| `sandesh-config` | `thiserror` | Everything else — it is read by every layer, so weight here propagates |
| `sandesh-proto` | `tokio` (net/io/time/sync), `tokio-util`, `thiserror` | Any protocol crate; any database crate |
| `sandesh-smtp` | `sandesh-core`, `sandesh-proto`, `sandesh-config`, `tokio`, `tokio-util`, `async-trait`, `thiserror`, `tracing` | **`sqlx` and `sandesh-storage`** — enforced by `tests/architecture.rs` |

`sandesh-smtp` takes `sandesh-storage` and `sqlx` as **dev-dependencies only**, for integration tests
that need a real database behind the ports. The architecture test walks runtime dependencies, so this
does not weaken the boundary.

### Phase 5 — IMAP

**No new third-party crates.** `sandesh-imap` is built entirely from dependencies the workspace
already had, which is the point: the parser is hand-written (ADR-002), and every security primitive
comes from `sandesh-proto` rather than a second copy.

| Crate | Runtime dependencies | Deliberately absent |
| --- | --- | --- |
| `sandesh-imap` | `sandesh-core`, `sandesh-proto`, `sandesh-config`, `tokio`, `tokio-rustls`, `thiserror`, `tracing` | **`sqlx`, `sandesh-storage`, `sandesh-smtp`, `imap-codec`, `imap-types`, `rustls`, `rcgen`, `rustls-pemfile`, `argon2`, `base64`, `subtle`** — all enforced by `tests/architecture.rs` |

`tokio-rustls` is present so `ImapServer::with_tls` can name `TlsAcceptor` and *receive* an acceptor
the composition root built from `sandesh_proto::Identity`. Using an acceptor is not configuring TLS:
`rustls`, `rcgen` and `rustls-pemfile` are banned as runtime dependencies precisely so certificate
loading cannot reappear here (ADR-034, and `imap_does_not_duplicate_tls_configuration`).

Dev-dependencies only: `sandesh-storage`, `sqlx`, `rustls`, `base64`, `tempfile`. The integration
tests drive a real server with a real `rustls` client and a real database behind the ports, so they
need all five — and because the architecture test walks *runtime* dependencies, having them does not
weaken any boundary.

### Rejected for Phase 2

| Option | Reason |
| --- | --- |
| `mailin` / `mailin-embedded` | Re-confirmed: last release 2024-08, 3 reverse dependencies, sync-first callback design. Adopting it now would mean fighting its shape for the parts Sandesh needs most — accepting malformed mail and exposing protocol traces |
| `samotop` | Still unmaintained since 2022, still on `async-std` |
| `nom` for command parsing | The SMTP grammar is roughly a dozen line-oriented verbs. Hand-written parsing is clearer here and avoids a dependency for marginal benefit. IMAP is the case where a parser crate earns its place, and `imap-codec` is already chosen for it |

---

## Phase 3 dependency audit

Five additions, all already evaluated in §§4–6 and adopted at the versions named there. No new
runtime was introduced and no competing framework was added — `tests/architecture.rs` now asserts
the latter mechanically.

| Crate | Version | Licence | Why | Features enabled |
| --- | --- | --- | --- | --- |
| `axum` | 0.8 | MIT | HTTP routing and WebSocket, per §4 | `http1`, `json`, `query`, `tokio`, `ws` — `default-features = false`. No multipart, no macros |
| `tower-http` | 0.6 | MIT | CORS, request-body limit, tracing | `cors`, `limit`, `trace` only |
| `serde` / `serde_json` | 1 | MIT/Apache-2.0 | API and event serialisation | `derive` |
| `futures-util` | 0.3 | MIT/Apache-2.0 | `SinkExt`/`StreamExt` for the split WebSocket | `std`, `sink` — no executor |
| `tracing-subscriber` | 0.3 | MIT | Log formatting and filtering in the binary | `fmt`, `env-filter`, `ansi` |

`tokio-tungstenite` 0.29 is a **dev-dependency only**, pinned to the version axum already pulls in
through its `ws` feature, so it adds nothing to the tree. It exists so the integration tests drive
the server with a real WebSocket client rather than a hand-rolled handshake.

**Still absent from `sandesh-core`:** `serde`, per §9. Domain types are not wire types, so the API
crate writes its DTOs out and converts explicitly. That is more code than a derive, and it is the
point — a field cannot enter the public JSON contract by being added to a domain struct.

**`utoipa` and `ts-rs`** were adopted in §7 for Phase 3 but are **not yet used**: both generate
artefacts for a consumer that does not exist until the Phase 4 UI, and a checked-in generated file
with no reader is a drift check that nobody notices failing. Moved to Phase 4.
