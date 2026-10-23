# Sandesh — Integration & Testing Guide

This guide provides step-by-step instructions for configuring and testing applications with Sandesh across **SMTP**, **IMAP**, **POP3**, and the **Web UI / REST API**.

---

## 1. Quick Reference: Endpoints & Defaults

Sandesh is one binary with no runtime to install. Get it whichever way suits you:

```bash
# A release download — verify, unpack, run. See README.md for the archive per platform.
sha256sum -c sandesh-<version>-<target>.tar.gz.sha256 && tar xzf sandesh-<version>-<target>.tar.gz
./sandesh

# Or a container.
docker run -d -p 1025:1025 -p 1080:1080 -p 1143:1143 -p 1110:1110 -v sandesh-data:/data sandesh

# Or from a source checkout.
cargo run
```

`./sandesh --help` lists the environment variables; there are no command-line flags.

> [!NOTE]
> The container publishes the Web UI on **1080**, while a local run defaults to **8080**. The tables
> below use the local defaults.

When running locally (`cargo run` or `./sandesh`), Sandesh listens on loopback (`127.0.0.1`) with developer-friendly defaults:

| Protocol | Default Port | Default Security | Default Authentication | Typical Purpose |
| :--- | :--- | :--- | :--- | :--- |
| **SMTP** | `1025` | Cleartext (`None`) / STARTTLS | Disabled (accepts all senders) | Inbound mail from apps, ERPs, frameworks |
| **IMAP** | `1143` | Cleartext (`None`) / STARTTLS | Developer mode (accepts any credentials) | Mailbox fetching, folders, unread search |
| **POP3** | `1110` | Cleartext (`None`) / STLS | Developer mode (accepts any credentials) | Simple inbox polling for legacy clients |
| **HTTP Web UI** | `8080` | Plain HTTP | None | Inspection console, SSE stream, composer |
| **REST API** | `8080` | Plain HTTP (`/api/v1`) | None | Automation, test assertions, programmatic send |

> [!NOTE]
> **Developer Mode Authentication (`AuthMode::Disabled`):**  
> Sandesh accepts any username (e.g. `catch-all`, `admin`, `user@example.com`) and any password (e.g. `test`, `any`, `password`). You do not need to create accounts or seed passwords before testing.

---

## 2. Odoo ERP Integration (Complete Two-Way Chatter)

Sandesh provides full round-trip support for Odoo: sending notifications, quotes, and messages via SMTP, and fetching customer replies back into Odoo Chatter via IMAP or POP3.

```
┌─────────────────┐       SMTP (:1025)       ┌────────────────────────┐
│    Odoo ERP     │─────────────────────────▶│    Sandesh Mailbox     │
│ (Chatter / CRM) │                          │     (Web UI :8080)     │
│                 │◀─────────────────────────│                        │
└─────────────────┘   IMAP (:1143) / POP3    └────────────────────────┘
```

### 2.1 Outgoing Mail Server (SMTP)

1. In Odoo, activate **Developer Mode** (Settings &rarr; scroll down &rarr; *Activate the developer mode*).
2. Navigate to **Settings** &rarr; **Technical** &rarr; **Outgoing Mail Servers**.
3. Click **New** and fill in:
   * **Description:** `Sandesh Local SMTP`
   * **SMTP Server:** `127.0.0.1` (or `localhost`)
   * **SMTP Port:** `1025`
   * **Connection Security:** `None`
   * **Username:** *(leave empty)*
   * **Password:** *(leave empty)*
4. Click **Test Connection**.
   * A green notification will confirm: *"Connection Test Successful! Everything seems properly set up."*

---

### 2.2 Incoming Mail Server (IMAP or POP3)

Navigate to **Settings** &rarr; **Technical** &rarr; **Incoming Mail Servers** and click **New**.

#### Option A: IMAP (Recommended)
* **Name:** `Sandesh Local IMAP`
* **Server Type:** `IMAP Server`
* **Server Name:** `127.0.0.1`
* **Port:** `1143`
* **SSL/TLS:** **Unchecked**
* **Username:** `catch-all` (or any string)
* **Password:** `test` (any string)
* **Keep Attachments:** Checked
* Click **Test & Confirm**. The server state will switch to **Confirmed** (green).

#### Option B: POP3
* **Name:** `Sandesh Local POP3`
* **Server Type:** `POP Server`
* **Server Name:** `127.0.0.1`
* **Port:** `1110`
* **SSL/TLS:** **Unchecked**
* **Username:** `catch-all`
* **Password:** `test`
* Click **Test & Confirm**. The server state will switch to **Confirmed** (green).

---

### 2.3 Testing the Two-Way Chatter Round-Trip

1. **Send a message from Odoo:**
   * Open **Contacts** &rarr; select any partner (e.g. **Abigail Peterson** or **Acme Corporation**).
   * In the bottom Chatter panel, click **Send message**.
   * Type: `Hello from Odoo! Please reply to confirm.` and click **Send**.
2. **Inspect & Reply in Sandesh:**
   * Open your browser at [http://localhost:8080](http://localhost:8080).
   * The message will appear instantly in **INBOX**. Click it to view headers, HTML/Plain preview, and raw MIME.
   * Click the **Reply** button in the message toolbar.
   * The **Developer Mail Composer** will open with the recipient (`admin@example.com` or Odoo alias), subject (`Re: ...`), and quoted history pre-filled.
   * Type your reply: `Hello Odoo, reply confirmed from customer!`
   * Click **Send**.
3. **Fetch in Odoo:**
   * Switch back to Odoo &rarr; **Incoming Mail Servers** &rarr; select `Sandesh Local IMAP`.
   * Click **Fetch Now** (or wait for the scheduled cron).
   * The log will report `Fetched 1 email(s)`.
4. **Verify Chatter:**
   * Return to the partner contact in Odoo.
   * The reply appears threaded directly under your original message in the Chatter!

---

### 2.4 Why Does a Sent Reply Show in Both `INBOX` and `Sent`?

When you reply from Sandesh Web UI, you will notice tags for both `[ INBOX ]` and `[ Sent ]`:
* **`INBOX` placement:** Odoo polls `INBOX` over IMAP/POP3 for unread mail. Sandesh places the reply in `INBOX` so Odoo can discover and fetch it.
* **`Sent` placement:** Sandesh records the message in `Sent` so you have an inspection record of what was authored in the Composer.
* In a local development environment, Sandesh uses a shared catch-all view, allowing you to see both sides of the exchange in one unified console.

---

## 3. Programming Languages & Frameworks

### 3.1 Python

#### Standard `smtplib` and `imaplib`
```python
import smtplib
import imaplib

# 1. Send an email via SMTP
with smtplib.SMTP('127.0.0.1', 1025) as smtp:
    msg = "From: sender@example.com\r\nTo: recipient@example.com\r\nSubject: Test\r\n\r\nHello Sandesh!"
    smtp.sendmail('sender@example.com', ['recipient@example.com'], msg)

# 2. Retrieve unread emails via IMAP
with imaplib.IMAP4('127.0.0.1', 1143) as imap:
    imap.login('catch-all', 'any_password')
    imap.select('INBOX')
    _, data = imap.search(None, '(UNSEEN)')
    for msg_id in data[0].split():
        _, fetch_data = imap.fetch(msg_id, '(RFC822)')
        raw_email = fetch_data[0][1]
        print("Received raw message:", raw_email.decode('utf-8', errors='replace'))
```

#### Django (`settings.py`)
```python
EMAIL_BACKEND = 'django.core.mail.backends.smtp.EmailBackend'
EMAIL_HOST = '127.0.0.1'
EMAIL_PORT = 1025
EMAIL_USE_TLS = False
EMAIL_USE_SSL = False
```

#### Flask-Mail
```python
app.config['MAIL_SERVER'] = '127.0.0.1'
app.config['MAIL_PORT'] = 1025
app.config['MAIL_USE_TLS'] = False
app.config['MAIL_USE_SSL'] = False
```

---

### 3.2 Node.js / TypeScript (Nodemailer)

```javascript
import nodemailer from 'nodemailer';

const transporter = nodemailer.createTransport({
  host: '127.0.0.1',
  port: 1025,
  secure: false, // true for 465, false for other ports
  ignoreTLS: true,
});

await transporter.sendMail({
  from: '"App Notifications" <alerts@myapp.local>',
  to: 'developer@example.com',
  subject: 'Password Reset Request',
  text: 'Click here to reset your password.',
  html: '<b>Click here</b> to reset your password.',
});
```

---

### 3.3 PHP / Laravel

#### Laravel (`.env`)
```ini
MAIL_MAILER=smtp
MAIL_HOST=127.0.0.1
MAIL_PORT=1025
MAIL_USERNAME=null
MAIL_PASSWORD=null
MAIL_ENCRYPTION=null
MAIL_FROM_ADDRESS="hello@example.com"
MAIL_FROM_NAME="${APP_NAME}"
```

#### PHPMailer
```php
use PHPMailer\PHPMailer\PHPMailer;

$mail = new PHPMailer();
$mail->isSMTP();
$mail->Host = '127.0.0.1';
$mail->Port = 1025;
$mail->SMTPAuth = false;
$mail->SMTPSecure = false;
$mail->setFrom('from@example.com', 'Mailer');
$mail->addAddress('to@example.com');
$mail->Subject = 'Test Email';
$mail->Body    = 'This is the HTML message body';
$mail->send();
```

---

### 3.4 Go (`net/smtp`)

```go
package main

import (
	"log"
	"net/smtp"
)

func main() {
	from := "sender@example.com"
	to := []string{"recipient@example.com"}
	msg := []byte("To: recipient@example.com\r\n" +
		"Subject: Hello from Go!\r\n" +
		"\r\n" +
		"Testing Sandesh SMTP server.\r\n")

	// In developer mode, authentication is optional
	err := smtp.SendMail("127.0.0.1:1025", nil, from, to, msg)
	if err != nil {
		log.Fatalf("Error sending mail: %v", err)
	}
	log.Println("Email sent successfully!")
}
```

---

### 3.5 Ruby on Rails (`config/environments/development.rb`)

```ruby
config.action_mailer.delivery_method = :smtp
config.action_mailer.smtp_settings = {
  address: '127.0.0.1',
  port: 1025,
  enable_starttls_auto: false
}
```

---

## 4. Desktop Email Clients (Thunderbird, Apple Mail)

You can connect standard desktop email clients directly to Sandesh to inspect received mail or test draft synchronization:

* **Incoming Server (IMAP):**
  * Server: `127.0.0.1`
  * Port: `1143`
  * Connection Security: `None`
  * Authentication: `Password, transmitted insecurely` (Plaintext) or `Normal Password`
  * Username: `catch-all`
* **Outgoing Server (SMTP):**
  * Server: `127.0.0.1`
  * Port: `1025`
  * Connection Security: `None`
  * Authentication: `No authentication`

---

## 5. Webhooks — Reacting to Delivered Mail

Instead of polling IMAP, register an HTTP endpoint and Sandesh will `POST` to it every time a message
is delivered. Failed deliveries are retried with exponential backoff and kept in an audit trail you
can read in the **Webhooks** view.

```bash
# Register an endpoint. `sign` is optional; see below.
curl -X POST http://127.0.0.1:8080/api/v1/webhooks \
  -H 'Content-Type: application/json' \
  -d '{"url":"http://localhost:3000/hooks/mail"}'

curl http://127.0.0.1:8080/api/v1/webhooks           # list endpoints
curl http://127.0.0.1:8080/api/v1/webhooks/outbox    # delivery attempts and failures
curl -X DELETE http://127.0.0.1:8080/api/v1/webhooks/<id>
```

### 5.1 Verifying signed payloads

Signing is **off by default** — a webhook pointed at a local test receiver has nothing to spoof. Pass
`sign: true` and Sandesh generates a secret and returns it in the create response, which is the only
response that ever carries it. **Store it then:** it cannot be recovered afterwards, only replaced by
registering the endpoint again. A listing reports `has_secret`, a boolean, never the key. To use a key
your receiver already knows, send it as `secret` alongside `sign`.

Signed deliveries carry `X-Sandesh-Signature: t=<unix seconds>,v1=<hex HMAC-SHA256>`. The signed
material is `"<t>.<body>"` — the timestamp is inside what is signed, so a captured delivery cannot be
replayed verbatim forever; a receiver rejects anything outside its tolerated clock skew. Each retry is
signed afresh with the time it was actually sent, so enforcing a tolerance will not reject a
legitimate redelivery. An unsigned endpoint sends no signature header at all.

```python
import hashlib, hmac, time

def verify(secret: str, body: bytes, header: str, tolerance: int = 300) -> bool:
    fields = dict(f.split("=", 1) for f in header.split(","))
    t = int(fields["t"])
    if abs(time.time() - t) > tolerance:   # reject a replayed capture
        return False
    expected = hmac.new(
        secret.encode(), f"{t}.".encode() + body, hashlib.sha256
    ).hexdigest()
    return hmac.compare_digest(expected, fields["v1"])
```

Compare in constant time — `hmac.compare_digest`, not `==`. A byte-by-byte comparison leaks, through
timing, how much of a forged signature was correct.

---

## 6. Troubleshooting & FAQs

### Q: Why did Odoo say `server not IMAP4 compliant`?
Sandesh implements **IMAP4rev2 (RFC 9051)** while advertising **IMAP4rev1 (RFC 3501)** for compatibility. Python's `imaplib` (used by Odoo) strictly enforces `AllowedVersions = ('IMAP4REV1', 'IMAP4')`. Sandesh advertises both capabilities simultaneously so Odoo and standard Python libraries connect cleanly.

### Q: Odoo reports `Fetched 0 email(s)` when clicking Fetch Now?
IMAP searches for messages with the `(UNSEEN)` flag. If a message was already fetched once or marked as read in the Sandesh Web UI, IMAP treats it as seen. To test again, send a new message or reply via Sandesh Composer, then click **Fetch Now**.

### Q: Can I trigger programmatic test deliveries?
Yes! Use the REST API:
```bash
curl -X POST http://127.0.0.1:8080/api/v1/send \
  -H "Content-Type: application/json" \
  -d '{
    "from": "alice@example.com",
    "to": ["bob@example.com"],
    "subject": "Automated Test Email",
    "text_body": "Test message body content"
  }'
```
All messages submitted through `POST /api/v1/send` emit real-time Server-Sent Events (SSE), appear instantly in the Web UI, and are available for retrieval over IMAP and POP3.
