#!/usr/bin/env python3
"""Smoke-test the container image against Phase 10's acceptance criteria.

`ROADMAP.md` Phase 10 asks three things of the image, and this asserts each
against a *running container* rather than inspecting the Dockerfile:

  1. "The Docker image starts, accepts mail, serves the UI"
  2. "`docker run` ... reaches a working state in under 60 seconds"
  3. every protocol listener works, because the image is the whole server

The distinction matters. A Dockerfile that builds proves the image exists; only
running it proves the binary inside is statically linked correctly, that
`scratch` carries everything it needs, and that the embedded UI survived the
multi-stage copy. The first two of those have no other test.

Usage:  scripts/docker_smoke.py [image-tag] [platform]
Exits non-zero, naming the criterion that failed.

`platform` (e.g. `linux/arm64`) runs a foreign-architecture image under binfmt
emulation. The ready budget is asserted for the native image only: emulation
measures QEMU, not the server, and a budget that fails under it would be
reporting the wrong thing.
"""
import imaplib
import json
import poplib
import smtplib
import subprocess
import sys
import time
import urllib.error
import urllib.request
from email.message import EmailMessage

IMAGE = sys.argv[1] if len(sys.argv) > 1 else "sandesh:test"
CONTAINER = "sandesh-smoke-test"

# An optional `--platform` for `docker run`, so a foreign-architecture image is
# asserted the same way the native one is rather than being taken on trust.
#
# Needed because `docker run` defaults to the host's architecture: on an amd64
# runner an arm64 image would otherwise either be refused or silently resolve to
# a different variant, and the run would prove nothing about the arm64 build. It
# requires binfmt emulation to be registered (`tonistiigi/binfmt`), which is what
# CI's `setup-qemu-action` does.
PLATFORM = sys.argv[2] if len(sys.argv) > 2 else None

# Deliberately not the defaults: the test must not pass because a Sandesh the
# developer already had running answered on 1080.
SMTP_PORT, HTTP_PORT, IMAP_PORT, POP3_PORT = 18925, 18980, 18943, 18910

# The criterion's own number. Generous on purpose — the point is to catch an
# image that hangs or crash-loops, not to benchmark the host.
READY_BUDGET_SECONDS = 60

# Under emulation the same image is asserted, but not against that number. A
# QEMU-translated binary is slower by a factor that has nothing to do with
# Sandesh, so holding it to the 60 s criterion would report on the emulator. The
# wait still has a ceiling, because a crash-loop must fail rather than hang.
EMULATED_CEILING_SECONDS = 300


def run(*args: str) -> str:
    """A docker command, failing loudly rather than silently continuing."""
    result = subprocess.run(
        ["docker", *args], capture_output=True, text=True, check=False
    )
    if result.returncode != 0:
        raise RuntimeError(f"docker {' '.join(args)} failed: {result.stderr.strip()}")
    return result.stdout.strip()


def http_json(path: str) -> dict:
    with urllib.request.urlopen(f"http://127.0.0.1:{HTTP_PORT}{path}", timeout=5) as r:
        return json.load(r)


def http_text(path: str) -> str:
    with urllib.request.urlopen(f"http://127.0.0.1:{HTTP_PORT}{path}", timeout=5) as r:
        return r.read().decode("utf-8", "replace")


def wait_until_ready() -> float:
    """Seconds until /health answers, or a failure naming the budget."""
    limit = EMULATED_CEILING_SECONDS if PLATFORM else READY_BUDGET_SECONDS
    started = time.monotonic()
    while time.monotonic() - started < limit:
        try:
            if http_json("/api/v1/health").get("status") == "ok":
                return time.monotonic() - started
        except (urllib.error.URLError, OSError, json.JSONDecodeError):
            pass
        time.sleep(0.25)

    # The logs are the useful part of this failure, so print them rather than
    # leaving the operator to go looking.
    print(run("logs", CONTAINER), file=sys.stderr)
    raise AssertionError(
        f"the image did not reach a working state within {limit}s"
    )


def deliver() -> None:
    message = EmailMessage()
    message["From"] = "smoke@example.test"
    message["To"] = "dev@example.test"
    message["Subject"] = "Container smoke test"
    message.set_content("plain body")
    message.add_alternative(
        '<html><body><a href="https://example.test/smoke">link</a></body></html>',
        subtype="html",
    )
    with smtplib.SMTP("127.0.0.1", SMTP_PORT, timeout=10) as client:
        client.send_message(message)


def main() -> int:
    run("rm", "-f", CONTAINER)
    print(f"starting {IMAGE}")
    run(
        "run", "-d", "--name", CONTAINER,
        *(("--platform", PLATFORM) if PLATFORM else ()),
        "-p", f"{SMTP_PORT}:1025",
        "-p", f"{HTTP_PORT}:1080",
        "-p", f"{IMAP_PORT}:1143",
        "-p", f"{POP3_PORT}:1110",
        IMAGE,
    )

    try:
        elapsed = wait_until_ready()
        if PLATFORM:
            # Reported, not asserted: see EMULATED_CEILING_SECONDS.
            print(f"  ready in {elapsed:.2f}s (emulated; budget not asserted)")
        else:
            print(f"  ready in {elapsed:.2f}s (budget {READY_BUDGET_SECONDS}s)")

        # "serves the UI" — the embedded assets, not merely a 200 from the API.
        root = http_text("/")
        assert '<div id="root"' in root, "the image must serve the embedded UI"
        print("  serves the embedded UI")

        # "accepts mail"
        deliver()
        for _ in range(40):
            listing = http_json("/api/v1/messages")
            if listing["total_estimate"] >= 1:
                break
            time.sleep(0.25)
        else:
            raise AssertionError("delivered mail never became queryable")
        print("  accepts mail over SMTP and serves it over the API")

        # Every listener, because the image is the whole server. A missing IMAP
        # port would otherwise be invisible until a user reported it.
        with imaplib.IMAP4("127.0.0.1", IMAP_PORT) as imap:
            imap.login("dev@example.test", "smoke")
            status, data = imap.select("INBOX")
            assert status == "OK" and int(data[0]) >= 1, "IMAP must see the message"
        print("  IMAP serves the same message")

        pop = poplib.POP3("127.0.0.1", POP3_PORT, timeout=10)
        pop.user("dev@example.test")
        pop.pass_("smoke")
        count, _ = pop.stat()
        pop.quit()
        assert count >= 1, "POP3 must see the message"
        print("  POP3 serves the same message")

        # Mail must outlive the container process, or the declared volume is
        # decorative.
        run("restart", CONTAINER)
        wait_until_ready()
        assert http_json("/api/v1/messages")["total_estimate"] >= 1, (
            "mail must survive a container restart"
        )
        print("  mail survives a restart")

    finally:
        run("rm", "-f", CONTAINER)

    print("docker smoke test passed")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (AssertionError, RuntimeError) as error:
        print(f"docker smoke test FAILED: {error}", file=sys.stderr)
        sys.exit(1)
