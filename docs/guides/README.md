# Sandesh — End-User Guides

**Sandesh** is a local mail development environment. Point your application's SMTP client at it, and inspect, search, and reply to mail through a modern developer console — over SMTP, IMAP, and POP3.

Pick the guide that matches how you received Sandesh:

---

## Choose Your Setup

| Guide | What you have | Best for |
| :--- | :--- | :--- |
| [01-DOCKER.md](01-DOCKER.md) | A Docker / Podman image (e.g. `sandesh.tar` or `sandesh:latest`) | Developers, CI environments, shared team setups |
| [02-BINARY-LINUX.md](02-BINARY-LINUX.md) | A `.tar.gz` file (e.g. `sandesh-0.2.0-x86_64-unknown-linux-musl.tar.gz`) | Any Linux machine, no installation needed |
| [03-BINARY-MACOS.md](03-BINARY-MACOS.md) | A `.tar.gz` file (e.g. `sandesh-0.2.0-aarch64-apple-darwin.tar.gz`) | macOS (Apple Silicon or Intel) |
| [04-BINARY-WINDOWS.md](04-BINARY-WINDOWS.md) | A `.zip` file (e.g. `sandesh-0.2.0-x86_64-pc-windows-msvc.zip`) | Windows 10 / 11 |
| [05-DEBIAN-UBUNTU.md](05-DEBIAN-UBUNTU.md) | A `.deb` file (e.g. `sandesh_0.2.0_amd64.deb`) | Debian, Ubuntu, or any `apt`-based system |
| [06-RHEL-FEDORA.md](06-RHEL-FEDORA.md) | A `.rpm` file (e.g. `sandesh-0.2.0-1.fc41.x86_64.rpm`) | Fedora, RHEL, CentOS, Rocky Linux, AlmaLinux |
| [07-DESKTOP.md](07-DESKTOP.md) | A desktop app (AppImage / `.deb` GUI) | Developers who want a system tray app + native notifications |

---

## What Sandesh Provides

Once running, Sandesh exposes:

| Service | Port (Local) | Port (Container) | Purpose |
| :--- | :--- | :--- | :--- |
| SMTP | `1025` | `1025` | Receives mail from your application |
| Web UI | `8080` | `1080` | Browser-based mail inspector & composer |
| REST API | `8080/api/v1` | `1080/api/v1` | Automation, test assertions |
| IMAP | `1143` | `1143` | Read mail via any email client |
| POP3 | `1110` | `1110` | Read mail via legacy clients |

> **Developer Mode** — no passwords to set up. Connect your IMAP/POP3 client using **any username and any password**.

---

## After Setup — Test Your Mail Flow

All guides end with the same E2E test:
1. Send a message to Sandesh over SMTP.
2. Open the Web UI and see it appear instantly.
3. Read it back over IMAP or POP3.
4. Confirm it via the REST API.

---

*Navigate to the guide for your artifact to get started.*
