# Changelog

All notable changes to Sandesh are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). See [RELEASING.md](RELEASING.md) for
how versions are chosen and published.

## [Unreleased]

## [0.2.0] - 2026-10-30

### Added

- Releases now include a signed SLSA provenance file (`sandesh-<version>.intoto.jsonl`)
  next to the binaries, verifiable without GitHub's attestation API.
- `SANDESH_MAX_MESSAGE_SIZE` sets the largest message SMTP accepts and IMAP `APPEND`
  takes. It was fixed at 25 MiB with no way to raise it.

### Changed

- **The message list shows the `From:` header as the sender**, display name included.
  It showed the SMTP envelope sender, usually a bounce address with no name. The
  envelope is still available from `GET /messages/{id}/delivery`.
- A non-ASCII address in `MAIL FROM` or `RCPT TO` is refused with `553 5.6.7`,
  saying SMTPUTF8 is not supported, instead of `500 Unrecognised command`.
- **Minimum supported Rust is now 1.98** (was 1.94), required by `mail-parser` 1.0.

### Fixed

- POP3 `CAPA` announced `IMPLEMENTATION Sandesh 0.1.0` whatever the version, and the
  desktop app's version was set separately from the rest. Both now come from `Cargo.toml`.
- Some malformed nested multipart messages (for example a `multipart/digest` or
  `multipart/report` with broken inner parts) were stored as unparsed, with no MIME
  tree. Upgrading `mail-parser` from 0.11 to 1.0 lets them parse.
- **The search index no longer indexes attachments' binary content.** PDFs, images
  and archives were indexed as text, making the index about four times the size of
  the mail it covered (4.1 GB of a 5.2 GB database at 100,000 messages; 432 MB
  after this fix). On first start after upgrading, the index is rebuilt in the
  background (about 100 s for 100,000 messages); search is correct but slower until
  it finishes. The database file does not shrink, but the freed space is reused.
- **The message list reads one page, not the whole mailbox.** Each page loaded and
  sorted every message, taking 2.6 s and about 170 MB at 100,000 messages; it now
  takes about 100 ms.
- Paging through `GET /search` returned the first page again as the second.
- A display name containing a comma, such as `"Doe, Jane" <jane@example.test>`, was
  split into two recipients, one of them a bogus `Doe`.
- A received HTML email containing `</` followed by a non-ASCII character made
  `GET /messages/{id}/links` fail.

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

[Unreleased]: https://github.com/HeliconiaIO/Sandesh/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/HeliconiaIO/Sandesh/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/HeliconiaIO/Sandesh/releases/tag/v0.1.0
