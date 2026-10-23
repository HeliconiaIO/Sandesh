# Sandesh on Windows — Static Binary

**What you need**: A `.zip` file — `sandesh-0.1.0-x86_64-pc-windows-msvc.zip`

**Prerequisites**: Nothing extra — Sandesh is a standalone `.exe`. Works on Windows 10 and Windows 11 (64-bit).

---

## Step 1 — Extract the Zip

1. Right-click `sandesh-0.1.0-x86_64-pc-windows-msvc.zip`
2. Select **Extract All...**
3. Choose a folder, e.g. `C:\sandesh\`

You should now have `C:\sandesh\sandesh.exe`.

---

## Step 2 — Allow Windows Defender / SmartScreen

The first time you run a downloaded executable, Windows SmartScreen may show a warning.

1. Double-click `sandesh.exe` — a blue "Windows protected your PC" dialog appears.
2. Click **More info** → then **Run anyway**.

Or from PowerShell, unblock it permanently:
```powershell
Unblock-File -Path C:\sandesh\sandesh.exe
```

---

## Step 3 — Run Sandesh

Open **PowerShell** or **Command Prompt**, navigate to the folder, and run:

```powershell
cd C:\sandesh
.\sandesh.exe
```

Expected output:
```
Sandesh 0.1.0 starting...
SMTP:      127.0.0.1:1025
IMAP:      127.0.0.1:1143
POP3:      127.0.0.1:1110
HTTP:      http://127.0.0.1:8080/api/v1
WebSocket: ws://127.0.0.1:8080/api/v1/events
Database:  sandesh.db
Blobs:     sandesh-blobs
Auth:      disabled · TLS: disabled
```

Keep this window open — closing it stops Sandesh.

---

## Step 4 — Open the Web UI

Open your browser: **http://localhost:8080**

The console is empty until you send a message.

---

## Step 5 — Configure Your Application

Point your application's SMTP settings at:

| Setting | Value |
| :--- | :--- |
| SMTP Host | `localhost` or `127.0.0.1` |
| SMTP Port | `1025` |
| Security | None |
| Authentication | None |

---

## Step 6 — Send a Test Email

### Python (if installed):
```python
import smtplib
from email.message import EmailMessage

msg = EmailMessage()
msg["From"]    = "myapp@example.com"
msg["To"]      = "you@example.com"
msg["Subject"] = "Sandesh Windows Test"
msg.set_content("Hello from Windows!")

with smtplib.SMTP("localhost", 1025) as s:
    s.send_message(msg)
    print("Sent!")
```

### PowerShell:
```powershell
$smtp = New-Object System.Net.Mail.SmtpClient("localhost", 1025)
$smtp.EnableSsl = $false
$smtp.Send("myapp@example.com", "you@example.com", "Test from PowerShell", "Hello!")
Write-Host "Sent!"
```

---

## Step 7 — Confirm Delivery in the Web UI

Open your browser at **http://localhost:8080**. The email you just sent will appear immediately in the message list.

Click on the message to inspect:
- **Headers** — complete RFC 5322 header view and hop timeline
- **HTML / Plain text body** — safely rendered in an isolated sandbox
- **MIME structure** — interactive part tree with byte sizes
- **Delivery transcript** — the raw SMTP protocol dialogue

---

## Step 8 — Read via IMAP

Configure Thunderbird, Outlook, or any IMAP client:

| Setting | Value |
| :--- | :--- |
| IMAP Server | `localhost` |
| Port | `1143` |
| Connection Security | `None` (or STARTTLS) |
| Authentication | `Normal password` |
| Username | anything (e.g. `you@example.com`) |
| Password | anything (e.g. `test`) |

> In Thunderbird, you may see a warning about an unencrypted connection. Click **I understand the risks** to proceed — this is normal for local development.

### Test IMAP with Python (optional):
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

## Step 9 — Read via POP3

You can also read messages over POP3:

| Setting | Value |
| :--- | :--- |
| POP3 Server | `localhost` |
| Port | `1110` |
| Connection Security | `None` |
| Username | anything (e.g. `you@example.com`) |
| Password | anything (e.g. `test`) |

### Test POP3 with Python:
```python
import poplib

pop = poplib.POP3("localhost", 1110)
pop.user("you@example.com")
pop.pass_("test")

count, size = pop.stat()
print(f"{count} message(s), {size} bytes")

if count:
    _, lines, _ = pop.retr(1)
    print("\n".join(l.decode() for l in lines[:15]))

pop.quit()
```

---

## Step 10 — REST API Quick Checks (PowerShell)

You can query Sandesh directly from PowerShell using `Invoke-RestMethod`:

```powershell
# Health check
Invoke-RestMethod http://localhost:8080/api/v1/health
# status  version
# ------  -------
# ok      0.1.0

# List messages
(Invoke-RestMethod http://localhost:8080/api/v1/messages).items | Select-Object id, subject, from, to

# Download raw MIME bytes of a message
Invoke-RestMethod http://localhost:8080/api/v1/messages/<ID>/raw
```

---

## Step 11 — Run as a Background Service (Optional)

Use the Windows Task Scheduler to start Sandesh automatically:

1. Open **Task Scheduler** (search in Start menu).
2. Click **Create Basic Task...** in the right panel.
3. Name it `Sandesh`, click Next.
4. Choose **When I log on**, click Next.
5. Choose **Start a program**, click Next.
6. Browse to `C:\sandesh\sandesh.exe`, click Next.
7. Check **Open the Properties dialog...**, click Finish.
8. In Properties → General tab → check **Run whether user is logged on or not**.

Or use **NSSM** (Non-Sucking Service Manager) for a proper Windows service:
```powershell
# Install NSSM via winget
winget install NSSM.NSSM

# Register Sandesh as a Windows service
nssm install Sandesh C:\sandesh\sandesh.exe
nssm set Sandesh AppEnvironmentExtra SANDESH_DATA_DIR=C:\sandesh\data
nssm start Sandesh
```

---

## Customising Ports (PowerShell)

```powershell
$env:SANDESH_SMTP_ADDR = "127.0.0.1:2025"
$env:SANDESH_HTTP_ADDR = "127.0.0.1:9080"
$env:SANDESH_IMAP_ADDR = "127.0.0.1:2143"
$env:SANDESH_POP3_ADDR = "127.0.0.1:2110"
$env:SANDESH_DATA_DIR  = "C:\sandesh\data"
.\sandesh.exe
```

---

## Troubleshooting

| Problem | Fix |
| :--- | :--- |
| SmartScreen blocks the app | Click "More info" → "Run anyway", or run `Unblock-File` in PowerShell |
| Port 1025 in use | Set `$env:SANDESH_SMTP_ADDR="127.0.0.1:2025"` before running |
| Firewall blocking ports | Allow `sandesh.exe` through Windows Defender Firewall (or use `localhost` only) |
| Web UI shows "connection refused" | Make sure the terminal window with Sandesh is still open |
| Closing terminal kills Sandesh | Use Task Scheduler or NSSM to run it as a background service |

