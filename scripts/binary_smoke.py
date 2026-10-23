#!/usr/bin/env python3
"""Smoke-test a released binary by unpacking and running it.

`ROADMAP.md` Phase 10: "Every target builds in CI and runs a smoke test", and
"`docker run` ... reaches a working state in under 60 seconds" — whose second
half, the downloaded binary, had no test because there were no binaries.

This is the binary counterpart to `docker_smoke.py`, and it asserts one thing
that test structurally cannot: that the binary runs **outside a container**, on
a host whose libc is not the one it was linked against. The container smoke
test starts the binary in the same `scratch` filesystem it was built for, so a
dynamic link against something Alpine happens to provide would pass there and
fail on a user's machine. Running it here, on the host, is what makes "static
binary" a claim with a test behind it.

What is deliberately *not* re-asserted: the protocol semantics. Those are the
subject of several thousand tests against the same code. The question here is
only whether this artifact, as a user would download it, starts and works.

Usage:  scripts/binary_smoke.py dist/sandesh-0.1.0-x86_64-unknown-linux-musl.tar.gz
"""
import hashlib
import json
import os
import shutil
import smtplib
import socket
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.request
import zipfile
from email.message import EmailMessage
from pathlib import Path

# The criterion's own number, matching `docker_smoke.py`.
READY_BUDGET_SECONDS = 60


def free_port() -> int:
    """An ephemeral port the OS says is free.

    Fixed ports would let this pass because a Sandesh the developer already had
    running answered — the trap `docker_smoke.py` avoids by choosing unusual
    numbers, solved here more strictly since a released binary may well be
    tested on a machine already running one.
    """
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def verify_checksum(archive: Path) -> None:
    """Check the archive against its `.sha256`, if one was published beside it.

    A checksum nobody verifies is a checksum that can be wrong for a whole
    release cycle, so the release's own test verifies it the way a user would.
    """
    checksum_file = archive.with_suffix(archive.suffix + ".sha256")
    if not checksum_file.exists():
        print("  no .sha256 beside the archive; skipping checksum verification")
        return

    expected = checksum_file.read_text(encoding="utf-8").split()[0]
    actual = hashlib.sha256(archive.read_bytes()).hexdigest()
    assert actual == expected, (
        f"checksum mismatch: published {expected}, archive is {actual}"
    )
    print(f"  checksum verified ({actual[:16]}…)")


def unpack(archive: Path, into: Path) -> Path:
    """Extract the archive and return the binary inside it."""
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as zf:
            zf.extractall(into)
    else:
        with tarfile.open(archive) as tf:
            # `data` refuses absolute paths, `..` traversal and special files.
            # The archive is our own, but a smoke test that unpacks a download
            # unfiltered teaches the wrong habit to whoever copies it.
            tf.extractall(into, filter="data")

    candidates = [p for p in into.rglob("sandesh*") if p.is_file()]
    assert candidates, f"no sandesh binary inside {archive.name}"
    binary = candidates[0]
    binary.chmod(0o755)
    return binary


def report_linkage(binary: Path) -> None:
    """Print what the binary links against, and fail if it is not static.

    The Linux release targets are musl static builds; a dynamic dependency here
    means the artifact will fail on some user's distribution and pass on ours,
    which is the failure mode this whole script exists to catch early.
    """
    if sys.platform != "linux" or shutil.which("file") is None:
        return

    described = subprocess.run(
        ["file", str(binary)], capture_output=True, text=True, check=False
    ).stdout
    print(f"  {described.split(':', 1)[-1].strip()[:90]}")

    if "musl" in binary.parent.name or "static" in described:
        assert "dynamically linked" not in described, (
            "a musl release target must be statically linked; this binary is not"
        )


def wait_until_ready(http_port: int, process: subprocess.Popen, log: Path) -> float:
    started = time.monotonic()
    while time.monotonic() - started < READY_BUDGET_SECONDS:
        if process.poll() is not None:
            print(log.read_text(encoding="utf-8", errors="replace"), file=sys.stderr)
            raise AssertionError(
                f"the binary exited with code {process.returncode} before "
                "becoming ready"
            )
        try:
            url = f"http://127.0.0.1:{http_port}/api/v1/health"
            with urllib.request.urlopen(url, timeout=5) as response:
                if json.load(response).get("status") == "ok":
                    return time.monotonic() - started
        except (urllib.error.URLError, OSError, json.JSONDecodeError):
            pass
        time.sleep(0.25)

    print(log.read_text(encoding="utf-8", errors="replace"), file=sys.stderr)
    raise AssertionError(
        f"the binary did not reach a working state within {READY_BUDGET_SECONDS}s"
    )


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__, file=sys.stderr)
        return 2

    archive = Path(sys.argv[1]).resolve()
    assert archive.exists(), f"no such archive: {archive}"
    print(f"smoke-testing {archive.name}")

    verify_checksum(archive)

    with tempfile.TemporaryDirectory() as tmp:
        workdir = Path(tmp)
        binary = unpack(archive, workdir / "unpacked")
        report_linkage(binary)

        http_port, smtp_port = free_port(), free_port()
        data_dir = workdir / "data"
        data_dir.mkdir()
        log = workdir / "sandesh.log"

        environment = {
            "SANDESH_DATA_DIR": str(data_dir),
            "SANDESH_HTTP_ADDR": f"127.0.0.1:{http_port}",
            "SANDESH_SMTP_ADDR": f"127.0.0.1:{smtp_port}",
            # Ephemeral, so the test never collides with a running Sandesh.
            "SANDESH_IMAP_ADDR": f"127.0.0.1:{free_port()}",
            "SANDESH_POP3_ADDR": f"127.0.0.1:{free_port()}",
            "PATH": "/usr/bin:/bin",
        }
        # Windows cannot start Winsock without SYSTEMROOT: in a process whose
        # environment lacks it every bind fails (WSAEPROVIDERFAILEDINIT), which
        # Rust reports as `Uncategorized`. The environment above is otherwise
        # deliberately minimal, so pass only that one through.
        if os.name == "nt":
            environment["SYSTEMROOT"] = os.environ["SYSTEMROOT"]

        with log.open("wb") as sink:
            process = subprocess.Popen(
                [str(binary)], env=environment, stdout=sink, stderr=subprocess.STDOUT
            )

        try:
            elapsed = wait_until_ready(http_port, process, log)
            print(f"  ready in {elapsed:.2f}s (budget {READY_BUDGET_SECONDS}s)")

            base = f"http://127.0.0.1:{http_port}"

            # "serves the UI" — the embedded assets survived packaging, which
            # is the half a plain `--version` check would miss entirely.
            with urllib.request.urlopen(f"{base}/", timeout=5) as response:
                root = response.read().decode("utf-8", "replace")
            assert '<div id="root"' in root, "the binary must serve the embedded UI"
            print("  serves the embedded UI")

            # "accepts mail"
            message = EmailMessage()
            message["From"] = "smoke@example.test"
            message["To"] = "dev@example.test"
            message["Subject"] = "Binary smoke test"
            message.set_content("plain body")
            with smtplib.SMTP("127.0.0.1", smtp_port, timeout=10) as client:
                client.send_message(message)

            for _ in range(40):
                with urllib.request.urlopen(f"{base}/api/v1/messages", timeout=5) as r:
                    if json.load(r)["total_estimate"] >= 1:
                        break
                time.sleep(0.25)
            else:
                raise AssertionError("delivered mail never became queryable")
            print("  accepts mail over SMTP and serves it over the API")

        finally:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()

        # A mail server that ignores SIGTERM is one an orchestrator kills
        # ungracefully on every deploy, so the shutdown path is part of the
        # artifact working, not a nicety.
        assert process.returncode is not None, "the binary did not stop on SIGTERM"
        print("  shuts down on SIGTERM")

    print("binary smoke test passed")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (AssertionError, RuntimeError, OSError) as error:
        print(f"binary smoke test FAILED: {error}", file=sys.stderr)
        sys.exit(1)
