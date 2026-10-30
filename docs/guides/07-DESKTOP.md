# Sandesh Desktop App — Tray Icon & Native Notifications

**What you need**: One of:
- `sandesh-desktop_0.2.0_amd64.deb` — for Debian/Ubuntu with a desktop environment
- `Sandesh-Desktop_0.2.0_amd64.AppImage` — portable, runs on any Linux distro with a desktop

**Prerequisites**: A Linux desktop environment (GNOME, KDE, XFCE, etc.) with GTK 3 support. This is **not** the headless server — it is a GUI application that runs in your system tray.

> **Not on the release page yet.** Releases publish the headless server, not these desktop
> bundles. Build them from a checkout (needs the Tauri CLI plus GTK and WebKit development
> packages):
>
> ```bash
> (cd web && npm ci && npm run build)
> cargo install tauri-cli --locked
> cd crates/sandesh-desktop && cargo tauri build
> ```

---

## Step 1 — Install

### Option A — AppImage (any distro, no install needed)
```bash
# Make the AppImage executable
chmod +x Sandesh-Desktop_0.2.0_amd64.AppImage

# Run it
./Sandesh-Desktop_0.2.0_amd64.AppImage
```

> AppImages are self-contained. They do not install anything into your system. Just download, make executable, and run.

### Option B — `.deb` Package (Debian / Ubuntu desktop)
```bash
sudo apt install ./sandesh-desktop_0.2.0_amd64.deb
```

After install, find **Sandesh** in your application launcher or run `sandesh-desktop` from the terminal.

---

## Step 2 — Launch and Locate the Tray Icon

When Sandesh Desktop starts:
1. The main window opens briefly to confirm it's running.
2. A **tray icon** appears in your system notification area (top-right on GNOME, bottom-right on KDE/XFCE).

**Click the tray icon** to see options:
- **Open Sandesh** — opens the mail console in your browser
- **Quit** — stops Sandesh and exits

> If the tray icon is hidden, look in your notification tray extension or panel settings.

---

## Step 3 — Open the Web UI

Either click **Open Sandesh** in the tray menu, or open your browser directly:

**http://localhost:8080**

---

## Step 4 — Configure Your Application

Point your app's SMTP settings at:

| Setting | Value |
| :--- | :--- |
| SMTP Host | `localhost` |
| SMTP Port | `1025` |
| Security | None |
| Authentication | None |

---

## Step 5 — Send a Test Email

```python
import smtplib
from email.message import EmailMessage

msg = EmailMessage()
msg["From"]    = "myapp@example.com"
msg["To"]      = "you@example.com"
msg["Subject"] = "Sandesh Desktop Test"
msg.set_content("Hello! This arrived via the Sandesh desktop app.")

with smtplib.SMTP("localhost", 1025) as s:
    s.send_message(msg)
    print("Sent!")
```

---

## Step 6 — Desktop Notification

Immediately after the email is sent, Sandesh fires a **native OS desktop notification**:

> 📬 **New message** — *"Sandesh Desktop Test"* from `myapp@example.com`

Click the notification to jump directly to that message in the browser console.

---

## Step 7 — Inspect the Message

In the Web UI at **http://localhost:8080**:
- Click the message in the list to open it.
- Switch between **Headers**, **Body**, and **Raw MIME** views.
- Use the **Compose** button to write a reply.

---

## Step 8 — Read via IMAP and POP3

The desktop app exposes the same IMAP and POP3 servers as the headless binary.

Configure any email client (Thunderbird, Evolution, Geary, Mutt):

| Setting | Value |
| :--- | :--- |
| IMAP Server | `localhost` |
| IMAP Port | `1143` |
| POP3 Server | `localhost` |
| POP3 Port | `1110` |
| Security | None (or STARTTLS) |
| Username | anything (e.g. `you@example.com`) |
| Password | anything (e.g. `test`) |

### Test IMAP via Python:
```python
import imaplib

m = imaplib.IMAP4("localhost", 1143)
m.login("you@example.com", "test")
m.select("INBOX")
_, ids = m.search(None, "ALL")
print(f"IMAP messages found: {len(ids[0].split())}")
m.logout()
```

### Test POP3 via Python:
```python
import poplib

pop = poplib.POP3("localhost", 1110)
pop.user("you@example.com")
pop.pass_("test")
count, size = pop.stat()
print(f"POP3 inbox: {count} messages, {size} bytes")
pop.quit()
```

---

## Step 9 — REST API Verification

The embedded server provides the full REST API at `http://localhost:8080/api/v1`:

```bash
# Health check
curl http://localhost:8080/api/v1/health
# {"status":"ok","version":"0.2.0"}

# List messages
curl -s http://localhost:8080/api/v1/messages | jq .
```

---

## Auto-Starting with Your Desktop Session

### GNOME / systemd user service:
```bash
mkdir -p ~/.config/systemd/user
cat > ~/.config/systemd/user/sandesh-desktop.service <<'EOF'
[Unit]
Description=Sandesh Desktop Mail Environment
After=graphical-session.target

[Service]
ExecStart=/usr/bin/sandesh-desktop
Restart=on-failure
Environment=DISPLAY=:0

[Install]
WantedBy=graphical-session.target
EOF

systemctl --user enable --now sandesh-desktop
```

### XDG Autostart (GNOME, KDE, XFCE — any desktop):
```bash
mkdir -p ~/.config/autostart
cat > ~/.config/autostart/sandesh-desktop.desktop <<'EOF'
[Desktop Entry]
Type=Application
Name=Sandesh
Comment=Local mail development environment
Exec=sandesh-desktop
Icon=sandesh
Hidden=false
NoDisplay=false
X-GNOME-Autostart-enabled=true
EOF
```

---

## Troubleshooting

| Problem | Fix |
| :--- | :--- |
| Tray icon not visible | Check your desktop's notification area settings; on GNOME, install "AppIndicator" extension |
| Desktop notifications not showing | Ensure `notify-osd` or `libnotify` is installed: `sudo apt install libnotify-bin` |
| AppImage won't run | Ensure FUSE is available: `sudo apt install fuse libfuse2` (needed on some distros) |
| Port 1025 in use | Set `SANDESH_SMTP_ADDR=127.0.0.1:2025` before launching |

---

## Differences vs the Headless Server Package

| Feature | Desktop App (`sandesh-desktop`) | Headless Server (`.deb`/`.rpm`) |
| :--- | :--- | :--- |
| GUI tray icon | ✅ Yes | ❌ No |
| Native OS notifications | ✅ Yes | ❌ No |
| Runs as a systemd service | ❌ No (runs as your user) | ✅ Yes |
| Requires desktop environment | ✅ Yes (GTK 3 + WebKit) | ❌ No |
| Best for | Developer laptops / workstations | Servers, CI, headless VMs |
