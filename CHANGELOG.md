# Changelog

All notable changes to Sandesh are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). See [RELEASING.md](RELEASING.md) for
how versions are chosen and published.

## [Unreleased]

## [0.1.0] - 2026-10-23

The first public release.

### Added

- **SMTP server** that captures the mail your application sends. It never relays mail onward.
  Supports STARTTLS, AUTH, `BDAT`/CHUNKING and configurable fault injection.
- **IMAP4rev2 server** (RFC 9051) with folders, flags, UIDs, SEARCH, FETCH and IDLE. Tested
  against real clients, including a full `mbsync` sync.
- **POP3 server** (RFC 1939, 2449, 2595, 5034).
- **One shared mail store** for all three protocols, on SQLite. The original message bytes are
  kept exactly as received.
- **REST API and WebSocket events** under `/api/v1`, described by an OpenAPI contract.
- **Web console** with a message inspector (headers, MIME tree, HTML and text bodies,
  attachments, raw source), full-text search, a composer and reply, multiple workspaces, and an
  API console that can call every documented endpoint.
- **Diagnostics:** protocol transcripts, a delivery record for every transaction, a `Received:`
  hop timeline, DKIM/SPF/DMARC header parsing (displayed, not verified), Prometheus `/metrics`
  and a live log stream.
- **Mail-testing tools:** fault injection, signed webhooks (HMAC-SHA256), message rules, replay
  with bulk import and export, and link and image extraction.
- **Optional authentication and TLS:** an API token, account credentials hashed with Argon2id,
  and STARTTLS or implicit TLS.
- **Distribution:** static binaries for Linux (x86_64, arm64), macOS (Intel, Apple Silicon) and
  Windows; a `FROM scratch` container image; `.deb` and `.rpm` packages; a Tauri desktop app for
  Linux.
- **Open-source project files:** Apache-2.0 `LICENSE` and `NOTICE`, third-party licence texts in
  every artifact, `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `SECURITY.md` and `CREDITS.md`.

### Security

- Every listener binds to loopback by default.
- `unsafe` code is forbidden. The MIME, SMTP, IMAP, POP3 and search parsers are fuzzed nightly.
- A malformed `multipart/report` message no longer crashes the server. Fuzzing found a panic in
  the `mail-parser` dependency; Sandesh now stores such a message as unparseable instead.
- Dependencies are checked against RustSec and npm advisories and a licence allowlist in CI.

[Unreleased]: https://github.com/HeliconiaIO/Sandesh/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/HeliconiaIO/Sandesh/releases/tag/v0.1.0
