## What and why

<!-- What does this change, and what problem does it solve? Link the issue: "Fixes #123". -->

## How it was tested

<!-- Commands run, tests added, clients tried. -->

## Checklist

- [ ] Every commit is signed off (`git commit -s`), see [CONTRIBUTING.md](../CONTRIBUTING.md)
- [ ] `cargo fmt`, `cargo clippy -D warnings` and `cargo test --workspace` pass
- [ ] For UI changes: `npm run lint`, `npm run build` and `npm test` pass in `web/`
- [ ] For API changes: `cargo xtask generate-api` was run and its output committed
- [ ] User-visible change: entry added under `## [Unreleased]` in `CHANGELOG.md`
- [ ] Design change: ADR added or updated in `docs/DECISIONS.md`
