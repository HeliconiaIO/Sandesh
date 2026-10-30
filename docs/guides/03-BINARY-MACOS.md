# Sandesh on macOS — Static Binary

**What you need**: A `.tar.gz` archive, for example:
- `sandesh-0.2.0-aarch64-apple-darwin.tar.gz` — Apple Silicon (M1, M2, M3, M4)
- `sandesh-0.2.0-x86_64-apple-darwin.tar.gz` — Intel Mac

**Prerequisites**: Nothing extra. Sandesh is a self-contained binary with no dependencies. Runs on macOS 12 Monterey and later.

---

## Step 1 — Find Your Architecture

```bash
uname -m
# arm64  → use aarch64-apple-darwin
# x86_64 → use x86_64-apple-darwin
```

---

## Step 2 — Verify & Extract

```bash
# Verify checksum (optional but recommended)
shasum -a 256 -c sandesh-0.2.0-aarch64-apple-darwin.tar.gz.sha256

# Extract
tar -xzf sandesh-0.2.0-aarch64-apple-darwin.tar.gz

# Make executable (should already be set, but just in case)
chmod +x sandesh
```

---

## Step 3 — Allow macOS Gatekeeper to Run It

The first time you run a downloaded binary, macOS will block it with a Gatekeeper warning.

**Method A — Right-click to open (recommended):**
1. In Finder, navigate to the `sandesh` binary.
2. **Right-click → Open** (not double-click).
3. A dialog asks "Are you sure you want to open it?" — click **Open**.

**Method B — Remove the quarantine attribute:**
```bash
xattr -dr com.apple.quarantine ./sandesh
```

---

## Step 4 — Run Sandesh

```bash
./sandesh
```

Expected output:
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

---

## Step 5 — Open the Web UI

Open your browser: **http://localhost:8080**

The console is empty until you send a message. Keep it open — new mail appears without refreshing.

---

## Step 6 — Configure Your Application

Point your application's SMTP settings at:

| Setting | Value |
| :--- | :--- |
| SMTP Host | `localhost` |
| SMTP Port | `1025` |
| Security | None |
| Authentication | None |

---

## Step 7 — Send a Test Email

### Python (built into macOS):
```python
import smtplib
from email.message import EmailMessage

msg = EmailMessage()
msg["From"]    = "myapp@example.com"
msg["To"]      = "you@example.com"
msg["Subject"] = "Sandesh macOS Test"
msg.set_content("Hello from macOS!")

with smtplib.SMTP("localhost", 1025) as s:
    s.send_message(msg)
    print("Sent!")
```

### `curl` (built into macOS):
```bash
curl --url 'smtp://localhost:1025' \
  --mail-from 'myapp@example.com' \
  --mail-rcpt 'you@example.com' \
  --upload-file - <<'EOF'
From: myapp@example.com
To: you@example.com
Subject: Test from macOS curl

Hello!
EOF
```

---

## Step 8 — Confirm Delivery in the Web UI

Switch back to your browser at **http://localhost:8080**. The email you sent will appear instantly in the message list.

Click on the message to inspect:
- **Headers** — full RFC 5322 header view and hop timeline
- **HTML / Plain text body** — sandboxed HTML rendering (no tracking pixels fetched)
- **MIME structure** — interactive part tree with byte sizes
- **Delivery transcript** — the raw SMTP protocol dialogue between your app and Sandesh

---

## Step 9 — Read via IMAP

Connect Apple Mail, Thunderbird, or any IMAP client:

| Setting | Value |
| :--- | :--- |
| IMAP Server | `localhost` |
| Port | `1143` |
| Connection Security | None (or STARTTLS) |
| Authentication | `None` / Normal Password |
| Username | any value (e.g. `you@example.com`) |
| Password | any value (e.g. `test`) |

> **Apple Mail note**: Apple Mail may display a warning about an unencrypted connection. Choose "Use without SSL" or allow insecure authentication.

### Test IMAP via Python:
```python
import imaplib

m = imaplib.IMAP4("localhost", 1143)
m.login("you@example.com", "test")
m.select("INBOX")

_, ids = m.search(None, "ALL")
for num in ids[0].split():
    _, data = m.fetch(num, "(RFC822)")
    print(data[0][1].decode()[:500])

m.logout()
```

---

## Step 10 — Read via POP3

You can also retrieve messages using POP3:

```python
import poplib

pop = poplib.POP3("localhost", 1110)
pop.user("you@example.com")
pop.pass_("test")

count, size = pop.stat()
print(f"{count} messages, {size} bytes in inbox")

if count:
    _, lines, _ = pop.retr(1)
    print("\n".join(l.decode() for l in lines[:15]))

pop.quit()
```

---

## Step 11 — REST API Quick Checks

Sandesh provides a full REST API for automation and test assertions:

```bash
# Health check
curl http://localhost:8080/api/v1/health
# {"status":"ok","version":"0.2.0"}

# List received messages
curl -s http://localhost:8080/api/v1/messages | jq .

# Download the raw RFC 822 MIME bytes
curl -s http://localhost:8080/api/v1/messages/<ID>/raw
```

---

## Customising Ports (Environment Variables)

To change listening ports or data storage location on macOS, pass environment variables when starting Sandesh:

```bash
SANDESH_SMTP_ADDR=127.0.0.1:2025 \
SANDESH_HTTP_ADDR=127.0.0.1:9080 \
SANDESH_IMAP_ADDR=127.0.0.1:2143 \
SANDESH_POP3_ADDR=127.0.0.1:2110 \
SANDESH_DATA_DIR=~/.sandesh-data \
./sandesh
```

| Variable | Default | Purpose |
| :--- | :--- | :--- |
| `SANDESH_SMTP_ADDR` | `127.0.0.1:1025` | SMTP listener |
| `SANDESH_HTTP_ADDR` | `127.0.0.1:8080` | Web UI and REST API |
| `SANDESH_IMAP_ADDR` | `127.0.0.1:1143` | IMAP listener |
| `SANDESH_POP3_ADDR` | `127.0.0.1:1110` | POP3 listener |
| `SANDESH_DATA_DIR` | `.` (current folder) | SQLite database and blob storage directory |

---

## Running as a Background Service (launchd)

To keep Sandesh running in the background across reboots and terminal sessions, create a launchd agent:

```bash
mkdir -p ~/Library/LaunchAgents
cat > ~/Library/LaunchAgents/com.sandesh.plist <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.sandesh</string>
  <key>ProgramArguments</key>
  <array>
    <string>/Users/YOUR_USERNAME/sandesh</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>SANDESH_DATA_DIR</key>
    <string>/Users/YOUR_USERNAME/.sandesh-data</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>/Users/YOUR_USERNAME/.sandesh-data/sandesh.log</string>
  <key>StandardErrorPath</key>
  <string>/Users/YOUR_USERNAME/.sandesh-data/sandesh.log</string>
</dict>
</plist>
EOF

# Replace YOUR_USERNAME with your actual macOS username
sed -i '' "s/YOUR_USERNAME/$(whoami)/g" ~/Library/LaunchAgents/com.sandesh.plist
mkdir -p ~/.sandesh-data

# Load and start the service
launchctl load ~/Library/LaunchAgents/com.sandesh.plist

# To stop and unload
launchctl unload ~/Library/LaunchAgents/com.sandesh.plist
```

---

## Troubleshooting

| Problem | Fix |
| :--- | :--- |
| `"sandesh" can't be opened because...` | Use Right-click → Open, or run `xattr -dr com.apple.quarantine ./sandesh` |
| Port 1025 in use | Set `SANDESH_SMTP_ADDR=127.0.0.1:2025 ./sandesh` |
| `Exec format error` | You downloaded the wrong binary (arm64 vs x86_64); check `uname -m` |
| Apple Mail won't connect to IMAP | Accept the "insecure connection" dialog or use Thunderbird instead |
| Web UI not loading | Ensure terminal running Sandesh is active, or verify launchd service status |

