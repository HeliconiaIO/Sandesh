#!/usr/bin/env python3
"""Install the `.rpm` in a real Fedora container and assert it works.

The counterpart to `deb_smoke.py`, and for the same reason: `rpm -qip` shows
what a package claims about itself, while only installing it shows whether the
scriptlets run, whether the service account is created, and whether the binary
starts from its installed path.

Fedora is deliberately a distribution the binary was *not* built on, and one
whose libc is glibc rather than the musl it was linked against. The package
declares no library dependencies; installing it here is what tests that claim.

Usage:  scripts/rpm_smoke.py [path/to/sandesh-<version>-1.fc41.x86_64.rpm]
"""
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
IMAGE = "fedora:41"
CONTAINER = "sandesh-rpm-smoke"

SCRIPT = r"""
set -eu

echo "--- installing"
# `rpm -i` without --nodeps: the package must resolve on its own. An invented
# dependency such as the `rtld(GNU_HASH)` rpm's ELF scanner emits for a static
# binary would fail here rather than on a user's machine.
rpm -i /tmp/sandesh.rpm

echo "--- installed files"
test -x /usr/bin/sandesh                            || { echo "binary missing"; exit 1; }
test -f /usr/lib/systemd/system/sandesh.service     || { echo "unit missing"; exit 1; }
test -f /etc/sandesh/sandesh.env                    || { echo "config missing"; exit 1; }

echo "--- no dependency on a C runtime"
# The whole point of the musl static build. `rpm -qR` must not name glibc or a
# dynamic loader.
if rpm -qR sandesh | grep -qiE 'rtld|glibc|libc\.so'; then
    echo "package declares a C runtime dependency it does not have:"
    rpm -qR sandesh | grep -iE 'rtld|glibc|libc\.so'
    exit 1
fi

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
# `su` comes from util-linux, which minimal Fedora images omit. Installed here
# for the *test harness*, not as a package dependency: dropping privileges is
# how this asserts the binary runs unprivileged, and there is no other tool for
# it in the image (runuser, setpriv and python3 are all absent too).
dnf install -y -q util-linux >/dev/null 2>&1

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

echo "--- erase keeps mail"
# RPM has no purge/remove distinction: `rpm -e` removes the package, and the
# data directory is marked %dir rather than %ghost precisely so that mail is
# not swept up with it.
touch /var/lib/sandesh/canary
rpm -e sandesh
test -f /var/lib/sandesh/canary || { echo "erase destroyed user data"; exit 1; }
echo "    mail survived rpm -e"

echo "--- an edited config is saved on erase, never silently destroyed"
# The guarantee worth asserting, and it took a corrected test to state it.
# `%config(noreplace)` means rpm removes an *unmodified* config on erase — no
# edit is lost, since the file is byte-identical to what shipped — but a
# *modified* one is preserved as `.rpmsave`. The first version of this test
# asserted the file always survives, which failed against correct behaviour.
rpm -i /tmp/sandesh.rpm
echo "SANDESH_LOG=debug" >> /etc/sandesh/sandesh.env
rpm -e sandesh

test -f /etc/sandesh/sandesh.env.rpmsave || {
    echo "an edited config was destroyed on erase"; exit 1;
}
grep -q "SANDESH_LOG=debug" /etc/sandesh/sandesh.env.rpmsave || {
    echo ".rpmsave exists but does not hold the operator's edit"; exit 1;
}
echo "    edits preserved as sandesh.env.rpmsave"

echo "--- an upgrade does not overwrite an edited config"
# This is what `%config(noreplace)` actually governs. Asserted separately
# because the erase check above does *not* pin it: removing `noreplace` still
# produces a .rpmsave on erase, so that test passed against the broken spec.
# Reinstalling over itself is the upgrade path rpm takes.
rm -f /etc/sandesh/sandesh.env.rpmsave
rpm -i /tmp/sandesh.rpm
echo "SANDESH_LOG=trace" >> /etc/sandesh/sandesh.env
rpm -U --replacepkgs /tmp/sandesh.rpm

grep -q "SANDESH_LOG=trace" /etc/sandesh/sandesh.env || {
    echo "the upgrade discarded the operator's edit"
    ls -la /etc/sandesh/
    exit 1
}

# Honest limit of this check: it does NOT pin `%config(noreplace)`. That
# directive only diverges from plain `%config` when the *shipped* config
# differs between versions, and reinstalling a package over itself cannot
# produce that. Both spellings pass here; verified directly with a pair of
# throwaway packages whose configs differed, where `noreplace` kept the
# operator's file and parked the new one as `.rpmnew`, while plain `%config`
# overwrote the live file and moved the edit aside to `.rpmsave` — silently
# ending its effect. Pinning it properly needs two real releases, so it waits
# for the second one rather than being faked with a synthetic version here.
#
# What this does assert: the upgrade path leaves the live config alone and
# writes no stray files, which is worth having regardless.
for stray in /etc/sandesh/sandesh.env.rpmsave /etc/sandesh/sandesh.env.rpmnew; do
    test ! -f "$stray" || {
        echo "the upgrade side-stepped the live config: $stray was written"
        ls -la /etc/sandesh/
        exit 1
    }
done
echo "    the edit survived the upgrade, with no .rpmnew/.rpmsave written"
"""


def main() -> int:
    if len(sys.argv) > 1:
        package = Path(sys.argv[1])
    else:
        candidates = sorted((REPO / "dist").glob("sandesh-*.rpm"))
        if not candidates:
            print("no .rpm found; run scripts/package_rpm.py first", file=sys.stderr)
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
            "-v", f"{package.resolve()}:/tmp/sandesh.rpm:ro",
            IMAGE, "sh", "-c", SCRIPT,
        ],
        text=True,
        check=False,
    )

    if result.returncode != 0:
        print("rpm smoke test FAILED", file=sys.stderr)
        return 1

    print("rpm smoke test passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
