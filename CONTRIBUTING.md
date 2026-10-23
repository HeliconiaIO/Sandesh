# Contributing to Sandesh

Thanks for helping. This guide covers how to set up, what CI checks, and how a change gets merged.

By taking part you agree to the [Code of Conduct](CODE_OF_CONDUCT.md). **Security issues do not go
in public issues.** Follow [SECURITY.md](SECURITY.md) instead.

## Before you start

- **Bugs:** open an issue using the bug report form. A message that reproduces the problem (an
  `.eml` file or the SMTP commands) is the most useful thing you can attach.
- **Features:** open an issue or a discussion first. Sandesh records its design choices as
  architecture decision records (ADRs) in [docs/DECISIONS.md](docs/DECISIONS.md). A change that
  goes against one needs a discussion before it needs code.
- **Small fixes** (typos, docs, obvious bugs): a pull request on its own is fine.

## Setup

You need rustup, which installs the pinned toolchain from `rust-toolchain.toml` (currently 1.99.1)
automatically; the minimum supported Rust is 1.94 (`rust-version` in `Cargo.toml`). Also Node.js 22 and
npm.

```bash
git clone https://github.com/HeliconiaIO/Sandesh.git
cd Sandesh
(cd web && npm ci && npm run build)   # the UI is embedded into the binary
cargo run
```

## Checks to run before opening a pull request

CI runs all of these. Running them first saves a round trip.

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo xtask generate-api --check      # the OpenAPI contract and generated TS are current

cd web
npm run lint
npm run build                         # the real type gate; `tsc --noEmit` is not enough
npm test
npm run e2e                           # Playwright, for UI changes
```

When you change an API handler or DTO, run `cargo xtask generate-api` and commit the regenerated
`docs/openapi.json` and `web/src/generated/api.ts` together with your change.

## Code rules CI enforces

- `unsafe` is forbidden throughout the workspace.
- `unwrap`, `expect`, `panic!` and `todo!` are denied outside test code.
- New dependencies must pass `cargo deny check` (Rust) and
  `scripts/third_party_notices.py --check` (npm). Both allow only permissive licences.
- Parsers of untrusted input (MIME, SMTP, IMAP, POP3, search) need tests. A fix for a crash
  should add its input as a reproducer.

## Commits and sign-off

Sandesh uses the [Developer Certificate of Origin](https://developercertificate.org/) (DCO).
Signing off certifies that you wrote the change, or otherwise have the right to submit it under
the project's licence. Sign every commit:

```bash
git commit -s -m "imap: handle empty SEARCH result"
```

This adds a `Signed-off-by: Your Name <you@example.com>` line, and the DCO check rejects commits
without one. To fix the last commit, run `git commit --amend -s`.

Commit messages use the [Conventional Commits](https://www.conventionalcommits.org/) style
(`feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `ci:`, `chore:`, with an optional scope such as
`fix(smtp):`). Release notes are drafted from these prefixes. See [RELEASING.md](RELEASING.md).

## Pull requests

- Keep one logical change per pull request.
- Describe what changed and why, and how you tested it. The template asks for this.
- For a user-visible change, add an entry under `## [Unreleased]` in [CHANGELOG.md](CHANGELOG.md).
- For a design change, add or update an ADR in `docs/DECISIONS.md`.
- A maintainer listed in [.github/CODEOWNERS](.github/CODEOWNERS) reviews it. All CI checks must
  pass before merge.

## Licence

Sandesh is licensed under [Apache-2.0](LICENSE). Contributions are accepted under the same licence
(Apache-2.0 §5), and the DCO sign-off records your agreement to that.
