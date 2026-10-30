# Sandesh on RHEL / Fedora / CentOS / Rocky Linux — `.rpm` Package

**What you need**: A `.rpm` file — e.g. `sandesh-0.2.0-1.fc41.x86_64.rpm`

**Prerequisites**: A Fedora, RHEL, CentOS, Rocky Linux, or AlmaLinux system with `dnf` or `rpm`. No other dependencies — the binary is statically linked and installs with zero library requirements.

> **This is the headless server package** — it installs a background service.  
> If you want a desktop GUI with a tray icon, see [07-DESKTOP.md](07-DESKTOP.md) instead.

---

## Step 1 — Install the Package

### Recommended (via `dnf`):
```bash
sudo dnf install ./sandesh-0.2.0-1.fc41.x86_64.rpm
```

`dnf` verifies there are no unresolved dependencies and runs the pre/post install scripts.

### Alternative (via `rpm`, works offline):
```bash
sudo rpm -ivh sandesh-0.2.0-1.fc41.x86_64.rpm
```

---

## Step 2 — What Gets Installed

| Path | Contents |
| :--- | :--- |
| `/usr/bin/sandesh` | The server binary |
| `/lib/systemd/system/sandesh.service` | Systemd service unit |
| `/etc/sandesh/sandesh.env` | Configuration file (env vars, mode 640) |
| `/var/lib/sandesh/` | Mail storage directory (owned by `sandesh:sandesh`, mode 750) |

A dedicated system user `sandesh` is created automatically. The service **does not start automatically on install**.

---

## Step 3 — Start the Service

```bash
# Enable on boot and start immediately
sudo systemctl enable --now sandesh

# Or: start now without enabling autostart
sudo systemctl start sandesh
```

Verify it's running:
```bash
sudo systemctl status sandesh
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

---

## Step 6 — Send a Test Email

### Via Python:
```python
import smtplib
from email.message import EmailMessage

msg = EmailMessage()
msg["From"]    = "myapp@example.com"
msg["To"]      = "you@example.com"
msg["Subject"] = "Sandesh RPM Test"
msg.set_content("Hello from the installed Sandesh RPM package!")

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
Subject: Test via curl to Sandesh RPM service

Hello from Fedora/RHEL!
EOF
```

---

## Step 7 — Verify Delivery

**In the Web UI**: Open **http://localhost:8080** — the message appears instantly with full header inspection, body rendering, MIME tree, and SMTP transcript.

**Via REST API**:
```bash
# Health check
curl http://localhost:8080/api/v1/health

# Message list
curl -s http://localhost:8080/api/v1/messages | python3 -m json.tool
```

**Via IMAP**:
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
# or: sudo vi /etc/sandesh/sandesh.env
```

Available options:
```bash
# Listen on all interfaces so other machines on your LAN can send mail
SANDESH_SMTP_ADDR=0.0.0.0:1025
SANDESH_HTTP_ADDR=0.0.0.0:8080
SANDESH_IMAP_ADDR=0.0.0.0:1143
SANDESH_POP3_ADDR=0.0.0.0:1110

# Custom ports (to avoid conflicts)
SANDESH_SMTP_ADDR=127.0.0.1:2025
SANDESH_HTTP_ADDR=127.0.0.1:9080

# Log verbosity
SANDESH_LOG=debug
```

After editing, restart the service:
```bash
sudo systemctl restart sandesh
```

> **Config file safety**: If you uninstall or upgrade Sandesh, your customised `sandesh.env` is preserved as `/etc/sandesh/sandesh.env.rpmsave` — it is never silently overwritten.

---

## Managing the Service

```bash
# View live logs
sudo journalctl -u sandesh -f

# Stop
sudo systemctl stop sandesh

# Disable autostart
sudo systemctl disable sandesh

# Check which ports are open
ss -tlnp | grep sandesh
```

---

## SELinux Note (RHEL / Rocky / AlmaLinux)

If you have SELinux enforcing and Sandesh fails to bind to non-standard ports, add a policy:
```bash
sudo semanage port -a -t http_port_t -p tcp 1025
sudo semanage port -a -t http_port_t -p tcp 8080
# Restart after policy change
sudo systemctl restart sandesh
```

---

## Upgrading

```bash
sudo dnf upgrade ./sandesh-0.2.0-1.fc41.x86_64.rpm
# Your mail data and edited config are preserved during upgrade
```

---

## Uninstalling

```bash
# Remove Sandesh but KEEP your mail in /var/lib/sandesh
sudo dnf remove sandesh

# Confirm mail is still there
ls /var/lib/sandesh/
```

---

## Troubleshooting

| Problem | Fix |
| :--- | :--- |
| `systemctl status sandesh` shows failed | Run `sudo journalctl -u sandesh -n 50` to see the error |
| Port 1025 refused by SELinux | Add SELinux policy (see SELinux note above) |
| Port 1025 in use | Edit `/etc/sandesh/sandesh.env`: `SANDESH_SMTP_ADDR=127.0.0.1:2025` |
| Web UI not reachable from another machine | Set `SANDESH_HTTP_ADDR=0.0.0.0:8080` and check firewall with `firewall-cmd` |
| Firewall blocking ports | `sudo firewall-cmd --add-port=1025/tcp --permanent && sudo firewall-cmd --reload` |
