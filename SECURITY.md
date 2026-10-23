# Security Policy

## Reporting a vulnerability

**Please do not open a public issue for a security report.**

Report privately through either channel:

- **GitHub Security Advisories** — [open a draft advisory](https://github.com/HeliconiaIO/Sandesh/security/advisories/new)
  on this repository. Preferred: it keeps the report, the fix and the eventual disclosure together.
- **Email** — <hello@heliconia.io>, with `Sandesh` in the subject.

A useful report says which version (`sandesh --version`) and platform you saw it on, how the
instance was configured — above all whether any listener was bound off loopback — and the smallest
sequence of steps that reproduces it. A message that triggers the bug is worth more than a
description of it; `.eml` attachments are welcome.

You will get an acknowledgement within **3 working days** and an assessment within **10**. We will
tell you whether the report is accepted, what the fix looks like and when we expect to ship it, and
we will credit you in the advisory unless you would rather we did not.

Please give us a reasonable window to ship a fix before disclosing publicly. We are a small team and
would rather agree a date with you than race you to one.

## Supported versions

Sandesh is pre-1.0 and ships from `main`. Fixes land in the next release; there are no maintained
backport branches yet. This section will become a table when there is more than one supported line.

| Version | Supported |
| --- | --- |
| 0.1.x | Yes |
| < 0.1 | No |

## What Sandesh is, and what that means for severity

Sandesh is a **local-first mail development environment** — a tool for capturing and inspecting the
mail your application sends while you build it. It is not a production mail transfer agent, it does
not relay mail onward, and it is not intended to sit on the public internet.

That shapes how we triage, so it is worth stating plainly rather than leaving you to guess:

- **Every listener binds to loopback by default** (SMTP, IMAP, POP3 and HTTP). Sandesh warns at
  startup when you bind elsewhere, and `/info` reports the posture.
- **Authentication is off by default.** On loopback, demanding a credential before `curl`
  can read a test message is friction with no security benefit. Bound off loopback without
  authentication, Sandesh is an open mail sink — it says so at startup.
- **Mail is untrusted input, and is treated as such.** Parsing is the primary attack surface: MIME,
  and the SMTP, IMAP and POP3 command grammars are continuously fuzzed, and `unsafe` code is
  forbidden workspace-wide.
- **Sandesh never fetches the links it extracts.** Resolving URLs found in received mail
  would be an SSRF vector and a delivery receipt for whoever sent the message.
- **DKIM, SPF and DMARC headers are parsed and displayed, never verified.** The UI presents what a
  message claims, not a judgement about it. Reading a displayed header as a verification result is a
  misreading we would like to know about if the UI invites it.

### In scope

We want to hear about, in roughly descending order of how much we care:

- Memory-safety faults, panics or unbounded resource use reachable from parsing a message or from
  any protocol command — including a denial of service from a malformed or hostile message.
- Any bypass of authentication or of the TLS layer when either is enabled.
- Escapes from the intended data boundary: reading or writing files outside the data directory,
  SQL injection, or reaching one account's mail from another.
- Stored or reflected XSS in the developer console — particularly anything that escapes the
  message-rendering sandbox or the Content-Security-Policy into the console's own origin.
- SSRF, including any path that causes Sandesh to fetch a URL taken from a message.
- Secrets — passwords, API tokens, TLS keys — appearing in logs, traces, transcripts or API
  responses.
- Anything that makes Sandesh relay mail onward to a third party.

### Out of scope

Not because they are uninteresting, but because they are the documented design:

- **Reaching an instance the operator deliberately exposed**, where the exposure itself is the
  finding — binding off loopback with authentication disabled is a supported configuration that
  Sandesh warns about at startup, in the README and via `/info`.
- **The absence of authentication by default** (see above).
- **Missing spam, virus or reputation filtering.** Sandesh captures mail; it does not judge it.
- **Missing DKIM/SPF/DMARC verification** — parsed and displayed is the specified behaviour.
- The self-signed certificate generated for local TLS being untrusted by clients. That is what
  self-signed means; it exists so a client can exercise `STARTTLS` locally.
- Findings that require an attacker who already has local code execution or filesystem access as the
  user running Sandesh.
- Vulnerabilities in build or test tooling that is not part of a shipped artifact. We still fix
  these, but they are not a vulnerability in Sandesh.

## Hardening an exposed instance

If you must bind off loopback — a shared team instance, a CI runner reachable by other jobs —
enable both of these together:

- **Authentication.** Set `SANDESH_API_TOKEN` for the HTTP API, and configure account credentials
  for SMTP, IMAP and POP3. They are verified with Argon2id.
- **TLS.** Terminate with a real certificate, or put Sandesh behind a reverse proxy that does.

Then treat the instance as holding real data, because captured test mail routinely contains
password-reset links, invitation tokens and customer addresses.

## How we build

Context for anyone assessing a report, all of it enforced in CI rather than aspirational:

- `unsafe` is **forbidden** workspace-wide, and `unwrap`, `expect`, `panic` and `todo` are denied
  outside test code.
- Parsers are fuzzed nightly (MIME, SMTP, IMAP, POP3, search), with every crash reproducer committed
  and replayed on every commit.
- The container image is `FROM scratch` with one static binary: no shell and no package manager, so
  a compromised parser has nothing to escalate with.
- Release artifacts ship with SHA-256 checksums, SBOMs and GitHub build-provenance attestations
  (`gh attestation verify`). Reproducibility of the archives was verified when they were introduced
  but is not re-checked on every release.
- Dependencies are checked against the RustSec advisory database and a licence allowlist on every
  pull request and nightly.
