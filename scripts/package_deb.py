#!/usr/bin/env python3
"""Build a Debian package for the Sandesh *server*.

`ROADMAP.md` Phase 10 lists `.deb`/`.rpm` under package distribution. This is
not the Phase 9 `.deb`: that one (ADR-094) is Tauri's bundle of the **desktop
app**, which needs GTK and WebKit and installs a `.desktop` entry for a GUI.
This one installs the headless server and a systemd unit, depends on nothing at
all, and is what belongs on a machine with no display.

## Why the package is built by hand rather than with `cargo-deb`

`cargo-deb` would read the manifest and do most of this, at the cost of a build
dependency that must itself be installed before a release can be cut, and of
describing the package's contents in a `[package.metadata.deb]` table that no
other part of the build reads. `dpkg-deb --build` takes a directory tree and a
control file; the tree is the thing being shipped, written out literally, which
is easier to audit than a metadata table.

The binary comes from `release.py`, so the packaged bytes are the same static
musl binary the smoke test ran — not a separate build.

Usage:  scripts/package_deb.py [--arch amd64] [--out dist]
"""
import argparse
import hashlib
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# Debian's architecture names, which are not Rust's.
ARCH_TARGET = {
    "amd64": "x86_64-unknown-linux-musl",
    "arm64": "aarch64-unknown-linux-musl",
}

# A system user, so the service does not run as root. `--system` gives no login
# shell and no password; the home directory is where mail is kept.
SERVICE_USER = "sandesh"
DATA_DIR = "/var/lib/sandesh"


def run(*args: str, **kwargs) -> str:
    result = subprocess.run(args, capture_output=True, text=True, check=False, **kwargs)
    if result.returncode != 0:
        raise RuntimeError(f"{args[0]} failed: {result.stderr.strip()[-2000:]}")
    return result.stdout.strip()


def version() -> str:
    manifest = (REPO / "Cargo.toml").read_text(encoding="utf-8")
    inside = False
    for line in manifest.splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            inside = stripped == "[workspace.package]"
        elif inside and stripped.startswith("version"):
            return stripped.split("=", 1)[1].strip().strip('"')
    raise RuntimeError("no version found in [workspace.package]")


def binary_for(arch: str, staging: Path) -> Path:
    """The static binary for this architecture, built if it is not already."""
    target = ARCH_TARGET[arch]
    archive = REPO / "dist" / f"sandesh-{version()}-{target}.tar.gz"

    if not archive.exists():
        print(f"  no {archive.name} yet; building it")
        run(
            sys.executable, str(REPO / "scripts" / "release.py"),
            "--target", target,
            cwd=REPO,
        )

    # Unpacked from the release archive rather than rebuilt, so the package and
    # the published tarball hold the same bytes. A package built from its own
    # compile would be a third artifact nothing had smoke-tested.
    with tarfile.open(archive) as tf:
        tf.extractall(staging, filter="data")
    return staging / "sandesh"


# The systemd unit. `DynamicUser` is deliberately *not* used: the data directory
# must persist across restarts with stable ownership, which dynamic uids make
# awkward. The hardening below is the subset that costs nothing for a process
# that only opens sockets and files under its own StateDirectory.
SERVICE = f"""\
[Unit]
Description=Sandesh — local mail server for developers
Documentation=https://github.com/HeliconiaIO/Sandesh
After=network.target

[Service]
Type=simple
User={SERVICE_USER}
Group={SERVICE_USER}
ExecStart=/usr/bin/sandesh
Restart=on-failure
RestartSec=5s

# Sandesh is configured by environment variable; this file is where an operator
# sets them. Marked `-` so the service still starts if it has been removed.
EnvironmentFile=-/etc/sandesh/sandesh.env

StateDirectory=sandesh
WorkingDirectory={DATA_DIR}

# The defaults bind loopback only (ADR-005). An operator who changes that in
# the environment file is making a deliberate choice; nothing here prevents it.
NoNewPrivileges=true
PrivateTmp=true
PrivateDevices=true
ProtectSystem=strict
ProtectHome=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
RestrictNamespaces=true
LockPersonality=true
MemoryDenyWriteExecute=true

[Install]
WantedBy=multi-user.target
"""

# The defaults, shipped as a conffile so an operator's edits survive an upgrade.
ENVIRONMENT_FILE = f"""\
# Sandesh configuration. Every setting is an environment variable; there are no
# command-line flags. `sandesh --help` lists the common ones.
#
# The defaults below bind loopback only, which is what a development mail server
# should do. Binding a public address means accepting mail from anywhere: set
# authentication (SANDESH_SMTP_AUTH, SANDESH_API_AUTH) before you do.

SANDESH_DATA_DIR={DATA_DIR}

#SANDESH_SMTP_ADDR=127.0.0.1:1025
#SANDESH_IMAP_ADDR=127.0.0.1:1143
#SANDESH_HTTP_ADDR=127.0.0.1:8080
#SANDESH_LOG=info
"""

# `set -e` so a failure in useradd aborts the install rather than leaving a
# service configured to run as a user that does not exist.
POSTINST = f"""\
#!/bin/sh
set -e

if [ "$1" = "configure" ]; then
    # `useradd`/`groupadd` from the `passwd` package, not Debian's `adduser`:
    # `adduser` is a Perl script and is absent from minimal images such as
    # `debian:stable-slim`, where this failed with exit 127. Found by installing
    # the package in that image rather than by reading the maintainer script.
    if ! getent group {SERVICE_USER} >/dev/null; then
        groupadd --system {SERVICE_USER}
    fi

    if ! getent passwd {SERVICE_USER} >/dev/null; then
        useradd --system --gid {SERVICE_USER} --no-create-home \\
                --home-dir {DATA_DIR} --shell /usr/sbin/nologin {SERVICE_USER}
    fi

    mkdir -p {DATA_DIR}
    chown {SERVICE_USER}:{SERVICE_USER} {DATA_DIR}
    chmod 0750 {DATA_DIR}

    # The environment file ships 0640 because it is where an API token goes;
    # the service therefore has to reach it by group. Ownership cannot be set
    # in the package itself, since the user does not exist until now.
    if [ -f /etc/sandesh/sandesh.env ]; then
        chown root:{SERVICE_USER} /etc/sandesh/sandesh.env
    fi

    # Make the unit visible to systemd. Not enabled and not started: a mail
    # server that begins listening the moment it is installed is a surprise,
    # and `deb-systemd-invoke` is unavailable without debhelper anyway.
    if [ -d /run/systemd/system ]; then
        systemctl daemon-reload >/dev/null 2>&1 || true
    fi
fi

exit 0
"""

# Mail is user data: removing the package must not delete it. Only `purge`
# does, which is the contract a Debian user expects.
POSTRM = f"""\
#!/bin/sh
set -e

if [ "$1" = "purge" ]; then
    rm -rf {DATA_DIR}
    # `userdel`, for the same reason `useradd` is used above. Failure is
    # tolerated: a leftover system account is untidy, not harmful, and must
    # never leave the package in a half-purged state.
    if getent passwd {SERVICE_USER} >/dev/null; then
        userdel {SERVICE_USER} >/dev/null 2>&1 || true
    fi
fi

exit 0
"""


def build(arch: str, out: Path) -> Path:
    package_version = version()
    name = f"sandesh_{package_version}_{arch}"

    with tempfile.TemporaryDirectory() as tmp:
        staging = Path(tmp)
        root = staging / "root"
        binary = binary_for(arch, staging)

        # The installed tree, written out literally.
        (root / "usr/bin").mkdir(parents=True)
        shutil.copy2(binary, root / "usr/bin/sandesh")
        (root / "usr/bin/sandesh").chmod(0o755)

        (root / "lib/systemd/system").mkdir(parents=True)
        (root / "lib/systemd/system/sandesh.service").write_text(SERVICE, encoding="utf-8")

        (root / "etc/sandesh").mkdir(parents=True)
        environment_path = root / "etc/sandesh/sandesh.env"
        environment_path.write_text(ENVIRONMENT_FILE, encoding="utf-8")
        # 0640, not the umask's 0664: this file is where SANDESH_API_TOKEN goes,
        # and a shared bearer token readable by every user on the host is a
        # credential leak the package would have shipped by default.
        environment_path.chmod(0o640)

        # Licence texts, unpacked from the release archive alongside the binary.
        # Debian policy §12.5 names the file `copyright`; NOTICE and the
        # third-party texts sit beside it.
        doc = root / "usr/share/doc/sandesh"
        doc.mkdir(parents=True)
        shutil.copy2(binary.parent / "LICENSE", doc / "copyright")
        for legal in ("NOTICE", "THIRD_PARTY_LICENSES.txt"):
            if (binary.parent / legal).exists():
                shutil.copy2(binary.parent / legal, doc / legal)

        installed_size = sum(
            path.stat().st_size for path in root.rglob("*") if path.is_file()
        )

        control = root / "DEBIAN"
        control.mkdir()
        # No `Depends`: the binary is statically linked, which is the whole
        # point of the musl build. Getting this wrong in the other direction —
        # declaring a dependency that is not needed — is what ADR-094 found in
        # the Tauri bundle, where the emitted GTK dependency was named for a
        # pre-24.04 Debian and made the package uninstallable.
        (control / "control").write_text(
            f"""\
Package: sandesh
Version: {package_version}
Section: mail
Priority: optional
Architecture: {arch}
Maintainer: Heliconia Solutions <hello@heliconia.io>
Installed-Size: {max(1, installed_size // 1024)}
Description: Local mail server for developers
 Sandesh captures the mail your application sends and lets you inspect it over
 SMTP, IMAP, POP3, a REST API and a web UI, from one static binary with no
 runtime to install.
 .
 This package installs the headless server and a systemd unit. It is not the
 desktop application.
""",
            encoding="utf-8",
        )

        # `sandesh.env` is a conffile: dpkg then prompts rather than silently
        # overwriting an operator's configuration on upgrade.
        (control / "conffiles").write_text("/etc/sandesh/sandesh.env\n", encoding="utf-8")

        for script, body in (("postinst", POSTINST), ("postrm", POSTRM)):
            path = control / script
            path.write_text(body, encoding="utf-8")
            path.chmod(0o755)

        # Normalise directory modes. `--root-owner-group` fixes *ownership* but
        # leaves the mode the build user's umask produced, which was 0775 here:
        # group-writable directories in a package are a genuine lintian error,
        # not cosmetics, since any member of the group could replace the binary.
        root.chmod(0o755)
        for path in root.rglob("*"):
            if path.name == "DEBIAN" or "DEBIAN" in path.parts:
                continue  # dpkg owns these; the maintainer scripts stay 0755.
            # Directories 0755, plain files 0644 — except the two set
            # deliberately above (the binary 0755, the environment file 0640).
            if path.is_dir():
                path.chmod(0o755)
            elif path.stat().st_mode & 0o777 not in (0o755, 0o640):
                path.chmod(0o644)

        out.mkdir(parents=True, exist_ok=True)
        package = out / f"{name}.deb"

        # `fakeroot` so the files inside are owned by root without this script
        # needing to be. Without it every path would carry the building user's
        # uid and the package would install with wrong ownership.
        build_command = ["dpkg-deb", "--root-owner-group", "--build", str(root), str(package)]
        if shutil.which("fakeroot") and os.geteuid() != 0:
            build_command = ["fakeroot", *build_command]
        run(*build_command)

    return package


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arch", default="amd64", choices=sorted(ARCH_TARGET))
    parser.add_argument("--out", default="dist")
    args = parser.parse_args()

    if not shutil.which("dpkg-deb"):
        print("dpkg-deb is required to build a .deb", file=sys.stderr)
        return 2

    out = Path(args.out) if os.path.isabs(args.out) else REPO / args.out
    print(f"packaging sandesh {version()} for {args.arch}")

    package = build(args.arch, out)
    digest = hashlib.sha256(package.read_bytes()).hexdigest()
    package.with_suffix(".deb.sha256").write_text(
        f"{digest}  {package.name}\n", encoding="utf-8"
    )

    print(f"  {package.name}  ({package.stat().st_size / 1_000_000:.1f} MB)")
    print(f"  {digest}")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (RuntimeError, OSError) as error:
        print(f"packaging FAILED: {error}", file=sys.stderr)
        sys.exit(1)
