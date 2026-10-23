# Sandesh on Debian / Ubuntu — `.deb` Package

**What you need**: A `.deb` file — e.g. `sandesh_0.1.0_amd64.deb`

**Prerequisites**: A Debian- or Ubuntu-based system with `apt` or `dpkg`. No other dependencies — the binary is statically linked and installs with zero library requirements.

> **This is the headless server package** — it installs a background service.  
> If you want a desktop GUI with a tray icon, see [07-DESKTOP.md](07-DESKTOP.md) instead.

---

## Step 1 — Install the Package

### Recommended (via `apt`):
```bash
sudo apt install ./sandesh_0.1.0_amd64.deb
```

`apt` confirms there are no unresolvable dependencies (there are none — the package is self-contained) and runs the maintainer scripts to set up the service user and directory.

### Alternative (via `dpkg`, works offline):
```bash
sudo dpkg -i sandesh_0.1.0_amd64.deb
```

---

## Step 2 — What Gets Installed

| Path | Contents |
| :--- | :--- |
| `/usr/bin/sandesh` | The server binary |
| `/lib/systemd/system/sandesh.service` | Systemd service unit |
| `/etc/sandesh/sandesh.env` | Configuration file (env vars, mode 640) |
| `/var/lib/sandesh/` | Mail storage directory (owned by `sandesh:sandesh`, mode 750) |

A dedicated system user `sandesh` is created automatically. The service **does not start automatically on install** — you start it when you're ready.

---

## Step 3 — Start the Service

```bash
# Enable on boot and start immediately
sudo systemctl enable --now sandesh

# Or: start without enabling on boot
sudo systemctl start sandesh
```

Check it's running:
```bash
sudo systemctl status sandesh
# You should see: Active: active (running) since ...
```

---

## Step 4 — Open the Web UI

Open your browser: **http://localhost:8080**

---

## Step 5 — Configure Your Application

Point your application's SMTP settings at:

| Setting | Value |
| :--- | :--- |
| SMTP Host | `localhost` |
| SMTP Port | `1025` |
| Security | None |
| Authentication | None |

> The service binds to `127.0.0.1` by default — it is only reachable from the same machine. See [customisation](#customising-configuration) to listen on a network interface.

---

## Step 6 — Send a Test Email

### Via Python:
```python
import smtplib
from email.message import EmailMessage

msg = EmailMessage()
msg["From"]    = "myapp@example.com"
msg["To"]      = "you@example.com"
msg["Subject"] = "Sandesh .deb Test"
msg.set_content("Hello from the installed Sandesh package!")

with smtplib.SMTP("localhost", 1025) as s:
    s.send_message(msg)
    print("Sent!")
```

### Via `curl`:
```bash
curl --url 'smtp://localhost:1025' \
  --mail-from 'myapp@example.com' \
  --mail-rcpt 'you@example.com' \
  --upload-file - <<'EOF'
From: myapp@example.com
To: you@example.com
Subject: Test via curl to Sandesh service

Hello from Debian/Ubuntu!
EOF
```

---

## Step 7 — Verify Delivery

**In the Web UI**: Open **http://localhost:8080** — the message appears instantly with full header inspection, body rendering, MIME tree, and SMTP transcript.

**Via REST API**:
```bash
# Check service health
curl http://localhost:8080/api/v1/health

# Verify message receipt
curl -s http://localhost:8080/api/v1/messages | jq .items[0].subject
```

**Via IMAP** (connect any email client or use Python):
```python
import imaplib

m = imaplib.IMAP4("localhost", 1143)
m.login("you@example.com", "test")   # any credentials work
m.select("INBOX")
_, ids = m.search(None, "ALL")
print("Messages:", ids[0].split())
m.logout()
```

**Via POP3**:
```python
import poplib

pop = poplib.POP3("localhost", 1110)
pop.user("you@example.com")
pop.pass_("test")
count, size = pop.stat()
print(f"{count} message(s), {size} bytes")
pop.quit()
```

---

## Customising Configuration

Edit the configuration file (requires root):
```bash
sudo nano /etc/sandesh/sandesh.env
```

Available options:
```bash
# Listen on all interfaces (useful if other machines on your LAN need to send mail)
SANDESH_SMTP_ADDR=0.0.0.0:1025
SANDESH_HTTP_ADDR=0.0.0.0:8080
SANDESH_IMAP_ADDR=0.0.0.0:1143
SANDESH_POP3_ADDR=0.0.0.0:1110

# Custom ports (if 1025/8080 conflict with something)
SANDESH_SMTP_ADDR=127.0.0.1:2025
SANDESH_HTTP_ADDR=127.0.0.1:9080

# Log verbosity
SANDESH_LOG=debug
```

After editing, restart the service:
```bash
sudo systemctl restart sandesh
```

---

## Managing the Service

```bash
# View live logs
sudo journalctl -u sandesh -f

# Stop the service
sudo systemctl stop sandesh

# Disable autostart on boot
sudo systemctl disable sandesh

# Check listening ports
ss -tlnp | grep sandesh
```

---

## Uninstalling

```bash
# Remove Sandesh but KEEP your mail data in /var/lib/sandesh
sudo apt remove sandesh

# Remove everything INCLUDING your mail data
sudo apt purge sandesh
```

> `apt remove` (or `apt purge --dry-run`) will not touch `/var/lib/sandesh` unless you use `purge`. Your mail is safe on a normal uninstall.

---

## Troubleshooting

| Problem | Fix |
| :--- | :--- |
| `systemctl status sandesh` shows failed | Run `sudo journalctl -u sandesh -n 50` to see the error |
| Port 1025 in use | Edit `/etc/sandesh/sandesh.env` and set `SANDESH_SMTP_ADDR=127.0.0.1:2025` |
| Web UI not reachable from another machine | Set `SANDESH_HTTP_ADDR=0.0.0.0:8080` in `/etc/sandesh/sandesh.env` |
| Permission denied to `/var/lib/sandesh` | The service user `sandesh` owns this — run as the service, not as root |
