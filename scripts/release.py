#!/usr/bin/env python3
"""Build a release artifact for one target, with its checksum.

`ROADMAP.md` Phase 10 asks for static binaries per target and "reproducible
checksums published per release". This produces both for one target at a time,
so the same code path runs locally and in the release matrix — a release script
that only ever executes in CI is a script whose failures are discovered during a
release.

## Why the Linux target is built through Docker

`x86_64-unknown-linux-musl` is built by reusing the `build` stage of the
`Dockerfile` (ADR-095) rather than by invoking `cargo build --target` here.
That stage already pins the musl toolchain, the C dependencies `ring` and
`sqlx` need, and the frontend build that `rust-embed` compiles in. Driving
`cargo` directly would be a *second* description of the same build, free to
drift from the one the container image is actually tested against — and the
binary that ships would then be one nothing had smoke-tested.

The other targets have no such stage: macOS and Windows binaries must be built
on their own runners, because cross-linking them needs an SDK that cannot be
redistributed. `--target` accepts them so the release workflow can call this
script uniformly; on a host that cannot build the requested target the script
says so and exits, rather than emitting something untested.

Usage:  scripts/release.py --target x86_64-unknown-linux-musl [--out dist]
"""
import argparse
import gzip
import hashlib
import os
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile
import zipfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# The targets Phase 10 lists. `docker` marks the ones this script can build
# anywhere via the Dockerfile's build stage; the rest need a matching host.
TARGETS = {
    "x86_64-unknown-linux-musl": {"os": "linux", "how": "docker", "ext": ""},
    "aarch64-unknown-linux-musl": {"os": "linux", "how": "docker", "ext": ""},
    "x86_64-apple-darwin": {"os": "darwin", "how": "cargo", "ext": ""},
    "aarch64-apple-darwin": {"os": "darwin", "how": "cargo", "ext": ""},
    "x86_64-pc-windows-msvc": {"os": "windows", "how": "cargo", "ext": ".exe"},
}

# Docker's own names for the platforms above, since they do not match Rust's.
DOCKER_PLATFORM = {
    "x86_64-unknown-linux-musl": "linux/amd64",
    "aarch64-unknown-linux-musl": "linux/arm64",
}

HOST_OS = {"Linux": "linux", "Darwin": "darwin", "Windows": "windows"}.get(
    platform.system(), platform.system().lower()
)


def run(*args: str, **kwargs) -> str:
    result = subprocess.run(
        args, capture_output=True, text=True, check=False, **kwargs
    )
    if result.returncode != 0:
        raise RuntimeError(f"{args[0]} failed: {result.stderr.strip()[-2000:]}")
    return result.stdout.strip()


def version() -> str:
    """The workspace version, read from the manifest rather than hardcoded."""
    manifest = (REPO / "Cargo.toml").read_text(encoding="utf-8")
    inside_workspace_package = False
    for line in manifest.splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            inside_workspace_package = stripped == "[workspace.package]"
        elif inside_workspace_package and stripped.startswith("version"):
            return stripped.split("=", 1)[1].strip().strip('"')
    raise RuntimeError("no version found in [workspace.package]")


def build_via_docker(target: str, staging: Path) -> Path:
    """Extract the static binary from the Dockerfile's build stage.

    Builds the `runtime` stage — the same image `docker_smoke.py` asserts
    against — and copies the binary out of it, so the released file and the
    tested image hold the same bytes rather than two separate builds of the
    same source.
    """
    platform_flag = DOCKER_PLATFORM[target]
    tag = f"sandesh:release-{target}"

    print(f"  building {platform_flag} image (this is the slow part)")
    run(
        "docker", "build",
        "--platform", platform_flag,
        "--target", "runtime",
        "-t", tag,
        str(REPO),
        cwd=REPO,
    )

    # `docker cp` needs a container, but the image has no shell to run, so
    # `create` without `start` is the way to get a filesystem to copy from.
    container = run("docker", "create", "--platform", platform_flag, tag)
    try:
        destination = staging / "sandesh"
        run("docker", "cp", f"{container}:/sandesh", str(destination))
    finally:
        run("docker", "rm", container)

    return destination


def build_via_cargo(target: str, staging: Path) -> Path:
    """Build natively for a target the host can link."""
    extension = TARGETS[target]["ext"]

    # The UI is compiled into the binary, so it must exist before cargo runs —
    # the same ordering the Dockerfile enforces with its stage order.
    if not (REPO / "web" / "dist" / "index.html").exists():
        raise RuntimeError(
            "web/dist is missing; run `npm ci && npm run build` in web/ first "
            "(rust-embed compiles it into the binary)"
        )

    print(f"  cargo build --release --target {target}")
    run(
        "cargo", "build", "--release", "--locked",
        "--bin", "sandesh", "--target", target,
        cwd=REPO,
    )

    built = REPO / "target" / target / "release" / f"sandesh{extension}"
    destination = staging / built.name
    shutil.copy2(built, destination)
    return destination


def legal_files() -> list[Path]:
    """The licence files that must travel with every binary.

    Apache-2.0 §4 requires LICENSE and NOTICE to accompany a redistribution, and
    the dependencies' own licences require their notices to as well. The third-
    party file is generated (`third_party_notices.py`); a local build without it
    still packages, with a warning, but CI generates it before this runs.
    """
    files = [REPO / "LICENSE", REPO / "NOTICE"]
    notices = REPO / "THIRD_PARTY_LICENSES.txt"
    if notices.exists():
        files.append(notices)
    else:
        print("warning: THIRD_PARTY_LICENSES.txt missing; run "
              "scripts/third_party_notices.py before a public release", file=sys.stderr)
    return files


def package(binary: Path, target: str, out: Path) -> Path:
    """Archive the binary in the convention of its platform."""
    stem = f"sandesh-{version()}-{target}"
    extras = legal_files()

    if TARGETS[target]["os"] == "windows":
        archive = out / f"{stem}.zip"
        with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as zf:
            zf.write(binary, binary.name)
            for extra in extras:
                zf.write(extra, extra.name)
        return archive

    archive = out / f"{stem}.tar.gz"
    # A fixed mtime and a normalised owner: the checksum must depend on the
    # binary, not on when or by whom the tarball was rolled. Without this the
    # "reproducible checksums" criterion fails on the packaging step alone,
    # even with a byte-identical binary inside.
    def normalise(info: tarfile.TarInfo) -> tarfile.TarInfo:
        info.uid = info.gid = 0
        info.uname = info.gname = "root"
        info.mtime = 0
        return info

    # Written as an explicit GzipFile with `mtime=0`, not `tarfile.open("w:gz")`.
    # Normalising the tar *entries* is not enough: gzip stamps its own
    # timestamp into its header, so two builds of a byte-identical binary still
    # produced two different checksums — measured, not assumed. The tar layer
    # and the binary already matched; the wrapper was the whole difference.
    with open(archive, "wb") as raw:
        with gzip.GzipFile(
            filename="", mode="wb", fileobj=raw, compresslevel=9, mtime=0
        ) as gz:
            with tarfile.open(fileobj=gz, mode="w") as tf:
                tf.add(binary, arcname=binary.name, filter=normalise)
                for extra in extras:
                    tf.add(extra, arcname=extra.name, filter=normalise)
    return archive


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True, choices=sorted(TARGETS))
    parser.add_argument("--out", default="dist", help="output directory")
    args = parser.parse_args()

    target = args.target
    spec = TARGETS[target]
    out = (REPO / args.out) if not os.path.isabs(args.out) else Path(args.out)
    out.mkdir(parents=True, exist_ok=True)

    # Refuse early and by name, rather than producing a binary for the wrong
    # platform or failing deep inside a linker.
    if spec["how"] == "cargo" and spec["os"] != HOST_OS:
        print(
            f"{target} must be built on {spec['os']}; this host is {HOST_OS}. "
            "Cross-linking it needs a platform SDK that cannot be shipped here.",
            file=sys.stderr,
        )
        return 2

    print(f"releasing sandesh {version()} for {target}")

    with tempfile.TemporaryDirectory() as tmp:
        staging = Path(tmp)
        if spec["how"] == "docker":
            binary = build_via_docker(target, staging)
        else:
            binary = build_via_cargo(target, staging)

        size_mb = binary.stat().st_size / 1_000_000
        print(f"  binary: {binary.name}, {size_mb:.1f} MB")

        archive = package(binary, target, out)

    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    checksum_file = archive.with_suffix(archive.suffix + ".sha256")
    # The `sha256sum -c` format, so a user can verify a download with the tool
    # already on their machine rather than one we ask them to install.
    checksum_file.write_text(f"{digest}  {archive.name}\n", encoding="utf-8")

    print(f"  {archive.name}  ({archive.stat().st_size / 1_000_000:.1f} MB)")
    print(f"  {digest}")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (RuntimeError, OSError) as error:
        print(f"release FAILED: {error}", file=sys.stderr)
        sys.exit(1)
