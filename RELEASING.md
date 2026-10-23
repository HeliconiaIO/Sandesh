# Versioning and Releases

This document describes how Sandesh is versioned, how changes reach `main`, and how a release is
published.

## Version numbers

Sandesh follows [Semantic Versioning 2.0](https://semver.org/). The version is set in one place,
`[workspace.package] version` in the root `Cargo.toml`. Every crate, archive name, package and
`sandesh --version` reads it from there.

Sandesh is pre-1.0, and while it is, the rules are:

| Change | Bump | Example |
| --- | --- | --- |
| Breaking change to the REST API, config, CLI or storage format | minor | 0.1.4 → 0.2.0 |
| New feature, backwards compatible | minor | 0.1.4 → 0.2.0 |
| Bug or security fix only | patch | 0.1.4 → 0.1.5 |

From 1.0 on, breaking changes bump the major version. "Breaking" includes the REST API under
`/api/v1`, the configuration keys and environment variables, CLI flags, and the on-disk database
(migrations must upgrade an existing database without losing data).

Pre-releases use a suffix: `0.2.0-rc.1`. The release workflow marks these as pre-releases on
GitHub.

## Branches and tags

- `main` is always releasable. All work lands through pull requests with passing CI.
- Each release is an annotated tag `vX.Y.Z` on a commit on `main`.
- There are no long-lived release branches before 1.0. If a fix has to go to an older line later,
  branch `release/X.Y` from its last tag.

## Changelog and release notes

- Every user-visible change adds a line under `## [Unreleased]` in [CHANGELOG.md](CHANGELOG.md),
  in the pull request that makes the change. Use the Keep a Changelog groups: Added, Changed,
  Deprecated, Removed, Fixed, Security.
- At release time the `[Unreleased]` entries move under the new version heading. **The release
  notes on GitHub are that section of CHANGELOG.md**, extracted by the release workflow. GitHub's
  automatically generated list of merged pull requests and contributors is appended below it.

## Cutting a release

1. Make sure `main` is green and up to date.
2. Pick the version from the table above.
3. In a pull request titled `release: vX.Y.Z`:
   - set `version` in the root `Cargo.toml` and run `cargo check` so `Cargo.lock` updates;
   - in `CHANGELOG.md`, rename `## [Unreleased]` to `## [X.Y.Z] - YYYY-MM-DD`, add a new empty
     `## [Unreleased]` above it, and update the comparison links at the bottom.
4. Merge it, then tag the merge commit and push the tag:

   ```bash
   git switch main && git pull
   git tag -a vX.Y.Z -m "Sandesh vX.Y.Z"
   git push origin vX.Y.Z
   ```

5. The tag starts the **Release** workflow (`.github/workflows/release.yml`). It:
   - checks that the tag matches the `Cargo.toml` version and that CHANGELOG.md has a section for
     it, and stops if either check fails;
   - generates `THIRD_PARTY_LICENSES.txt`;
   - builds the binaries for all five targets, the `.deb` and `.rpm`, and CycloneDX SBOMs;
   - builds the multi-arch container image and pushes it to `ghcr.io/heliconiaio/sandesh`;
   - writes `SHA256SUMS` and a build-provenance attestation for every artifact;
   - creates the GitHub Release with the changelog section as its notes.
6. Check the release page, and download one artifact to verify it:

   ```bash
   sha256sum -c SHA256SUMS --ignore-missing
   gh attestation verify sandesh-X.Y.Z-x86_64-unknown-linux-musl.tar.gz -R HeliconiaIO/Sandesh
   ```

## If a release goes wrong

Never move or reuse a published tag. Fix the problem on `main` and release the next patch
version. If an artifact is broken or unsafe, mark the GitHub Release as a pre-release or delete
its assets, and say why in the release notes of the next version.
