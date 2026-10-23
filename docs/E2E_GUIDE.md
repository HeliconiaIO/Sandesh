# Sandesh — End-to-End User Guide

**Sandesh** is a local mail development environment. Run it on your machine, point your application's SMTP client at it, and inspect, search, reply to, and replay your mail in a modern developer console — with no external mail provider required.

---

## Quick Start — Choose Your Setup

Find the guide that matches what you have:

| I have... | Guide |
| :--- | :--- |
| A Docker or Podman image (`sandesh.tar` or `sandesh:latest`) | [01-DOCKER.md](guides/01-DOCKER.md) |
| A `.tar.gz` file for Linux (e.g. `sandesh-0.1.0-x86_64-unknown-linux-musl.tar.gz`) | [02-BINARY-LINUX.md](guides/02-BINARY-LINUX.md) |
| A `.tar.gz` file for macOS (e.g. `sandesh-0.1.0-aarch64-apple-darwin.tar.gz`) | [03-BINARY-MACOS.md](guides/03-BINARY-MACOS.md) |
| A `.zip` file for Windows (e.g. `sandesh-0.1.0-x86_64-pc-windows-msvc.zip`) | [04-BINARY-WINDOWS.md](guides/04-BINARY-WINDOWS.md) |
| A `.deb` file (Debian / Ubuntu) | [05-DEBIAN-UBUNTU.md](guides/05-DEBIAN-UBUNTU.md) |
| A `.rpm` file (Fedora / RHEL / CentOS / Rocky Linux) | [06-RHEL-FEDORA.md](guides/06-RHEL-FEDORA.md) |
| A desktop app (AppImage or `.deb` GUI app with tray icon) | [07-DESKTOP.md](guides/07-DESKTOP.md) |

---

## Default Ports — Quick Reference

Once Sandesh is running, these are the addresses your application and tools connect to:

| Service | Local Binary Port | Container Port | Purpose |
| :--- | :--- | :--- | :--- |
| SMTP | `1025` | `1025` | Your app sends mail here |
| Web UI | `8080` | `1080` | Browser-based mail console |
| REST API | `8080/api/v1` | `1080/api/v1` | Automated queries, test assertions |
| IMAP | `1143` | `1143` | Read mail via any email client |
| POP3 | `1110` | `1110` | Read mail via legacy clients |
| WebSocket Events | `8080/api/v1/events` | `1080/api/v1/events` | Real-time new-mail stream |

> **Authentication is disabled by default.** IMAP and POP3 accept any username and any password — no setup needed for local development.

---

## E2E Mail Flow — Verify Everything Works

After completing your target-specific guide, run through this flow to confirm end-to-end delivery:

### 1. Send a message via SMTP

```python
import smtplib
from email.message import EmailMessage

msg = EmailMessage()
msg["From"]    = "myapp@example.com"
msg["To"]      = "you@example.com"
msg["Subject"] = "Sandesh E2E Test"
msg.set_content("Hello! Sandesh is working correctly.")

# Use port 1025 for local binary, 1025 for Docker (both the same)
with smtplib.SMTP("localhost", 1025) as s:
    s.send_message(msg)
    print("Sent!")
```

### 2. Confirm health via REST API

```bash
curl http://localhost:8080/api/v1/health
# {"status":"ok","version":"0.1.0"}

# Docker: use port 1080
curl http://localhost:1080/api/v1/health
```

### 3. List messages via REST API

```bash
curl -s http://localhost:8080/api/v1/messages | jq .items[0].subject
# "Sandesh E2E Test"
```

### 4. Read via IMAP

```python
import imaplib

m = imaplib.IMAP4("localhost", 1143)
m.login("you@example.com", "test")   # any credentials
m.select("INBOX")
_, ids = m.search(None, "ALL")
_, data = m.fetch(ids[0].split()[0], "(RFC822)")
print(data[0][1].decode()[:300])
m.logout()
```

### 5. Read via POP3

```python
import poplib

pop = poplib.POP3("localhost", 1110)
pop.user("you@example.com")
pop.pass_("test")
count, _ = pop.stat()
print(f"{count} message(s) in inbox")
pop.quit()
```

### 6. Inspect in the browser

Open **http://localhost:8080** (or **:1080** for Docker) and verify the message appears with:
- Full headers and hop timeline
- Rendered HTML body in sandboxed iframe
- MIME part tree
- The raw SMTP conversation transcript

---

## For Developers with Source Access

If you have the Sandesh source code and want to build and run all targets yourself, see the [developer build guide](INTEGRATION_GUIDE.md).
