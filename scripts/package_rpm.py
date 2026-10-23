#!/usr/bin/env python3
"""Build an RPM package for the Sandesh *server*.

The counterpart to `package_deb.py`, and the same shape: the headless server, a
systemd unit, no dependencies (the binary is static musl), and mail that
survives `dnf remove`.

## Why this builds inside a container

`rpmbuild` is a Fedora/RHEL tool and is not installed on every developer's
machine — it is not on this one. ADR-097 declined to ship a `.rpm` on exactly
that basis: a spec file written without ever running it is an untested artifact,
and this phase keeps refusing those.

Running `rpmbuild` inside `fedora:41` removes the reason for the refusal rather
than working around it. The spec is executed, the package is built by the real
tool, and `rpm_smoke.py` then installs it on a clean Fedora. The only thing
needed on the host is Docker, which the container image and `.deb` smoke tests
already require.

Usage:  scripts/package_rpm.py [--arch x86_64] [--out dist]
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

# RPM's architecture names, which are neither Debian's nor Rust's.
ARCH_TARGET = {
    "x86_64": "x86_64-unknown-linux-musl",
    "aarch64": "aarch64-unknown-linux-musl",
}

BUILDER_IMAGE = "fedora:41"

SERVICE_USER = "sandesh"
DATA_DIR = "/var/lib/sandesh"


def run(*args: str, **kwargs) -> str:
    result = subprocess.run(args, capture_output=True, text=True, check=False, **kwargs)
    if result.returncode != 0:
        raise RuntimeError(f"{args[0]} failed: {result.stderr.strip()[-3000:]}")
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
    """The static binary, unpacked from the release archive `release.py` built."""
    target = ARCH_TARGET[arch]
    archive = REPO / "dist" / f"sandesh-{version()}-{target}.tar.gz"

    if not archive.exists():
        print(f"  no {archive.name} yet; building it")
        run(sys.executable, str(REPO / "scripts" / "release.py"), "--target", target, cwd=REPO)

    # Unpacked rather than recompiled, so the `.rpm`, the `.deb` and the
    # published tarball all carry the same bytes.
    with tarfile.open(archive) as tf:
        tf.extractall(staging, filter="data")
    return staging / "sandesh"


# Shared with the Debian package by intent, not by accident: an operator moving
# between distributions should find the same unit and the same paths. Kept as a
# literal in both scripts rather than imported, so neither can break the other.
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

EnvironmentFile=-/etc/sandesh/sandesh.env

StateDirectory=sandesh
WorkingDirectory={DATA_DIR}

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


def spec(package_version: str, arch: str) -> str:
    """The RPM spec.

    `%global debug_package %{nil}` because the binary is already stripped and
    statically linked: without it `rpmbuild` fails outright trying to extract
    debug symbols that are not there.

    `BuildArch` is set explicitly and no `%build` stage exists — the binary is
    an input, not something compiled here, which is what keeps the packaged
    bytes identical to the smoke-tested release archive.
    """
    return f"""\
%global debug_package %{{nil}}
%global __strip /bin/true

# Suppress rpm's automatic dependency generation. Its ELF scanner sees an
# executable and emits `rtld(GNU_HASH)`, which resolves to glibc — a dependency
# a static-pie musl binary genuinely does not have, and which would drag glibc
# onto a system that has none. Verified: `rpm -qpR` listed it before this, and
# does not after.
%global __requires_exclude_from ^%{{_bindir}}/sandesh$

Name:           sandesh
Version:        {package_version}
Release:        1%{{?dist}}
Summary:        Local mail server for developers

License:        Apache-2.0
URL:            https://github.com/HeliconiaIO/Sandesh
BuildArch:      {arch}

# The binary is statically linked against musl, so it needs no runtime library.
# Declaring a dependency it does not have is the error ADR-094 found in the
# Tauri bundle; here the risk is rpm inventing one, which `__requires_exclude_from`
# above prevents.
AutoReqProv:    no

# `shadow-utils` really is needed: %pre calls useradd/groupadd.
Requires(pre):  shadow-utils

# systemd is *recommended*, not required. The unit is useless without it, but a
# hard requirement makes the package uninstallable in a container — which is a
# normal way to run a development mail server, and was observed: `rpm -i` failed
# with "systemd is needed by sandesh". The binary itself runs fine there.
Recommends:     systemd
BuildRequires:  systemd-rpm-macros

%description
Sandesh captures the mail your application sends and lets you inspect it over
SMTP, IMAP, POP3, a REST API and a web UI, from one static binary with no
runtime to install.

This package installs the headless server and a systemd unit. It is not the
desktop application.

%install
install -D -m 0755 %{{_sourcedir}}/sandesh        %{{buildroot}}%{{_bindir}}/sandesh
install -D -m 0644 %{{_sourcedir}}/sandesh.service %{{buildroot}}%{{_unitdir}}/sandesh.service
install -D -m 0640 %{{_sourcedir}}/sandesh.env     %{{buildroot}}%{{_sysconfdir}}/sandesh/sandesh.env
install -d -m 0750 %{{buildroot}}{DATA_DIR}
for f in %{{_sourcedir}}/legal/*; do
    install -D -m 0644 "$f" %{{buildroot}}%{{_licensedir}}/sandesh/"$(basename "$f")"
done

%pre
# The service account must exist before the files land. `getent` first so an
# upgrade does not fail on an account that is already there.
getent group {SERVICE_USER} >/dev/null || groupadd --system {SERVICE_USER}
getent passwd {SERVICE_USER} >/dev/null || \
    useradd --system --gid {SERVICE_USER} --no-create-home \
            --home-dir {DATA_DIR} --shell /sbin/nologin {SERVICE_USER}
exit 0

%post
# Guarded on `systemctl` existing, because systemd is only Recommends: the
# macros expand to systemctl calls that would fail the transaction in a
# container. Not enabled or started — see the Debian package for why.
if [ -x /usr/bin/systemctl ]; then
%systemd_post sandesh.service
fi

%preun
if [ -x /usr/bin/systemctl ]; then
%systemd_preun sandesh.service
fi

%postun
if [ -x /usr/bin/systemctl ]; then
%systemd_postun_with_restart sandesh.service
fi

%files
%{{_bindir}}/sandesh
%{{_licensedir}}/sandesh/
%{{_unitdir}}/sandesh.service
# %config(noreplace): an operator's edits survive an upgrade, which is the
# whole reason this file is not baked into the binary.
%config(noreplace) %attr(0640, root, {SERVICE_USER}) %{{_sysconfdir}}/sandesh/sandesh.env
%dir %attr(0750, {SERVICE_USER}, {SERVICE_USER}) {DATA_DIR}

%changelog
* Mon Jan 01 2024 Heliconia Solutions <hello@heliconia.io> - {package_version}-1
- Headless server package with a systemd unit.
"""


def build(arch: str, out: Path) -> Path:
    package_version = version()

    with tempfile.TemporaryDirectory() as tmp:
        staging = Path(tmp)
        sources = staging / "SOURCES"
        sources.mkdir()

        binary = binary_for(arch, staging)
        shutil.copy2(binary, sources / "sandesh")
        # Licence texts from the release archive, installed under %_licensedir.
        (sources / "legal").mkdir()
        for legal in ("LICENSE", "NOTICE", "THIRD_PARTY_LICENSES.txt"):
            if (binary.parent / legal).exists():
                shutil.copy2(binary.parent / legal, sources / "legal" / legal)
        (sources / "sandesh.service").write_text(SERVICE, encoding="utf-8")
        (sources / "sandesh.env").write_text(ENVIRONMENT_FILE, encoding="utf-8")
        (staging / "sandesh.spec").write_text(spec(package_version, arch), encoding="utf-8")

        out.mkdir(parents=True, exist_ok=True)

        # `--define` rather than a `~/.rpmmacros`, so the build leaves nothing
        # behind in the container's home directory and the paths are visible
        # here rather than in a file written elsewhere.
        print(f"  running rpmbuild in {BUILDER_IMAGE}")
        run(
            "docker", "run", "--rm",
            "-v", f"{staging}:/work",
            "-v", f"{out.resolve()}:/out",
            BUILDER_IMAGE,
            "sh", "-c",
            "set -e; "
            "dnf install -y -q rpm-build systemd-rpm-macros >/dev/null 2>&1; "
            # `_topdir` is inside the container, not the bind mount: rpmbuild
            # runs as root and its build tree would otherwise be left on the
            # host owned by root, where this script cannot remove it.
            "rpmbuild -bb /work/sandesh.spec "
            "  --define '_sourcedir /work/SOURCES'"
            "  --define '_topdir /tmp/rpmbuild'"
            f" --target {arch}; "
            "cp /tmp/rpmbuild/RPMS/*/*.rpm /out/; "
            # Hand the result back to the invoking user. Without this the `.rpm`
            # lands in dist/ owned by root and neither the checksum step below
            # nor a later `rm -rf dist` can touch it.
            f"chown {os.getuid()}:{os.getgid()} /out/*.rpm",
        )

    packages = sorted(out.glob(f"sandesh-{package_version}-*.{arch}.rpm"))
    if not packages:
        raise RuntimeError("rpmbuild produced no package")
    return packages[-1]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arch", default="x86_64", choices=sorted(ARCH_TARGET))
    parser.add_argument("--out", default="dist")
    args = parser.parse_args()

    if not shutil.which("docker"):
        print("docker is required: rpmbuild runs inside a Fedora container", file=sys.stderr)
        return 2

    out = Path(args.out) if os.path.isabs(args.out) else REPO / args.out
    print(f"packaging sandesh {version()} for {args.arch}")

    package = build(args.arch, out)
    digest = hashlib.sha256(package.read_bytes()).hexdigest()
    package.with_suffix(".rpm.sha256").write_text(
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
