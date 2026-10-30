# Sandesh on Linux — Static Binary

**What you need**: A `.tar.gz` archive, for example:
- `sandesh-0.2.0-x86_64-unknown-linux-musl.tar.gz` (most Linux desktops/servers)
- `sandesh-0.2.0-aarch64-unknown-linux-musl.tar.gz` (ARM64, e.g. Raspberry Pi, AWS Graviton)

**Prerequisites**: None. This binary is fully self-contained — no runtime, no libraries, no `apt install` required. It runs on Ubuntu, Debian, Fedora, Alpine, Arch, CentOS, RHEL, and any other Linux distro.

---

## Step 1 — Verify & Extract the Archive

```bash
# Verify the checksum (the .sha256 file must be in the same folder)
sha256sum -c sandesh-0.2.0-x86_64-unknown-linux-musl.tar.gz.sha256

# If verification passes:
# sandesh-0.2.0-x86_64-unknown-linux-musl.tar.gz: OK

# Extract
tar -xzf sandesh-0.2.0-x86_64-unknown-linux-musl.tar.gz

# Confirm the binary is there
ls -lh sandesh
```

> If you don't have the `.sha256` file, skip the verify step and go straight to extracting.

---

## Step 2 — Run Sandesh

```bash
./sandesh
```

You should see output like:
```
Sandesh 0.2.0 starting...
SMTP:      127.0.0.1:1025
IMAP:      127.0.0.1:1143
POP3:      127.0.0.1:1110
HTTP:      http://127.0.0.1:8080/api/v1
WebSocket: ws://127.0.0.1:8080/api/v1/events
Database:  sandesh.db
Blobs:     sandesh-blobs
Auth:      disabled · TLS: disabled
```

Sandesh is now running. Press **Ctrl+C** to stop it when you're done.

---

## Step 3 — Open the Web UI

Open your browser: **http://localhost:8080**

The mail console will be empty. Keep it open — new mail appears here automatically without refreshing.

---

## Step 4 — Configure Your Application

Point your application's outgoing mail / SMTP settings at:

| Setting | Value |
| :--- | :--- |
| SMTP Host | `127.0.0.1` or `localhost` |
| SMTP Port | `1025` |
| Security | None (no TLS) |
| Authentication | None (leave username/password blank) |

---

## Step 5 — Send a Test Email

### Python (`smtplib`):
```python
import smtplib
from email.message import EmailMessage

msg = EmailMessage()
msg["From"]    = "myapp@example.com"
msg["To"]      = "you@example.com"
msg["Subject"] = "Sandesh Linux Binary Test"
msg.set_content("Hello! If you see this in Sandesh, everything works.")

with smtplib.SMTP("127.0.0.1", 1025) as s:
    s.send_message(msg)
    print("Message sent!")
```

### `curl`:
```bash
curl --url 'smtp://127.0.0.1:1025' \
  --mail-from 'myapp@example.com' \
  --mail-rcpt 'you@example.com' \
  --upload-file - <<'EOF'
From: myapp@example.com
To: you@example.com
Subject: Hello from curl

Quick test!
EOF
```

### Swaks (Swiss Army Knife for SMTP):
```bash
swaks --to you@example.com --from myapp@example.com \
      --server 127.0.0.1 --port 1025 \
      --body "Hello from swaks!"
```

---

## Step 6 — Confirm Delivery via Web UI

The email you just sent appears immediately at **http://localhost:8080**. Click the message to see:
- Full headers and routing details
- Plain text and rendered HTML (isolated, no external resources fetched)
- MIME part tree
- The SMTP protocol conversation your app had with Sandesh

---

## Step 7 — Read via IMAP

Connect any email client (Thunderbird, Mutt, Alpine, Neomutt, etc.) to IMAP:

| Setting | Value |
| :--- | :--- |
| Server | `localhost` |
| Port | `1143` |
| Security | None |
| Username | anything (e.g. `you@example.com`) |
| Password | anything (e.g. `test`) |

Or test with Python:
```python
import imaplib

m = imaplib.IMAP4("localhost", 1143)
m.login("you@example.com", "test")
m.select("INBOX")

_, ids = m.search(None, "ALL")
for num in ids[0].split():
    _, data = m.fetch(num, "(RFC822)")
    print(data[0][1].decode()[:600])

m.logout()
```

---

## Step 8 — Read via POP3

```python
import poplib

pop = poplib.POP3("localhost", 1110)
pop.user("you@example.com")
pop.pass_("test")

count, _ = pop.stat()
print(f"{count} messages in inbox")
if count:
    _, lines, _ = pop.retr(1)
    print("\n".join(l.decode() for l in lines[:15]))

pop.quit()
```

---

## Step 9 — REST API Quick Checks

```bash
# Health
curl http://localhost:8080/api/v1/health

# List messages
curl -s http://localhost:8080/api/v1/messages | jq .

# Get a message's raw MIME bytes
curl http://localhost:8080/api/v1/messages/<ID>/raw
```

---

## Running as a Background Service (Optional)

```bash
# Keep Sandesh running in the background, logs to a file
SANDESH_DATA_DIR=~/.sandesh ./sandesh >> ~/.sandesh/sandesh.log 2>&1 &

# Stop it
pkill sandesh
```

Or use `tmux` / `screen` to keep it alive across SSH sessions:
```bash
tmux new-session -d -s sandesh './sandesh'
# Later: tmux attach -t sandesh
```

---

## Customising Ports (Environment Variables)

Sandesh is configured entirely via environment variables — no config file needed:

```bash
# Custom ports example
SANDESH_SMTP_ADDR=127.0.0.1:2025 \
SANDESH_HTTP_ADDR=127.0.0.1:9080 \
SANDESH_IMAP_ADDR=127.0.0.1:2143 \
SANDESH_POP3_ADDR=127.0.0.1:2110 \
SANDESH_DATA_DIR=/home/user/maildata \
./sandesh
```

| Variable | Default | Purpose |
| :--- | :--- | :--- |
| `SANDESH_SMTP_ADDR` | `127.0.0.1:1025` | SMTP listener |
| `SANDESH_HTTP_ADDR` | `127.0.0.1:8080` | Web UI and REST API |
| `SANDESH_IMAP_ADDR` | `127.0.0.1:1143` | IMAP listener |
| `SANDESH_POP3_ADDR` | `127.0.0.1:1110` | POP3 listener |
| `SANDESH_DATA_DIR` | `.` (working directory) | Where to store the database and blobs |
| `SANDESH_LOG` | `info` | Log verbosity (`trace`, `debug`, `info`, `warn`, `error`) |

---

## Troubleshooting

| Problem | Fix |
| :--- | :--- |
| `Permission denied: ./sandesh` | Run `chmod +x sandesh` |
| Port already in use | Change the port via `SANDESH_SMTP_ADDR=127.0.0.1:2025 ./sandesh` |
| Web UI not loading | Make sure no firewall is blocking `8080`; check logs in the terminal |
| `Exec format error` | You downloaded the wrong architecture binary (e.g. ARM binary on x86) |
