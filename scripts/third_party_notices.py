#!/usr/bin/env python3
"""Collect the licence texts every shipped Sandesh artifact owes its dependencies.

A release binary embeds two dependency trees: the Rust crates it links and the
npm packages compiled into the web UI (`rust-embed` bakes `web/dist` in). Almost
all of them are MIT, BSD or Apache licensed, and every one of those licences
makes the same demand of a binary redistribution: carry the copyright notice and
licence text with it. The bundled fonts (SIL OFL-1.1) require the same.

This writes `THIRD_PARTY_LICENSES.txt`, which `release.py`, `package_deb.py`
and `package_rpm.py` ship next to the binary. With `--check` it writes nothing
and instead fails if a shipped npm package carries a licence outside the
allowlist — the npm counterpart of the `[licenses]` check `cargo-deny` already
runs for crates. The allowlist is read from `deny.toml`, so there is one list,
plus the font licence that only the web side needs.

Rust texts come from `cargo about` (configured by `about.toml`); npm texts are
read from `web/node_modules`, so run `npm ci` in `web/` first.

Usage:  scripts/third_party_notices.py [--out THIRD_PARTY_LICENSES.txt] [--check]
"""
import argparse
import json
import subprocess
import sys
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
WEB = REPO / "web"

# Licences allowed only on the web side: the fonts are OFL, and nothing in the
# Rust tree should ever need it.
WEB_ONLY = {"OFL-1.1"}

LICENCE_FILES = ("LICENSE", "LICENSE.md", "LICENSE.txt", "LICENCE", "LICENCE.md",
                 "license", "license.md", "COPYING", "OFL.txt")


# What the binary or image contains that neither cargo-about nor the npm
# lockfile can see: the C library the Linux builds link statically, the Rust
# standard library, SQLite (compiled in by libsqlite3-sys) and the CA bundle the
# container image copies from its Alpine build stage.
SYSTEM_NOTICES = """\
================================================================================
musl libc (MIT) -- statically linked into the Linux binaries
--------------------------------------------------------------------------------
Copyright © 2005-2020 Rich Felker, et al.

Permission is hereby granted, free of charge, to any person obtaining
a copy of this software and associated documentation files (the
"Software"), to deal in the Software without restriction, including
without limitation the rights to use, copy, modify, merge, publish,
distribute, sublicense, and/or sell copies of the Software, and to
permit persons to whom the Software is furnished to do so, subject to
the following conditions:

The above copyright notice and this permission notice shall be
included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT,
TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

Full copyright file: https://git.musl-libc.org/cgit/musl/tree/COPYRIGHT

================================================================================
Rust standard library (MIT OR Apache-2.0) -- compiled into every binary
--------------------------------------------------------------------------------
Copyright The Rust Project Developers.
Licence texts: https://github.com/rust-lang/rust/blob/master/LICENSE-MIT
and https://github.com/rust-lang/rust/blob/master/LICENSE-APACHE
(the Apache-2.0 text is identical to Sandesh's LICENSE).

================================================================================
SQLite (public domain) -- compiled into every binary
--------------------------------------------------------------------------------
The SQLite source code is in the public domain: https://sqlite.org/copyright.html

================================================================================
Mozilla CA certificate bundle (MPL-2.0) -- container image only, /etc/ssl/cert.pem
--------------------------------------------------------------------------------
Copied unmodified from Alpine Linux's ca-certificates package. Source:
https://hg.mozilla.org/projects/nss/file/tip/lib/ckfw/builtins/certdata.txt
Licence: https://www.mozilla.org/MPL/2.0/
"""


def allowlist() -> set[str]:
    with open(REPO / "deny.toml", "rb") as f:
        return set(tomllib.load(f)["licenses"]["allow"]) | WEB_ONLY


def shipped_npm_packages() -> list[dict]:
    """Every package in the lockfile that reaches the browser bundle."""
    lock = json.loads((WEB / "package-lock.json").read_text(encoding="utf-8"))
    found = []
    for path, entry in sorted(lock["packages"].items()):
        # `""` is the app itself; `dev`/`devOptional` never reach `dist/`.
        if not path or entry.get("dev") or entry.get("devOptional"):
            continue
        directory = WEB / path
        manifest_path = directory / "package.json"
        if not manifest_path.exists():
            # An optional package for another platform: not installed, not shipped.
            continue
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        licence = manifest.get("license") or entry.get("license") or "UNKNOWN"
        if isinstance(licence, dict):
            licence = licence.get("type", "UNKNOWN")
        text = next(
            (p.read_text(encoding="utf-8", errors="replace")
             for p in (directory / n for n in LICENCE_FILES) if p.is_file()),
            None,
        )
        found.append({
            "name": manifest.get("name", path.removeprefix("node_modules/")),
            "version": manifest.get("version", "?"),
            "licence": licence,
            "text": text,
        })
    return found


def acceptable(expression: str, allowed: set[str]) -> bool:
    """True when an SPDX expression is satisfiable from the allowlist.

    Handles the forms npm packages actually use: a single id, `(A OR B)` and
    `A AND B`. Anything more exotic is refused rather than guessed at.
    """
    expression = expression.strip().strip("()")
    if " OR " in expression:
        return any(acceptable(part, allowed) for part in expression.split(" OR "))
    if " AND " in expression:
        return all(acceptable(part, allowed) for part in expression.split(" AND "))
    return expression in allowed


def rust_notices() -> str:
    return subprocess.run(
        ["cargo", "about", "generate", "--locked", "about.hbs"],
        cwd=REPO, check=True, capture_output=True, text=True,
    ).stdout


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", default="THIRD_PARTY_LICENSES.txt")
    parser.add_argument("--check", action="store_true",
                        help="only verify npm licences against the allowlist")
    args = parser.parse_args()

    if not (WEB / "node_modules").is_dir():
        print("web/node_modules is missing; run `npm ci` in web/ first", file=sys.stderr)
        return 2

    allowed = allowlist()
    packages = shipped_npm_packages()
    refused = [p for p in packages if not acceptable(p["licence"], allowed)]
    for p in refused:
        print(f"refused: {p['name']}@{p['version']} is {p['licence']}", file=sys.stderr)
    if refused:
        return 1
    if args.check:
        print(f"{len(packages)} shipped npm packages, all licences allowed")
        return 0

    parts = [
        "Third-party software in Sandesh\n"
        "===============================\n\n"
        "Sandesh is licensed under Apache-2.0 (see LICENSE and NOTICE). The\n"
        "binary includes the components below, each under its own licence.\n\n"
        "Part 1: Rust crates\n",
        rust_notices(),
        "\nPart 2: Web UI packages (npm)\n",
    ]
    for p in packages:
        parts.append(
            "=" * 80 + f"\n{p['name']} {p['version']} ({p['licence']})\n" + "-" * 80 + "\n"
            + (p["text"] or f"No licence file shipped; declared licence: {p['licence']}.\n")
            + "\n"
        )
    parts.append("\nPart 3: Components outside the crate and npm graphs\n" + SYSTEM_NOTICES)
    out = REPO / args.out
    out.write_text("".join(parts), encoding="utf-8")
    print(f"wrote {out}: {len(packages)} npm packages plus Rust crates")
    return 0


if __name__ == "__main__":
    sys.exit(main())
