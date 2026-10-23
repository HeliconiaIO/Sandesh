# Sandesh on Docker / Podman

**What you need**: Internet access to pull the official image, `ghcr.io/heliconiaio/sandesh`
(multi-arch: `linux/amd64` and `linux/arm64`). Offline? A `.tar` of the image works too; see
step 1.

**Prerequisites**: Docker Desktop, Docker Engine, or Podman installed and running.

---

## Step 1 — Get the Image

```bash
docker pull ghcr.io/heliconiaio/sandesh:latest
```

Use a version tag such as `:0.1.0` instead of `:latest` when you need a fixed version, e.g. in CI.

Offline, with a `.tar` someone shared: `docker load -i sandesh.tar`, then use `sandesh:latest` in
place of the image name below.

Verify it loaded:
```bash
docker images | grep sandesh
```

---

## Step 2 — Start Sandesh

```bash
docker run -d \
  --name sandesh \
  -p 1025:1025 \
  -p 1080:1080 \
  -p 1143:1143 \
  -p 1110:1110 \
  -v sandesh-data:/data \
  ghcr.io/heliconiaio/sandesh:latest
```

What each flag does:
- `-d` — runs in the background
- `--name sandesh` — gives the container a memorable name
- `-p 1025:1025` — SMTP (your app sends mail here)
- `-p 1080:1080` — Web UI and REST API
- `-p 1143:1143` — IMAP (mail client access)
- `-p 1110:1110` — POP3 (mail client access)
- `-v sandesh-data:/data` — stores mail in a named volume so it survives container restarts

---

## Step 3 — Confirm It Is Running

```bash
# Check the container is up
docker ps | grep sandesh

# Health check
curl http://localhost:1080/api/v1/health
# Expected: {"status":"ok","version":"0.1.0"}
```

---

## Step 4 — Open the Web UI

Open your browser: **http://localhost:1080**

You will see the Sandesh developer console. It is empty — no mail yet. Leave it open; new mail will appear here instantly without a page refresh.

---

## Step 5 — Send a Test Email

Point your application's SMTP client at **`localhost:1025`** (no TLS, no authentication needed).

### Quick test via Python:
```python
import smtplib
from email.message import EmailMessage

msg = EmailMessage()
msg["From"]    = "myapp@example.com"
msg["To"]      = "developer@example.com"
msg["Subject"] = "Sandesh Docker Test"
msg.set_content("Hello! Sandesh received this message.")

with smtplib.SMTP("localhost", 1025) as s:
    s.send_message(msg)
    print("Sent!")
```

### Quick test via `curl`:
```bash
curl --url 'smtp://localhost:1025' \
  --mail-from 'myapp@example.com' \
  --mail-rcpt 'developer@example.com' \
  --upload-file - <<'EOF'
From: myapp@example.com
To: developer@example.com
Subject: Test from curl

Hello from curl!
EOF
```

---

## Step 6 — Inspect the Message in the Web UI

Switch to your browser. The email you just sent should appear immediately in the message list. Click on it to see:
- **Headers** — full RFC header view
- **HTML / Plain text body** — sandboxed HTML rendering
- **MIME structure** — part tree with byte ranges
- **Delivery transcript** — the exact SMTP conversation Sandesh had with your app

---

## Step 7 — Read Mail via IMAP

Connect any IMAP client (Thunderbird, Apple Mail, Outlook, `mutt`, etc.):

| Setting | Value |
| :--- | :--- |
| Server | `localhost` |
| Port | `1143` |
| Security | `None` (or STARTTLS) |
| Username | anything — e.g. `developer@example.com` |
| Password | anything — e.g. `test` |

Or test with Python:
```python
import imaplib

mail = imaplib.IMAP4("localhost", 1143)
mail.login("developer@example.com", "test")  # any credentials work
mail.select("INBOX")

_, ids = mail.search(None, "ALL")
for num in ids[0].split():
    _, data = mail.fetch(num, "(RFC822)")
    print(data[0][1].decode()[:500])  # first 500 chars

mail.logout()
```

---

## Step 8 — Read Mail via POP3

```python
import poplib

pop = poplib.POP3("localhost", 1110)
pop.user("developer@example.com")  # any credentials
pop.pass_("test")

count, size = pop.stat()
print(f"{count} messages, {size} bytes")

if count:
    _, lines, _ = pop.retr(1)
    print("\n".join(l.decode() for l in lines[:15]))

pop.quit()
```

---

## Step 9 — Query via REST API

```bash
# List all received messages
curl -s http://localhost:1080/api/v1/messages | jq .

# Download the raw MIME bytes of a specific message
curl -s http://localhost:1080/api/v1/messages/<ID>/raw
```

---

## Useful Commands

```bash
# View live server logs
docker logs -f sandesh

# Stop the container
docker stop sandesh

# Restart (mail persists in the named volume)
docker start sandesh

# Remove container but keep mail
docker rm sandesh

# Remove everything including mail
docker rm sandesh && docker volume rm sandesh-data
```

---

## Sharing the Image with a Coworker

```bash
# Save (use --format docker-archive if using Podman, for Docker compatibility)
podman save --format docker-archive -o sandesh.tar sandesh:latest
# or with Docker:
docker save -o sandesh.tar sandesh:latest

# Share the sandesh.tar file (e.g. via USB, file share, S3)
# Your coworker then runs:
docker load -i sandesh.tar
docker run -d -p 1025:1025 -p 1080:1080 -p 1143:1143 -p 1110:1110 \
  -v sandesh-data:/data sandesh:latest
```

---

## Troubleshooting

| Problem | Fix |
| :--- | :--- |
| Port already in use | Change host port: `-p 2025:1025` then send to `localhost:2025` |
| Container exits immediately | Run `docker logs sandesh` to see the error |
| Web UI not loading | Confirm `-p 1080:1080` is present in `docker run` command |
| IMAP/POP3 connection refused | Confirm `-p 1143:1143` and `-p 1110:1110` are in `docker run` |
| Mail disappeared after restart | Make sure you used `-v sandesh-data:/data` |
