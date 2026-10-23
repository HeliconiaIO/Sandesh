#!/usr/bin/env python3
"""Install the `.deb` in a real Debian container and assert it works.

`ROADMAP.md` Phase 10's tests line asks for "install-path verification", and
the phase's standing rule since ADR-095 is that packaging claims are asserted by
*running* the artifact rather than by reading its metadata. `dpkg-deb --info`
shows what a package says about itself; only `apt install` shows whether the
maintainer scripts run, whether the service user is created, whether the systemd
unit is accepted, and whether the binary starts from its installed path.

Uses `debian:stable-slim` — deliberately a distribution the binary was *not*
built on. The package declares no dependencies because the binary is statically
linked against musl; installing it on Debian and running it is what tests that
claim rather than restating it.

Usage:  scripts/deb_smoke.py [path/to/sandesh_<version>_amd64.deb]
"""
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
IMAGE = "debian:stable-slim"
CONTAINER = "sandesh-deb-smoke"

# Run as a script inside the container: each assertion prints a line, and `set
# -e` aborts at the first failure so the output ends at the step that broke.
SCRIPT = r"""
set -eu

# Without this, debconf's failure to find a usable frontend prints a dozen lines
# of Perl warnings that buried the real dpkg error the first time this ran.
export DEBIAN_FRONTEND=noninteractive

echo "--- installing"
# `apt install` rather than `dpkg -i`, because it is what a user runs and it
# resolves dependencies. A package wrongly declaring one fails loudly here.
apt-get update -qq >/dev/null 2>&1
apt-get install -y -qq /tmp/sandesh.deb >/dev/null

echo "--- installed files"
test -x /usr/bin/sandesh                       || { echo "binary missing"; exit 1; }
test -f /lib/systemd/system/sandesh.service    || { echo "unit missing"; exit 1; }
test -f /etc/sandesh/sandesh.env               || { echo "config missing"; exit 1; }

echo "--- the service user exists"
getent passwd sandesh >/dev/null || { echo "service user not created"; exit 1; }

echo "--- the data directory is owned and private"
owner=$(stat -c '%U:%G %a' /var/lib/sandesh)
echo "    /var/lib/sandesh -> $owner"
test "$owner" = "sandesh:sandesh 750" || { echo "wrong ownership/mode"; exit 1; }

echo "--- the config is not world-readable"
mode=$(stat -c '%a' /etc/sandesh/sandesh.env)
test "$mode" = "640" || { echo "config mode is $mode, expected 640"; exit 1; }

echo "--- the binary identifies itself"
/usr/bin/sandesh --version

echo "--- it runs as the service user, from the installed path"
# The real test of a static binary: this container has no Rust, no musl and a
# different libc from the one it was linked against.
su -s /bin/sh sandesh -c '
    cd /var/lib/sandesh
    SANDESH_DATA_DIR=/var/lib/sandesh \
    SANDESH_HTTP_ADDR=127.0.0.1:18080 \
    SANDESH_SMTP_ADDR=127.0.0.1:18025 \
    SANDESH_IMAP_ADDR=127.0.0.1:18143 \
    SANDESH_POP3_ADDR=127.0.0.1:18110 \
    /usr/bin/sandesh >/tmp/sandesh.log 2>&1 &
    echo $! > /tmp/sandesh.pid

    for _ in $(seq 1 60); do
        if grep -q "Sandesh ready" /tmp/sandesh.log 2>/dev/null; then break; fi
        sleep 0.5
    done

    grep -q "Sandesh ready" /tmp/sandesh.log || { cat /tmp/sandesh.log; exit 1; }
    kill "$(cat /tmp/sandesh.pid)" 2>/dev/null || true
'
echo "    started and stopped cleanly"

echo "--- removal keeps mail, purge removes it"
touch /var/lib/sandesh/canary
apt-get remove -y -qq sandesh >/dev/null
test -f /var/lib/sandesh/canary || { echo "remove destroyed user data"; exit 1; }
apt-get purge -y -qq sandesh >/dev/null
test ! -d /var/lib/sandesh || { echo "purge left data behind"; exit 1; }
echo "    remove kept it; purge removed it"
"""


def main() -> int:
    if len(sys.argv) > 1:
        package = Path(sys.argv[1])
    else:
        candidates = sorted((REPO / "dist").glob("sandesh_*_amd64.deb"))
        if not candidates:
            print("no .deb found; run scripts/package_deb.py first", file=sys.stderr)
            return 2
        package = candidates[-1]

    if not package.exists():
        print(f"no such package: {package}", file=sys.stderr)
        return 2

    print(f"smoke-testing {package.name} on {IMAGE}")

    subprocess.run(["docker", "rm", "-f", CONTAINER], capture_output=True, check=False)
    result = subprocess.run(
        [
            "docker", "run", "--rm", "--name", CONTAINER,
            "-v", f"{package.resolve()}:/tmp/sandesh.deb:ro",
            IMAGE, "sh", "-c", SCRIPT,
        ],
        text=True,
        check=False,
    )

    if result.returncode != 0:
        print("deb smoke test FAILED", file=sys.stderr)
        return 1

    print("deb smoke test passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
