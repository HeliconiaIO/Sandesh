# Sandesh

[![CI](https://github.com/HeliconiaIO/Sandesh/actions/workflows/ci.yml/badge.svg)](https://github.com/HeliconiaIO/Sandesh/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/HeliconiaIO/Sandesh?include_prereleases)](https://github.com/HeliconiaIO/Sandesh/releases)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/HeliconiaIO/Sandesh/badge)](https://scorecard.dev/viewer/?uri=github.com/HeliconiaIO/Sandesh)

**A local-first Mail Development Environment for developers.**

Sandesh is a single, self-contained mail environment you run on your own machine. Point your
application's SMTP client at it, and inspect, search, and replay the resulting mail through a
modern developer console — over SMTP, IMAP and POP3, backed by one shared mail state.

> Run Sandesh locally and have a complete mail development/testing environment without needing an
> external mail provider.

---

## Status

**Usable today, version 0.1.0.** One binary starts SMTP, IMAP, POP3, an HTTP API, a live event
stream and the developer console, over one shared mail state. It ships as static binaries for
Linux, macOS and Windows, a container image, and `.deb`/`.rpm` packages.

Send a message with your application's SMTP client and watch it appear in the browser without a
refresh, then inspect its headers, MIME structure and raw bytes. Real clients work: `mbsync`
completes a full bidirectional sync, and Python `imaplib`'s `SEARCH UNSEEN` → `FETCH` →
`STORE \Seen` loop runs in CI.

Beyond storing mail, Sandesh helps explain it:

- **Diagnostics:** per-connection protocol transcripts, a delivery record for every transaction
  including rejections, a `Received:` hop timeline with DKIM/SPF/DMARC parsing (**parsed, never
  verified**), a Prometheus `/metrics` endpoint, an API console that can call every documented
  endpoint, and a live log stream.
- **Fault injection:** configurable SMTP failures, set at runtime and cleared by restart.
- **Webhooks** on delivery, with exponential-backoff retry and an outbox that survives restarts.
- **Message rules** that auto-tag at delivery and record which rule fired.
- **`BDAT`/CHUNKING**, for clients that prefer counted chunks to dot-stuffing.
- **Replay and bulk import/export:** re-deliver a stored message with its origin recorded, or move a
  whole corpus in and out as `mbox` (`mboxrd` framing, byte-for-byte round trips).
- **Link and image extraction:** every URL a message's HTML references, classified and grouped by
  host, and never fetched. A mail server that resolves links in mail it received is an SSRF risk
  and a delivery receipt for whoever sent it.

The same engine also runs inside a Tauri desktop app with a tray icon and new-mail notifications.
Every artifact is smoke-tested by *running* it: the container in its image, and each binary on a
host whose libc it was not linked against. Pushing a version tag publishes all of it as a GitHub
Release, with a multi-arch image on `ghcr.io/heliconiaio/sandesh`, SBOMs and build-provenance
attestations ([RELEASING.md](RELEASING.md)).

Every capability in this document is labelled with its real state:

| Label | Meaning |
| --- | --- |
| **Planned** | Agreed as in-scope, not yet specified in detail |
| **Designed** | Fully specified in `docs/`, not yet built |
| **Implemented** | Code exists and runs |
| **Tested** | Covered by automated tests in CI |

| Area | State |
| --- | --- |
| Architecture & crate boundaries | **Tested** — enforced by `tests/architecture.rs` |
| Core domain model | **Tested** — `sandesh-core` |
| MIME parsing & byte-range model | **Tested** — raw bytes authoritative |
| Storage schema (SQLite) + migrations | **Tested** — `sandesh-storage` |
| Message / mailbox / account model | **Tested** — independent per-mailbox UIDs and flags |
| Domain events | **Tested** — emitted by storage, published by the `sandesh-events` bus |
| Test fixture corpus | **Tested** — 102 `.eml` fixtures across five categories, guarded by a count test |
| **SMTP server** | **Tested** — [SMTP_DESIGN.md](docs/SMTP_DESIGN.md); verified against Python `smtplib` |
| **IMAP server** | **Tested** — RFC 9051 with UIDPLUS, MOVE, IDLE, CONDSTORE, ESEARCH, NAMESPACE, SPECIAL-USE and modified-UTF-7; real `mbsync` completes a bidirectional sync ([IMAP_DESIGN.md](docs/IMAP_DESIGN.md)) |
| **POP3 server** | **Tested** — RFC 1939 with UIDL and TOP support |
| **Developer Mail Composer & Reply** | **Tested** — in-browser composition and quick reply with real-time SSE |
| **Protocol scaffolding** | **Tested** — `sandesh-proto`, reusable by IMAP and POP3 |
| **Configuration** | **Tested** — `sandesh-config`; loopback-only by default |
| **REST API** | **Tested** — `sandesh-api`; read surface, flag mutation, and send endpoint ([API_DESIGN.md](docs/API_DESIGN.md)) |
| **WebSocket event stream** | **Tested** — `/api/v1/events`, server-side filtering, bounded per-client queues |
| **Event bus** | **Tested** — `sandesh-events`; total order, at-most-once, lag reporting |
| **Application / binary** | **Tested** — `sandesh-app`; `cargo run` starts everything |
| **Web UI** | **Tested** — React 19 + TypeScript, embedded in the binary; component and browser tests |
| **OpenAPI contract** | **Tested** — generated from the handlers, served at `/api/openapi.json` |
| **Generated TypeScript** | **Tested** — derived from the contract; CI fails on drift |
| **HTML email isolation** | **Tested** — sandboxed iframe, verified in a browser test |
| **Protocol trace viewer** | **Tested** — per-connection transcripts for SMTP, IMAP and POP3, **live and historical**: an open connection's transcript grows as the client speaks. Credentials redacted and `DATA` elided at capture |
| **Delivery explanation** | **Tested** — envelope, per-recipient outcome and routing for every transaction, rejections included |
| **Header analysis** | **Tested** — `Received:` hop timeline with per-hop delay; DKIM/SPF/DMARC and `X-Spam-*` **parsed and displayed, never verified** |
| **Metrics endpoint** | **Tested** — Prometheus text format at `/metrics`, derived from storage; authenticated like every other path |
| **API reference & console** | **Tested** — every documented endpoint listed and executable against the running server, derived from the served contract |
| **Log stream** | **Tested** — bounded ring plus live SSE at `/api/v1/logs`, filtered by level and target; carries no message content or credentials |
| **Webhooks** | **Tested** — HTTP notification on delivery, with retry and a permanent outbox audit trail. Payloads are optionally signed with HMAC-SHA256 (`X-Sandesh-Signature: t=…,v1=…`); the timestamp is inside the signed material, so a captured delivery cannot be replayed ([INTEGRATION_GUIDE.md](docs/INTEGRATION_GUIDE.md#5-webhooks--reacting-to-delivered-mail)) |
| Integration & Testing Guide | **Available** — [INTEGRATION_GUIDE.md](docs/INTEGRATION_GUIDE.md) |
| UI/UX direction | Designed — [UI_UX_DIRECTION.md](docs/UI_UX_DIRECTION.md) |
| Desktop (Tauri) | **Tested** — `sandesh-desktop` embeds the engine in-process with a tray icon and new-mail notifications; `.deb` and AppImage built and run-verified; not yet published by releases, so build them with `cargo tauri build` |
| Container image | **Tested** — 20.6 MB `FROM scratch`, one static binary; `scripts/docker_smoke.py` runs it in CI |
| Release binaries | **Tested** — five targets built in CI with reproducible `sha256` checksums; `scripts/binary_smoke.py` unpacks and runs the artifact outside any container — each on a native runner, arm64 included |
| Debian package | **Tested** — headless server plus a systemd unit, no dependencies; `scripts/deb_smoke.py` installs it on `debian:stable-slim` and runs it as the service user |
| RPM package | **Tested** — the same server and unit built by `rpmbuild` in a container; `scripts/rpm_smoke.py` installs it on `fedora:41`, and fails if rpm reintroduces the false glibc dependency its ELF scanner invents for a static binary |
| Release automation | **Built** — a `vX.Y.Z` tag publishes binaries, packages, a multi-arch GHCR image, SBOMs, `SHA256SUMS` and build-provenance attestations ([RELEASING.md](RELEASING.md)) |
| Package distribution | Planned — Homebrew, Scoop, `cargo install`; key-based signing not yet built |

> 📖 **Looking for setup instructions?** Check out the [End-to-End Guide](docs/E2E_GUIDE.md) for complete instructions across all 6 targets and mail protocols, or the [Integration & Testing Guide](docs/INTEGRATION_GUIDE.md) for connecting **Odoo ERP**, Python, Node.js, PHP/Laravel, and desktop email clients.

### Download a binary

No toolchain, no runtime, no container. Each release publishes one static binary per platform with
a checksum beside it:

```bash
# Pick your platform's archive from the release page, then verify and unpack it.
sha256sum -c sandesh-0.1.0-x86_64-unknown-linux-musl.tar.gz.sha256
# or check everything you downloaded against the release's combined list:
sha256sum -c SHA256SUMS --ignore-missing
tar xzf sandesh-0.1.0-x86_64-unknown-linux-musl.tar.gz
./sandesh --version   # confirm what you downloaded
./sandesh             # start it
```

| Platform | Archive |
| --- | --- |
| Linux x86_64 | `sandesh-<version>-x86_64-unknown-linux-musl.tar.gz` |
| Linux aarch64 | `sandesh-<version>-aarch64-unknown-linux-musl.tar.gz` |
| macOS Intel | `sandesh-<version>-x86_64-apple-darwin.tar.gz` |
| macOS Apple silicon | `sandesh-<version>-aarch64-apple-darwin.tar.gz` |
| Windows x86_64 | `sandesh-<version>-x86_64-pc-windows-msvc.zip` |

The Linux builds are statically linked against musl, so they run on any distribution regardless of
its glibc version — verified by running them on a glibc host, not merely by how they were compiled.
The checksums are reproducible: two independent builds of the same commit produce byte-identical
archives, so a mismatch means the file changed in transit rather than that the build moved. Each
archive also contains `LICENSE`, `NOTICE` and `THIRD_PARTY_LICENSES.txt`. To check who built a file
and from which commit: `gh attestation verify <file> -R HeliconiaIO/Sandesh`.

To build one yourself — the same script CI runs, so a release never depends on a path only CI
takes:

```bash
python3 scripts/release.py --target x86_64-unknown-linux-musl   # writes dist/
python3 scripts/binary_smoke.py dist/sandesh-*.tar.gz           # verify + run it
```

### Debian and Ubuntu

```bash
sudo apt install ./sandesh_0.1.0_amd64.deb
sudo systemctl enable --now sandesh

# Or, with no network at all — the package needs no dependency resolution:
sudo dpkg -i sandesh_0.1.0_amd64.deb
```

Installs the **headless server** and a systemd unit running as an unprivileged `sandesh` user, with
mail under `/var/lib/sandesh` and settings in `/etc/sandesh/sandesh.env`. It depends on nothing —
the binary is static — and it is not started on install, because a mail server that begins listening
the moment it is unpacked is a surprise. `apt remove` keeps your mail; only `apt purge` deletes it.

This is *not* the desktop application's `.deb`, which is a GUI app needing GTK and WebKit.

```bash
python3 scripts/package_deb.py    # build it
python3 scripts/deb_smoke.py      # install and run it in a Debian container
```

### Fedora, RHEL and openSUSE

```bash
sudo rpm -i sandesh-0.1.0-1.fc41.x86_64.rpm
sudo systemctl enable --now sandesh
```

The same headless server, unit and paths as the Debian package. `systemd` is *recommended* rather
than required, so the package also installs in a container where the binary runs directly. An edited
`/etc/sandesh/sandesh.env` is kept across upgrades and saved as `.rpmsave` if you erase the package.

```bash
python3 scripts/package_rpm.py    # build it (rpmbuild runs in a Fedora container)
python3 scripts/rpm_smoke.py      # install and run it in a Fedora container
```

### Running from source

```bash
(cd web && npm ci && npm run build)   # once, and after UI changes
cargo run
```

The Web UI is compiled into the binary, so there is no second server to start — but it has to be
built first. Without `web/dist`, Sandesh still runs and the API works, and the browser gets a page
explaining that the UI was not built. Sandesh prints where it is listening and starts serving:

```text
Sandesh 0.1.0 starting...
SMTP:      127.0.0.1:1025
IMAP:      127.0.0.1:1143
POP3:      127.0.0.1:1110
HTTP:      http://127.0.0.1:8080/api/v1
WebSocket: ws://127.0.0.1:8080/api/v1/events
Database:  sandesh.db
Blobs:     sandesh-blobs
Auth:      disabled · TLS: disabled
```

### Docker

```bash
docker run -d -p 1025:1025 -p 1080:1080 -p 1143:1143 -p 1110:1110 \
  -v sandesh-data:/data ghcr.io/heliconiaio/sandesh:latest
```

The official image is multi-arch (`linux/amd64` and `linux/arm64`) and published by every release;
pin a version tag such as `:0.1.0` in CI. Inside the container the HTTP port is **1080**, not 8080.
To build it yourself instead: `docker build -t sandesh .`

A 20.6 MB image built `FROM scratch`: one static binary with the UI embedded, and no shell or
package manager inside it. It reaches a working state in well under a second; point your
application's SMTP client at `localhost:1025` and open <http://localhost:1080>.

Mail lives in the `/data` volume, so it survives a restart. `scripts/docker_smoke.py` runs the
image and asserts all of that, and CI runs it on every push.

Then open **<http://127.0.0.1:8080>** for the developer console: mailboxes, a live message list,
and an inspector with rendered content, headers, MIME tree, raw source and attachments. Mail
appears the moment it arrives — there is no refresh button.

Everything binds to **loopback only**. Ctrl-C (or `SIGTERM`) shuts down cleanly.

By default the database is `sandesh.db` and message blobs are `sandesh-blobs/`, both relative to the
working directory. Every path and port can be moved with an environment variable — a missing data
directory is created:

| Variable | Effect | Default |
| --- | --- | --- |
| `SANDESH_DATA_DIR` | Puts both stores under one directory | the working directory |
| `SANDESH_DATABASE_PATH` | The database file | `sandesh.db` |
| `SANDESH_BLOB_PATH` | The blob store | `sandesh-blobs` |
| `SANDESH_SMTP_ADDR` | SMTP listener, as `host:port` | `127.0.0.1:1025` |
| `SANDESH_IMAP_ADDR` | IMAP listener, as `host:port` | `127.0.0.1:1143` |
| `SANDESH_IMAP_TLS_ADDR` | IMAP implicit-TLS listener; setting it enables the port | off |
| `SANDESH_IMAP_ENABLED` | Whether the IMAP listener runs | `true` |
| `SANDESH_IMAP_AUTH` | `disabled`, `optional` or `required` | `disabled` |
| `SANDESH_IMAP_STARTTLS` | Whether `STARTTLS` is offered on the IMAP cleartext port | `false` |
| `SANDESH_POP3_ADDR` | POP3 listener, as `host:port` | `127.0.0.1:1110` |
| `SANDESH_POP3_TLS_ADDR` | POP3 implicit-TLS listener; setting it enables the port | off |
| `SANDESH_POP3_ENABLED` | Whether the POP3 listener runs | `true` |
| `SANDESH_POP3_AUTH` | `disabled`, `optional` or `required` | `disabled` |
| `SANDESH_POP3_STARTTLS` | Whether `STLS` is offered on the POP3 cleartext port | `false` |
| `SANDESH_HTTP_ADDR` | HTTP listener, as `host:port` | `127.0.0.1:8080` |
| `SANDESH_LOG` | Tracing filter, e.g. `sandesh_smtp=debug` | `info` |
| `SANDESH_SMTP_ENABLED` | Run the SMTP listener | `true` |
| `SANDESH_HTTP_ENABLED` | Run the HTTP listener | `true` |
| `SANDESH_SMTP_AUTH` | `disabled`, `optional` or `required` | `disabled` |
| `SANDESH_SMTP_ALLOW_CLEARTEXT_AUTH` | Accept `AUTH` on a connection without TLS | `false` |
| `SANDESH_SMTP_TLS` | `disabled`, `starttls` or `implicit` | `disabled` |
| `SANDESH_API_AUTH` | HTTP API auth: `disabled` or `required`. Governs the API only — `SANDESH_SMTP_AUTH` never did | `disabled` |
| `SANDESH_API_TOKEN` | Bearer token the API accepts. Basic auth against an account's password also works | *(unset)* |
| `SANDESH_API_TLS` | Serve the API over HTTPS, reusing the mail listeners' certificate | `false` |
| `SANDESH_TRACING_ENABLED` | Record a protocol transcript per connection | `true` |
| `SANDESH_TRACE_RETENTION` | Connection records kept before the oldest are evicted | `1000` |

A TLS mode with no usable certificate fails startup rather than falling back to plaintext.

Transcripts record the command exchange only. Message bodies become a byte count and credentials
are never written, so a trace cannot leak the password-reset links and one-time codes a
development mail server holds.

```bash
SANDESH_DATA_DIR=/var/lib/sandesh SANDESH_HTTP_ADDR=0.0.0.0:8080 cargo run
```

A variable that is set but unusable fails startup and names itself, rather than being silently
ignored — `SANDESH_HTTP_ADDR=8080` reports that a bare port is not an address and suggests
`127.0.0.1:8080`. Binding off loopback is deliberate and supported; Sandesh warns when you do,
because it accepts mail without authentication and, by default, serves the API the same
way. Bound off loopback, protect it:

```bash
SANDESH_HTTP_ADDR=0.0.0.0:8080 SANDESH_API_AUTH=required \
  SANDESH_API_TOKEN=$(openssl rand -hex 32) SANDESH_API_TLS=true cargo run
```

Then `curl -H "Authorization: Bearer $TOKEN" https://host:8080/api/v1/messages`, or
`curl -u user:password`. Only `/api/v1/health` is reachable without a credential, so a container
healthcheck still works.

Check it is alive, and see the connection details:

```bash
curl http://127.0.0.1:8080/api/v1/health        # {"status":"ok","version":"0.1.0"}
curl http://127.0.0.1:8080/api/v1/health/ready  # also checks the database
curl http://127.0.0.1:8080/api/v1/info          # addresses, database location
```

Point your application's SMTP client at `127.0.0.1:1025` — no authentication, no TLS — then
assert on what arrived:

```bash
curl http://127.0.0.1:8080/api/v1/messages                # list, newest first
curl http://127.0.0.1:8080/api/v1/messages/{id}           # headers, MIME summary, placements
curl http://127.0.0.1:8080/api/v1/messages/{id}/raw       # the exact original octets
```

Or watch mail arrive live over the WebSocket at `/api/v1/events`. Full route list, error model and
event schema: [API_DESIGN.md](docs/API_DESIGN.md) and [EVENT_MODEL.md](docs/EVENT_MODEL.md).

The API describes itself:

```bash
curl http://127.0.0.1:8080/api/openapi.json     # the generated OpenAPI 3.1 contract
```

### Building

```bash
cargo test --workspace     # 2058 tests
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

### Authentication and TLS

**Both are off by default, deliberately.** Sandesh is a local development mail server: requiring
credentials before it will accept a test message is friction with no security benefit on loopback.
Every listener binds to loopback, and Sandesh warns when you bind elsewhere.

To turn them on:

```bash
SANDESH_SMTP_AUTH=required SANDESH_SMTP_TLS=starttls cargo run
```

On first start with TLS enabled, Sandesh generates a self-signed certificate for `localhost`,
`127.0.0.1` and `::1`, stores it beside the database, and prints its fingerprint:

```text
Auth:      required · TLS: starttls
Cert:      SHA-256 CD:02:4C:31:...
```

Compare that against what your client reports — it is the only way to trust a self-signed
certificate deliberately rather than by clicking through a warning. It matches
`openssl x509 -in <data-dir>/sandesh-cert.pem -noout -fingerprint -sha256`.

**`AUTH` is offered only over an encrypted connection.** `PLAIN` and `LOGIN` both transmit the
password, so on a cleartext connection Sandesh does not advertise `AUTH` and answers `538` if a
client tries anyway. Enabling authentication without TLS therefore does nothing unless you also set
`SANDESH_SMTP_ALLOW_CLEARTEXT_AUTH=true`, and Sandesh says so at startup rather than failing
silently.

**Two TLS modes.** `starttls` keeps the port cleartext until a client issues `STARTTLS`;
`implicit` completes the handshake before any SMTP is spoken, so plaintext is never interpreted on
that port at all. Verify either yourself:

```bash
# starttls
openssl s_client -connect 127.0.0.1:1025 -starttls smtp

# implicit — note the absence of -starttls
SANDESH_SMTP_TLS=implicit cargo run
openssl s_client -connect 127.0.0.1:1025
```

On an implicit-TLS port a plaintext client receives a TLS alert, never an SMTP greeting.

Passwords are stored as Argon2id hashes. There is no recoverable secret anywhere, which is why
CRAM-MD5 is refused rather than implemented.

**These settings govern SMTP only.** The HTTP API has its own: `SANDESH_API_AUTH=required` with
`SANDESH_API_TOKEN`, and `SANDESH_API_TLS=true` for HTTPS (see the environment table above). Both are
off by default, which is why every listener binds to loopback.

### The Web UI

The console lives in `web/`. A release build embeds `web/dist` into the binary, so **the UI must be
built before Sandesh is compiled** if you want it served:

```bash
cd web
npm install
npm run build          # writes web/dist, which the binary embeds
```

Then `cargo build` (or `cargo run`) from the repository root. If no UI has been built, the API still
runs and the browser is told how to build one, rather than being served a blank page.

For frontend work, run Vite against a running Sandesh instead — it proxies `/api` and the WebSocket
to the backend, so no URL is hardcoded:

```bash
cargo run              # terminal 1: the backend on :8080
cd web && npm run dev  # terminal 2: the UI on :5173, proxying to it
```

Frontend commands:

```bash
npm run typecheck      # tsc
npm run lint           # eslint
npm test               # component and unit tests (vitest)
npm run build          # production bundle
npm run e2e            # browser tests (playwright); starts Sandesh itself
```

### The API contract

The `OpenAPI` document and the frontend's TypeScript types are **generated from the Rust API** and
committed. One command regenerates both:

```bash
cargo xtask generate-api           # writes docs/openapi.json and web/src/generated/api.ts
cargo xtask generate-api --check   # fails if either is out of date
```

```text
#[utoipa::path] on a handler ─┐
#[derive(ToSchema)] on a DTO ─┼─▶ docs/openapi.json ─▶ web/src/generated/api.ts ─▶ the UI
```

Nothing downstream is written by hand. Change a DTO without regenerating and CI fails with the
command to run — so the frontend can never be typed against an API that no longer exists. The
reasoning is in [DECISIONS.md](docs/DECISIONS.md).

The SMTP server also remains a library any host can embed:

```rust
let delivery = Arc::new(DeliveryService::new(accounts, mailboxes, messages, deliveries, policy));
let (server, warnings) = SmtpServer::new(SmtpConfig::default(), delivery).bind().await?;
server.serve(CancellationToken::new()).await?;
```

---

## Why Sandesh exists

Developers testing email today choose between two imperfect options:

- **Capture-only tools** (Mailpit and similar) give a superb inspection experience but treat mail as
  a one-way stream. They receive over SMTP and expose a read view; mailbox semantics — folders,
  flags, UIDs, multi-account delivery — are not the model.
- **Protocol test servers** (GreenMail and similar) provide real SMTP/IMAP/POP3 semantics, but are
  built as an in-process testing fixture for one language ecosystem, with a developer experience
  that stops at the API.

Sandesh takes the position that these are the same product. A developer testing a mail feature
needs *both* a faithful multi-protocol mail server *and* a first-class inspection console over the
same state — as one native binary, with no runtime to install.

Sandesh is an independent implementation. GreenMail and Mailpit are studied as prior art for
protocol scope and developer experience respectively; no code, UI, or branding is taken from either.

## Target users

- Application developers testing transactional mail (signup, reset, receipts, notifications)
- Developers building or debugging **mail clients**, who need a real IMAP/POP3 server to talk to
- QA and CI pipelines asserting on delivered mail through a REST API
- Anyone debugging MIME, encodings, headers, or malformed messages

## Core capabilities

Everything below is built and tested. See the status table above for per-area detail and
[ROADMAP.md](docs/ROADMAP.md) for phasing.

- **SMTP server** — accepts mail from your application; never relays to the outside world by default
- **IMAP server** — folders, flags, UIDs, SEARCH, FETCH, IDLE; point a real mail client at it
- **POP3 server** — simple retrieval for clients and scripts that need it
- **Shared mail core** — all three protocols operate on one canonical mail state
- **Byte-exact raw message preservation** — the original RFC 5322/MIME bytes are the source of truth
- **Message inspector** — headers, MIME tree, HTML/text bodies, inline resources, attachments, raw source
- **Developer Mail Composer & Reply** — send and reply to test emails directly from the Web UI without third-party mail clients
- **Full-text search** — across headers and body text, over an FTS5 trigram index
- **Developer diagnostics** — protocol transcripts, delivery explanations, a `Received:` hop
  timeline, Prometheus metrics, a try-it API console and a live log stream
- **REST API + WebSocket** — versioned at `/api/v1`, with live events for automation and the UI
- **Single static binary + Docker image** — no JVM, no interpreter, no external services

## Default ports

Chosen to sit above 1024 (no root required) and to avoid colliding with a real local mail stack.

| Service | Default | Rationale |
| --- | --- | --- |
| SMTP | `127.0.0.1:1025` | De facto developer convention |
| IMAP | `127.0.0.1:1143` | `143 + 1000`, mirrors the SMTP convention |
| POP3 | `127.0.0.1:1110` | `110 + 1000` |
| Web UI + REST API | `127.0.0.1:8080` | UI and API share one listener. The Docker image uses `1080` |

Binding to loopback by default is a deliberate safety decision: a mail server that captures
everything should not be reachable from the network unless the developer opts in.

## Architecture at a glance

**Mail Core first. Protocols are adapters. UI is a client.**

```text
   Your application            A real mail client
          │                            │
        SMTP                     IMAP / POP3
          └─────────────┬─────────────┘
                        ▼
              Protocol adapters
                        ▼
                   MAIL CORE          ← the canonical model; knows no protocol
                   ┌────┴────┐
                   ▼         ▼
              Storage     Event bus
              (SQLite)        ▼
                        REST + WebSocket
                              ▼
                           Web UI
```

The core crate depends on no protocol, no HTTP framework, and no database. Every boundary in that
diagram is a trait, which is what makes SMTP, IMAP, POP3 and the UI interchangeable and independently
testable. See [ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Future direction

- **Desktop installers** for macOS (`.dmg`) and Windows (`.msi`), and desktop bundles published by
  each release. The desktop app runs on Linux today but is built from source.
- **More install channels:** Homebrew, Scoop and `cargo install`.
- **Release signing** with a published key, on top of today's checksums and provenance attestations.
- **HTML compatibility linting**, flagging markup that common mail clients render differently.
- **Opt-in link checking.** Links are extracted today but never fetched, on purpose; fetching would
  only ever happen when you ask for it.
- **An optional PostgreSQL backend** for shared team instances.

## Documentation

| Document | Contents |
| --- | --- |
| [ARCHITECTURE.md](docs/ARCHITECTURE.md) | System architecture, crate boundaries, invariants |
| [CORE_SPEC.md](docs/CORE_SPEC.md) | Domain model, entities, invariants, lifecycle, error model |
| [PROTOCOL_CAPABILITIES.md](docs/PROTOCOL_CAPABILITIES.md) | Per-protocol command and extension matrix |
| [SMTP_DESIGN.md](docs/SMTP_DESIGN.md) | SMTP architecture, state machine, DATA handling, limits |
| [IMAP_DESIGN.md](docs/IMAP_DESIGN.md) | IMAP scope, session states, capabilities, auth, TLS, literals |
| [STORAGE_DESIGN.md](docs/STORAGE_DESIGN.md) | SQLite schema, indexes, migrations, blob storage |
| [API_DESIGN.md](docs/API_DESIGN.md) | REST endpoints and WebSocket protocol |
| [EVENT_MODEL.md](docs/EVENT_MODEL.md) | Event types, payloads, delivery, ordering guarantees |
| [UI_UX_DIRECTION.md](docs/UI_UX_DIRECTION.md) | Information architecture and visual principles |
| [DEPENDENCIES.md](docs/DEPENDENCIES.md) | Evaluated dependencies and the reasoning behind each |
| [TESTING_STRATEGY.md](docs/TESTING_STRATEGY.md) | Test layers, fixtures, cross-protocol tests |
| [ROADMAP.md](docs/ROADMAP.md) | Phased plan with acceptance criteria |
| [DECISIONS.md](docs/DECISIONS.md) | Architecture decision records and open decisions |

## Contributing

Contributions are welcome. [CONTRIBUTING.md](CONTRIBUTING.md) covers setup, the checks CI runs and
the DCO sign-off; [RELEASING.md](RELEASING.md) covers versioning and releases, and
[CHANGELOG.md](CHANGELOG.md) records every release. Everyone taking part follows the
[Code of Conduct](CODE_OF_CONDUCT.md). For help, see [SUPPORT.md](SUPPORT.md).

## Credits

Sandesh is developed by [Heliconia Solutions](https://www.heliconia.io) and its contributors, on the
shoulders of many open-source projects. See [CREDITS.md](CREDITS.md).

## Licence

[Apache-2.0](LICENSE), Copyright 2026 Heliconia Solutions Pvt. Ltd. See [NOTICE](NOTICE). Chosen
for its explicit patent grant; every dependency is permissively licensed. Every release ships the licence texts of its third-party components in
`THIRD_PARTY_LICENSES.txt`.

## Security

Please report vulnerabilities privately — see [SECURITY.md](SECURITY.md). Do not open a public
issue for a security report.
