/**
 * Setup and Integration Guide page.
 *
 * Provides a comprehensive, interactive guide for configuring Sandesh across
 * Odoo ERP, frameworks (Python, Node, PHP), and desktop mail clients.
 */

import { useState } from 'react'
import { Link } from 'react-router-dom'
import { Lock, Unlock, Users } from 'lucide-react'
import { CopyButton } from '../components/CopyButton'
import { useInfo, useWorkspaces } from '../hooks/queries'
import './GuideRoute.css'
import './SettingsRoute.css'

type TabKey = 'odoo' | 'python' | 'node' | 'php' | 'go' | 'ruby' | 'clients' | 'api'

export function GuideRoute() {
  const { data: info } = useInfo()
  const { data: workspacesList } = useWorkspaces()
  const workspaces = workspacesList ?? []
  const [selectedWsId, setSelectedWsId] = useState<string>('')
  const [activeTab, setActiveTab] = useState<TabKey>('odoo')

  const currentWs =
    workspaces.find((w) => w.id === selectedWsId) ??
    workspaces.find((w) => w.is_active) ??
    workspaces[0]
  const wsId = currentWs?.id ?? 'default'
  const scopedUser = `${wsId}/dev`
  const authRequired = currentWs?.auth_required ?? false

  const smtpAddr = info?.smtp_address ?? '127.0.0.1:1025'
  const imapAddr = info?.imap_address ?? '127.0.0.1:1143'
  const pop3Addr = info?.pop3_address ?? '127.0.0.1:1110'
  const httpAddr = info?.http_address ?? '127.0.0.1:8080'

  return (
    <div className="guide">
      <div className="guide__inner">
        <div className="panel">
          <h1 className="panel__title">Integration & Setup Guide</h1>
        <p className="panel__lede">
          Connect any application, ERP, or test suite to Sandesh using standard mail protocols.
        </p>

        {/* Workspace selector & Auth status bar */}
        <div className="guide__workspace-bar">
          <div className="guide__workspace-select-col">
            <label htmlFor="guide-ws-select" className="guide__workspace-label">
              Configuring Workspace:
            </label>
            <select
              id="guide-ws-select"
              className="guide__workspace-select"
              value={wsId}
              onChange={(e) => setSelectedWsId(e.target.value)}
            >
              {workspaces.map((ws) => (
                <option key={ws.id} value={ws.id}>
                  {ws.name} ({ws.id}){ws.is_active ? ' — Active' : ''}
                </option>
              ))}
            </select>
          </div>

          <div className="guide__workspace-auth-col">
            <span className="guide__workspace-label">Auth Policy:</span>
            {authRequired ? (
              <span className="ws-auth-badge ws-auth-badge--required" title="Authentication strictly required">
                <Lock size={11} aria-hidden="true" />
                <span>Auth Required</span>
              </span>
            ) : (
              <span className="ws-auth-badge ws-auth-badge--optional" title="Permissive dev mode">
                <Unlock size={11} aria-hidden="true" />
                <span>Permissive Dev Mode (Optional)</span>
              </span>
            )}
          </div>

          <div className="guide__workspace-auth-col">
            <Link
              to="/settings?tab=accounts"
              className="button button--ghost button--sm"
              style={{ display: 'inline-flex', alignItems: 'center', gap: '5px', textDecoration: 'none' }}
              title="Open Settings to manage accounts and set passwords"
            >
              <Users size={12} aria-hidden="true" />
              <span>Manage Credentials in Settings &rarr;</span>
            </Link>
          </div>
        </div>

        <div className="guide__grid">
          <div className="guide__card">
            <div className="guide__card-header">
              <span className="guide__card-title">SMTP (Ingress)</span>
              <CopyButton value={smtpAddr} label="SMTP address" />
            </div>
            <code className="guide__card-value">{smtpAddr}</code>
            <span className="guide__card-hint">Send mail here from your app</span>
          </div>

          <div className="guide__card">
            <div className="guide__card-header">
              <span className="guide__card-title">IMAP (Retrieval)</span>
              <CopyButton value={imapAddr} label="IMAP address" />
            </div>
            <code className="guide__card-value">{imapAddr}</code>
            <span className="guide__card-hint">RFC 9051 & RFC 3501 compatible</span>
          </div>

          <div className="guide__card">
            <div className="guide__card-header">
              <span className="guide__card-title">POP3 (Retrieval)</span>
              <CopyButton value={pop3Addr} label="POP3 address" />
            </div>
            <code className="guide__card-value">{pop3Addr}</code>
            <span className="guide__card-hint">Simple inbox polling</span>
          </div>

          <div className="guide__card">
            <div className="guide__card-header">
              <span className="guide__card-title">Web UI / REST</span>
              <CopyButton value={httpAddr} label="HTTP address" />
            </div>
            <code className="guide__card-value">{httpAddr}</code>
            <span className="guide__card-hint">Inspection console & API</span>
          </div>
        </div>

        <div className="guide__note">
          <strong>Scoped Username Routing:</strong> Use <code>{scopedUser}</code> in your applications
          to guarantee emails route into <strong>{currentWs?.name ?? wsId}</strong> regardless of which
          workspace is currently active.
          {authRequired ? (
            <div style={{ marginTop: '4px', color: 'var(--warn)' }}>
              ⚠️ Authentication is strictly required for this workspace. Clients must supply credentials.
            </div>
          ) : (
            <div style={{ marginTop: '4px' }}>
              In permissive developer mode, clients may use any password or omit authentication.
            </div>
          )}
        </div>

        <div className="guide__tabs" role="tablist">
          <button
            type="button"
            role="tab"
            aria-selected={activeTab === 'odoo'}
            className={`guide__tab${activeTab === 'odoo' ? ' guide__tab--active' : ''}`}
            onClick={() => setActiveTab('odoo')}
          >
            🏢 Odoo ERP
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={activeTab === 'python'}
            className={`guide__tab${activeTab === 'python' ? ' guide__tab--active' : ''}`}
            onClick={() => setActiveTab('python')}
          >
            🐍 Python / Django
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={activeTab === 'node'}
            className={`guide__tab${activeTab === 'node' ? ' guide__tab--active' : ''}`}
            onClick={() => setActiveTab('node')}
          >
            🟨 Node.js
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={activeTab === 'php'}
            className={`guide__tab${activeTab === 'php' ? ' guide__tab--active' : ''}`}
            onClick={() => setActiveTab('php')}
          >
            🐘 PHP / Laravel
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={activeTab === 'clients'}
            className={`guide__tab${activeTab === 'clients' ? ' guide__tab--active' : ''}`}
            onClick={() => setActiveTab('clients')}
          >
            ✉️ Desktop Clients
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={activeTab === 'go'}
            className={`guide__tab${activeTab === 'go' ? ' guide__tab--active' : ''}`}
            onClick={() => setActiveTab('go')}
          >
            🐹 Go
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={activeTab === 'ruby'}
            className={`guide__tab${activeTab === 'ruby' ? ' guide__tab--active' : ''}`}
            onClick={() => setActiveTab('ruby')}
          >
            💎 Ruby / Rails
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={activeTab === 'api'}
            className={`guide__tab${activeTab === 'api' ? ' guide__tab--active' : ''}`}
            onClick={() => setActiveTab('api')}
          >
            ⚡ REST API
          </button>
        </div>

        {activeTab === 'odoo' && (
          <div className="guide__content">
            <h2 className="guide__section-title">1. Outgoing Mail Server (SMTP)</h2>
            <p className="panel__lede">
              Go to <strong>Settings &rarr; Technical &rarr; Outgoing Mail Servers &rarr; New</strong>:
            </p>
            <table className="guide__table">
              <thead>
                <tr>
                  <th>Field</th>
                  <th>Value</th>
                </tr>
              </thead>
              <tbody>
                <tr>
                  <td>Description</td>
                  <td><code>Sandesh - {currentWs?.name ?? wsId}</code></td>
                </tr>
                <tr>
                  <td>SMTP Server</td>
                  <td><code>127.0.0.1</code></td>
                </tr>
                <tr>
                  <td>SMTP Port</td>
                  <td><code>1025</code></td>
                </tr>
                <tr>
                  <td>Connection Security</td>
                  <td><code>None</code></td>
                </tr>
                <tr>
                  <td>Username</td>
                  <td><code>{scopedUser}</code> <em>(or bare <code>dev</code>)</em></td>
                </tr>
                <tr>
                  <td>Password</td>
                  <td>{authRequired ? <strong>(Required - set for this workspace)</strong> : <em>(Leave blank or any text in dev mode)</em>}</td>
                </tr>
              </tbody>
            </table>
            <p className="panel__lede">
              Click <strong>Test Connection</strong>. It will confirm with a green success message.
            </p>

            <h2 className="guide__section-title">2. Incoming Mail Server (IMAP or POP3)</h2>
            <p className="panel__lede">
              Go to <strong>Settings &rarr; Technical &rarr; Incoming Mail Servers &rarr; New</strong>:
            </p>
            <table className="guide__table">
              <thead>
                <tr>
                  <th>Field</th>
                  <th>IMAP (Recommended)</th>
                  <th>POP3</th>
                </tr>
              </thead>
              <tbody>
                <tr>
                  <td>Server Type</td>
                  <td><code>IMAP Server</code></td>
                  <td><code>POP Server</code></td>
                </tr>
                <tr>
                  <td>Server Name</td>
                  <td><code>127.0.0.1</code></td>
                  <td><code>127.0.0.1</code></td>
                </tr>
                <tr>
                  <td>Port</td>
                  <td><code>1143</code></td>
                  <td><code>1110</code></td>
                </tr>
                <tr>
                  <td>SSL/TLS</td>
                  <td>Unchecked</td>
                  <td>Unchecked</td>
                </tr>
                <tr>
                  <td>Username</td>
                  <td><code>{scopedUser}</code></td>
                  <td><code>{scopedUser}</code></td>
                </tr>
                <tr>
                  <td>Password</td>
                  <td>{authRequired ? <strong>(Required)</strong> : <code>test</code>}</td>
                  <td>{authRequired ? <strong>(Required)</strong> : <code>test</code>}</td>
                </tr>
              </tbody>
            </table>
            <p className="panel__lede">
              Click <strong>Test & Confirm</strong>. The status will turn green (<strong>Confirmed</strong>).
            </p>

            <h2 className="guide__section-title">3. Testing the Two-Way Chatter Round-Trip</h2>
            <ol className="guide__steps">
              <li>
                <strong>Send from Odoo Chatter:</strong> Open any contact or lead (e.g. Abigail Peterson
                or Acme Corporation) and click <em>Send message</em> in the chatter.
              </li>
              <li>
                <strong>Inspect in Sandesh:</strong> The email arrives immediately at{' '}
                <a href="/" target="_blank" rel="noreferrer">
                  Sandesh Web UI
                </a>
                .
              </li>
              <li>
                <strong>Quick Reply:</strong> Click the <strong>Reply</strong> button in the message
                header. Type your customer reply and click <strong>Send</strong>.
              </li>
              <li>
                <strong>Fetch in Odoo:</strong> Go to Odoo &rarr; Incoming Mail Servers &rarr; click{' '}
                <strong>Fetch Now</strong>.
              </li>
              <li>
                <strong>Verify:</strong> Open the contact in Odoo &mdash; your reply appears threaded
                directly in the Chatter!
              </li>
            </ol>

            <div className="guide__note">
              <strong>Why is a sent reply tagged with both [ INBOX ] and [ Sent ]?</strong>
              <br />
              In local development, Sandesh places replies in <code>INBOX</code> so Odoo can fetch them
              into the chatter, and in <code>Sent</code> so you have an inspection record of your
              outgoing message.
            </div>
          </div>
        )}

        {activeTab === 'python' && (
          <div className="guide__content">
            <h2 className="guide__section-title">Python smtplib & imaplib</h2>
            <div className="guide__code-block">
              <pre>{`import smtplib
import imaplib

# 1. Send an email via SMTP
with smtplib.SMTP('127.0.0.1', 1025) as smtp:
    ${authRequired ? `smtp.login('${scopedUser}', 'your-password')\n    ` : ''}msg = "From: dev@local\\r\\nTo: user@local\\r\\nSubject: Test\\r\\n\\r\\nHello Sandesh!"
    smtp.sendmail('dev@local', ['user@local'], msg)

# 2. Retrieve unread emails via IMAP
with imaplib.IMAP4('127.0.0.1', 1143) as imap:
    imap.login('${scopedUser}', '${authRequired ? 'your-password' : 'any_password'}')
    imap.select('INBOX')
    _, data = imap.search(None, '(UNSEEN)')
    for msg_id in data[0].split():
        _, fetch_data = imap.fetch(msg_id, '(RFC822)')
        raw_email = fetch_data[0][1]
        print("Received:", raw_email.decode('utf-8', errors='replace'))`}</pre>
            </div>

            <h2 className="guide__section-title">Django settings.py</h2>
            <div className="guide__code-block">
              <pre>{`EMAIL_BACKEND = 'django.core.mail.backends.smtp.EmailBackend'
EMAIL_HOST = '127.0.0.1'
EMAIL_PORT = 1025
EMAIL_HOST_USER = '${scopedUser}'
EMAIL_HOST_PASSWORD = '${authRequired ? 'your-password' : ''}'
EMAIL_USE_TLS = False
EMAIL_USE_SSL = False`}</pre>
            </div>
          </div>
        )}

        {activeTab === 'node' && (
          <div className="guide__content">
            <h2 className="guide__section-title">Nodemailer Configuration</h2>
            <div className="guide__code-block">
              <pre>{`import nodemailer from 'nodemailer';

const transporter = nodemailer.createTransport({
  host: '127.0.0.1',
  port: 1025,
  secure: false,
  ignoreTLS: true,
  auth: {
    user: '${scopedUser}',
    pass: '${authRequired ? 'your-password' : 'test'}',
  },
});

await transporter.sendMail({
  from: '"App Notifications" <alerts@myapp.local>',
  to: 'developer@example.com',
  subject: 'Welcome to our platform!',
  text: 'Hello, your account is ready in workspace "${currentWs?.name ?? wsId}".',
  html: '<b>Hello!</b> Your account is ready.',
});`}</pre>
            </div>
          </div>
        )}

        {activeTab === 'php' && (
          <div className="guide__content">
            <h2 className="guide__section-title">Laravel (.env)</h2>
            <div className="guide__code-block">
              <pre>{`MAIL_MAILER=smtp
MAIL_HOST=127.0.0.1
MAIL_PORT=1025
MAIL_USERNAME="${scopedUser}"
MAIL_PASSWORD="${authRequired ? 'your-password' : 'null'}"
MAIL_ENCRYPTION=null
MAIL_FROM_ADDRESS="hello@example.com"
MAIL_FROM_NAME="\${APP_NAME}"`}</pre>
            </div>
          </div>
        )}

        {activeTab === 'clients' && (
          <div className="guide__content">
            <h2 className="guide__section-title">Thunderbird / Apple Mail Configuration</h2>
            <table className="guide__table">
              <thead>
                <tr>
                  <th>Setting</th>
                  <th>Incoming (IMAP)</th>
                  <th>Outgoing (SMTP)</th>
                </tr>
              </thead>
              <tbody>
                <tr>
                  <td>Server</td>
                  <td><code>127.0.0.1</code></td>
                  <td><code>127.0.0.1</code></td>
                </tr>
                <tr>
                  <td>Port</td>
                  <td><code>1143</code></td>
                  <td><code>1025</code></td>
                </tr>
                <tr>
                  <td>Security</td>
                  <td><code>None</code></td>
                  <td><code>None</code></td>
                </tr>
                <tr>
                  <td>Authentication</td>
                  <td><code>Normal password</code> (or Plaintext)</td>
                  <td>{authRequired ? <code>Normal password</code> : <code>No authentication</code>}</td>
                </tr>
                <tr>
                  <td>Username</td>
                  <td><code>{scopedUser}</code></td>
                  <td><code>{scopedUser}</code></td>
                </tr>
                <tr>
                  <td>Password</td>
                  <td>{authRequired ? <strong>(Required password)</strong> : <code>test</code>}</td>
                  <td>{authRequired ? <strong>(Required password)</strong> : <em>(None or test)</em>}</td>
                </tr>
              </tbody>
            </table>
          </div>
        )}

        {activeTab === 'go' && (
          <div className="guide__content">
            <h2 className="guide__section-title">Go — net/smtp</h2>
            <div className="guide__code-block">
              <pre>{`package main

import (
\t"log"
\t"net/smtp"
)

func main() {
\tfrom := "sender@example.com"
\tto := []string{"recipient@example.com"}
\tmsg := []byte(
\t\t"To: recipient@example.com\\r\\n" +
\t\t"Subject: Hello from Go!\\r\\n" +
\t\t"\\r\\n" +
\t\t"Testing Sandesh SMTP server.\\r\\n",
\t)
\t// Developer mode: pass nil for auth
\terr := smtp.SendMail("127.0.0.1:1025", nil, from, to, msg)
\tif err != nil {
\t\tlog.Fatalf("smtp error: %v", err)
\t}
\tlog.Println("Email sent!")
}`}
              </pre>
            </div>
          </div>
        )}

        {activeTab === 'ruby' && (
          <div className="guide__content">
            <h2 className="guide__section-title">Ruby on Rails — config/environments/development.rb</h2>
            <div className="guide__code-block">
              <pre>{`config.action_mailer.delivery_method = :smtp
config.action_mailer.smtp_settings = {
  address: '127.0.0.1',
  port: 1025,
  enable_starttls_auto: false
}`}
              </pre>
            </div>
          </div>
        )}

        {activeTab === 'api' && (
          <div className="guide__content">
            <h2 className="guide__section-title">Compose & Send Email via REST API</h2>
            <p className="panel__lede">
              Trigger programmatic deliveries for testing and automated pipelines:
            </p>
            <div className="guide__code-block">
              <pre>{`curl -X POST http://127.0.0.1:8080/api/v1/send \\
  -H "Content-Type: application/json" \\
  -d '{
    "from": "sender@test.local",
    "to": ["admin@example.com"],
    "subject": "Automated Pipeline Test",
    "text_body": "This message is delivered to INBOX and Sent!"
  }'`}</pre>
            </div>
          </div>
        )}
      </div>
    </div>
  </div>
  )
}
