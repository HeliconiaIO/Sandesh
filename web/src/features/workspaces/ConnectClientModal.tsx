import { useState } from 'react'
import {
  Building2,
  Check,
  Code2,
  Eye,
  EyeOff,
  FileText,
  Globe,
  KeyRound,
  Lock,
  Mail,
  RefreshCw,
  Server,
  Sparkles,
  Terminal,
  Unlock,
  User,
  X,
} from 'lucide-react'
import type { Workspace } from '../../api/client'
import { CopyButton } from '../../components/CopyButton'
import {
  useAccounts,
  useActivateWorkspace,
  useInfo,
  usePatchAccount,
  useWorkspaces,
} from '../../hooks/queries'
import './ConnectClientModal.css'

interface ConnectClientModalProps {
  workspace: Workspace
  onClose: () => void
}

type TabKey = 'odoo' | 'node' | 'python' | 'laravel' | 'go' | 'spring' | 'desktop'

function parseHostPort(address?: string | null, defaultPort = '1025'): { host: string; port: string } {
  if (!address) return { host: '127.0.0.1', port: defaultPort }
  const parts = address.split(':')
  if (parts.length >= 2 && parts[0] && parts[parts.length - 1]) {
    const host = parts[0] === '0.0.0.0' ? '127.0.0.1' : parts[0]
    const port = parts[parts.length - 1] ?? defaultPort
    return { host, port }
  }
  return { host: '127.0.0.1', port: defaultPort }
}

export function ConnectClientModal({ workspace, onClose }: ConnectClientModalProps) {
  const { data: info } = useInfo()
  const { data: workspaces } = useWorkspaces()
  const activateWorkspace = useActivateWorkspace()
  const { data: accountsData } = useAccounts()
  const patchAccount = usePatchAccount()

  const [activeTab, setActiveTab] = useState<TabKey>('odoo')
  const [useScoped, setUseScoped] = useState(false)
  const [passwordInput, setPasswordInput] = useState('')
  const [showPassword, setShowPassword] = useState(false)
  const [saveSuccess, setSaveSuccess] = useState(false)
  const [saveError, setSaveError] = useState<string | null>(null)

  const activeWs = workspaces?.find((w) => w.is_active)
  const isThisActive = activeWs?.id === workspace.id

  const accounts = isThisActive ? accountsData?.items ?? [] : []
  const primaryAccount = accounts.find((a) => a.is_catch_all) ?? accounts[0]
  const [selectedAccountId, setSelectedAccountId] = useState<string | undefined>(undefined)

  const currentAccount = accounts.find((a) => a.id === (selectedAccountId ?? primaryAccount?.id)) ?? primaryAccount
  const baseUsername = currentAccount?.username ?? 'dev'

  const effectiveUsername = useScoped ? `${workspace.id}/${baseUsername}` : baseUsername
  const effectivePassword = passwordInput.trim()
    ? passwordInput.trim()
    : currentAccount?.has_password
      ? '(configured password)'
      : workspace.auth_required
        ? 'your-password'
        : 'any-password'

  const handleSavePassword = async () => {
    if (!currentAccount) return
    const pw = passwordInput.trim()
    if (!pw) {
      setSaveError('Please enter a password before saving')
      return
    }
    setSaveError(null)
    setSaveSuccess(false)
    try {
      await patchAccount.mutateAsync({
        id: currentAccount.id,
        password: pw,
      })
      setSaveSuccess(true)
      setTimeout(() => setSaveSuccess(false), 4000)
    } catch (err: unknown) {
      setSaveError(err instanceof Error ? err.message : 'Failed to update password')
    }
  }

  const handleGeneratePassword = () => {
    const chars = 'abcdefghjkmnpqrstuvwxyz23456789'
    let gen = ''
    for (let i = 0; i < 8; i++) {
      gen += chars.charAt(Math.floor(Math.random() * chars.length))
    }
    setPasswordInput(gen)
    setShowPassword(true)
  }

  const handleActivateThis = async () => {
    try {
      await activateWorkspace.mutateAsync(workspace.id)
    } catch {
      // Ignored
    }
  }

  const smtp = parseHostPort(info?.smtp_address, '1025')
  const imap = parseHostPort(info?.imap_address, '1143')
  const pop3 = parseHostPort(info?.pop3_address, '1110')

  const snippets: Record<TabKey, { title: string; filename: string; code: string; desc: string }> = {
    odoo: {
      title: 'Odoo ERP Mail Settings',
      filename: 'Settings > Technical > Email',
      desc: 'Configure Odoo Outgoing (SMTP) and Incoming (IMAP/POP3) mail servers. Credentials below match your configured account.',
      code: `# Outgoing Mail Server (SMTP)
Server: ${smtp.host}
Port: ${smtp.port}
Connection Security: None (or STARTTLS)
Username: ${effectiveUsername}
Password: ${effectivePassword}

# Incoming Mail Server (IMAP)
Server Type: IMAP Server
Server: ${imap.host}
Port: ${imap.port}
SSL/TLS: unchecked
Username: ${effectiveUsername}
Password: ${effectivePassword}`,
    },
    node: {
      title: 'Node.js (Nodemailer)',
      filename: 'mailer.js',
      desc: 'Send test emails via Nodemailer routed directly to this workspace.',
      code: `const nodemailer = require('nodemailer');

const transporter = nodemailer.createTransport({
  host: '${smtp.host}',
  port: ${smtp.port},
  secure: false, // TLS not required for local Sandesh
  auth: {
    user: '${effectiveUsername}',
    pass: '${effectivePassword}',
  },
});

async function sendTestEmail() {
  const info = await transporter.sendMail({
    from: '"Test Sender" <sender@example.com>',
    to: 'customer@example.com',
    subject: 'Order Confirmation #${workspace.id}',
    text: 'Hello from Node.js in workspace "${workspace.name}"!',
  });
  console.log('Message delivered:', info.messageId);
}

sendTestEmail();`,
    },
    python: {
      title: 'Python (smtplib & imaplib)',
      filename: 'email_test.py',
      desc: 'Standard library SMTP delivery and IMAP mailbox retrieval.',
      code: `import smtplib
import imaplib
from email.message import EmailMessage

# 1. Send an email via SMTP
msg = EmailMessage()
msg['Subject'] = 'Invoice for ${workspace.name}'
msg['From'] = 'billing@example.com'
msg['To'] = 'customer@example.com'
msg.set_content('This is a test invoice routed into workspace ${workspace.id}.')

with smtplib.SMTP('${smtp.host}', ${smtp.port}) as server:
    server.login('${effectiveUsername}', '${effectivePassword}')
    server.send_message(msg)
    print('Delivered to Sandesh SMTP')

# 2. Retrieve inbox via IMAP
with imaplib.IMAP4('${imap.host}', ${imap.port}) as mail:
    mail.login('${effectiveUsername}', '${effectivePassword}')
    mail.select('INBOX')
    status, data = mail.search(None, 'ALL')
    print('Found message IDs:', data[0].split())`,
    },
    laravel: {
      title: 'PHP / Laravel (.env)',
      filename: '.env',
      desc: 'Configure Laravel mail driver to deliver all outgoing emails into this workspace.',
      code: `MAIL_MAILER=smtp
MAIL_HOST=${smtp.host}
MAIL_PORT=${smtp.port}
MAIL_USERNAME="${effectiveUsername}"
MAIL_PASSWORD="${effectivePassword}"
MAIL_ENCRYPTION=null
MAIL_FROM_ADDRESS="notifications@example.com"
MAIL_FROM_NAME="${workspace.name}"`,
    },
    go: {
      title: 'Go (net/smtp)',
      filename: 'main.go',
      desc: 'Send test messages in Go using the standard net/smtp package.',
      code: `package main

import (
	"fmt"
	"net/smtp"
)

func main() {
	auth := smtp.PlainAuth("", "${effectiveUsername}", "${effectivePassword}", "${smtp.host}")
	to := []string{"client@example.com"}
	msg := []byte("To: client@example.com\\r\\n" +
		"Subject: Notification from Go\\r\\n" +
		"\\r\\n" +
		"Hello from Go in workspace ${workspace.id}!\\r\\n")

	err := smtp.SendMail("${smtp.host}:${smtp.port}", auth, "app@example.com", to, msg)
	if err != nil {
		panic(err)
	}
	fmt.Println("Sent email successfully to Sandesh")
}`,
    },
    spring: {
      title: 'Java / Spring Boot',
      filename: 'application.properties',
      desc: 'Spring Boot Mail Sender properties with per-workspace routing credentials.',
      code: `spring.mail.host=${smtp.host}
spring.mail.port=${smtp.port}
spring.mail.username=${effectiveUsername}
spring.mail.password=${effectivePassword}
spring.mail.properties.mail.smtp.auth=${workspace.auth_required}
spring.mail.properties.mail.smtp.starttls.enable=false`,
    },
    desktop: {
      title: 'Desktop Mail Clients',
      filename: 'Thunderbird / Apple Mail / Outlook',
      desc: 'Set up an email account in your preferred desktop mail client to inspect messages.',
      code: `Account Type: IMAP or POP3
Incoming Server: ${imap.host} (Port: ${imap.port}, Security: None/STARTTLS)
Outgoing Server (SMTP): ${smtp.host} (Port: ${smtp.port}, Security: None/STARTTLS)
Username: ${effectiveUsername}
Password: ${effectivePassword}
Email Address: ${baseUsername}@${workspace.id}.local`,
    },
  }

  const current = snippets[activeTab]

  return (
    <div
      className="ws-modal-overlay"
      role="dialog"
      aria-modal="true"
      aria-labelledby="connect-ws-title"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose()
      }}
    >
      <div className="ws-modal connect-modal">
        <header className="ws-modal__header">
          <h2 id="connect-ws-title" className="ws-modal__title">
            <Server size={16} aria-hidden="true" />
            <span>Connect to "{workspace.name}"</span>
          </h2>
          <button
            type="button"
            className="ws-modal__close"
            onClick={onClose}
            aria-label="Close connection modal"
          >
            <X size={16} aria-hidden="true" />
          </button>
        </header>

        <div className="connect-modal__body">
          {/* Quick connection coordinates */}
          <div className="connect-modal__summary">
            <div className="connect-modal__summary-item">
              <span className="connect-modal__summary-label">Authentication Mode</span>
              <div>
                {workspace.auth_required ? (
                  <span className="connect-modal__auth-badge connect-modal__auth-badge--required">
                    <Lock size={10} aria-hidden="true" />
                    <span>Auth Required</span>
                  </span>
                ) : (
                  <span className="connect-modal__auth-badge connect-modal__auth-badge--optional">
                    <Unlock size={10} aria-hidden="true" />
                    <span>Permissive (Dev Mode)</span>
                  </span>
                )}
              </div>
            </div>

            <div className="connect-modal__summary-item">
              <span className="connect-modal__summary-label">SMTP Port</span>
              <span className="connect-modal__summary-value">
                <span>{smtp.host}:{smtp.port}</span>
                <CopyButton value={`${smtp.host}:${smtp.port}`} label="SMTP address" />
              </span>
            </div>

            <div className="connect-modal__summary-item">
              <span className="connect-modal__summary-label">IMAP Port</span>
              <span className="connect-modal__summary-value">
                <span>{imap.host}:{imap.port}</span>
                <CopyButton value={`${imap.host}:${imap.port}`} label="IMAP address" />
              </span>
            </div>

            <div className="connect-modal__summary-item">
              <span className="connect-modal__summary-label">POP3 Port</span>
              <span className="connect-modal__summary-value">
                <span>{pop3.host}:{pop3.port}</span>
                <CopyButton value={`${pop3.host}:${pop3.port}`} label="POP3 address" />
              </span>
            </div>
          </div>

          {/* Direct Credentials & Password Manager Box */}
          <div className="connect-modal__creds-card">
            <div className="connect-modal__creds-header">
              <div className="connect-modal__creds-title">
                <KeyRound size={16} aria-hidden="true" style={{ color: 'var(--accent)' }} />
                <span>Account Credentials for "{workspace.name}"</span>
              </div>
              <label htmlFor="connect-use-scoped" className="connect-modal__toggle-row">
                <input
                  id="connect-use-scoped"
                  name="use_scoped"
                  type="checkbox"
                  checked={useScoped}
                  onChange={(e) => setUseScoped(e.target.checked)}
                />
                <span>Use Scoped Username (<code>{workspace.id}/{baseUsername}</code>)</span>
              </label>
            </div>

            {!isThisActive ? (
              <div className="ws-hint-box" style={{ margin: 0 }}>
                <Globe size={14} aria-hidden="true" />
                <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', width: '100%', flexWrap: 'wrap', gap: '8px' }}>
                  <span>This workspace is not currently active. You can still connect using scoped username <code>{workspace.id}/dev</code>.</span>
                  <button
                    type="button"
                    className="btn btn--xs btn--primary-subtle"
                    onClick={handleActivateThis}
                    disabled={activateWorkspace.isPending}
                  >
                    <RefreshCw size={12} className={activateWorkspace.isPending ? 'spin' : ''} aria-hidden="true" />
                    <span>Switch to this Workspace</span>
                  </button>
                </div>
              </div>
            ) : (
              <div className="connect-modal__creds-grid">
                {/* Username column */}
                <div className="connect-modal__field-group">
                  <div className="connect-modal__field-label">
                    <span>Username</span>
                    {accounts.length > 1 && (
                      <select
                        id="connect-acc-select"
                        name="account_id"
                        value={selectedAccountId ?? primaryAccount?.id}
                        onChange={(e) => setSelectedAccountId(e.target.value)}
                        className="connect-modal__acc-select"
                      >
                        {accounts.map((a) => (
                          <option key={a.id} value={a.id}>
                            {a.username} {a.is_catch_all ? '(Catch-all)' : ''}
                          </option>
                        ))}
                      </select>
                    )}
                  </div>
                  <div className="connect-modal__field-input-wrap">
                    <User size={14} style={{ color: 'var(--text-tertiary)' }} aria-hidden="true" />
                    <input
                      id="connect-username"
                      name="username"
                      type="text"
                      readOnly
                      value={effectiveUsername}
                      className="connect-modal__field-input"
                    />
                    <CopyButton value={effectiveUsername} label="username" />
                  </div>
                  <span className="connect-modal__hint" style={{ fontSize: '11px' }}>
                    {useScoped
                      ? 'Scoped username routes directly to this workspace from any client.'
                      : 'Bare username routes to the currently active workspace.'}
                  </span>
                </div>

                {/* Password column */}
                <div className="connect-modal__field-group">
                  <div className="connect-modal__field-label">
                    <span>Password</span>
                    {currentAccount?.has_password && (
                      <span style={{ color: 'var(--ok)', fontSize: '11px', fontWeight: 500 }}>
                        ✓ Password Configured
                      </span>
                    )}
                  </div>
                  <div className="connect-modal__field-input-wrap">
                    <input
                      id="connect-password"
                      name="password"
                      type={showPassword ? 'text' : 'password'}
                      placeholder={currentAccount?.has_password ? 'Enter new password to change...' : 'Set a password'}
                      value={passwordInput}
                      onChange={(e) => {
                        setPasswordInput(e.target.value)
                        setSaveSuccess(false)
                        setSaveError(null)
                      }}
                      className="connect-modal__field-input"
                    />
                    <button
                      type="button"
                      className="connect-modal__icon-btn"
                      onClick={() => setShowPassword((p) => !p)}
                      title={showPassword ? 'Hide password' : 'Show password'}
                      aria-label={showPassword ? 'Hide password' : 'Show password'}
                    >
                      {showPassword ? <EyeOff size={14} aria-hidden="true" /> : <Eye size={14} aria-hidden="true" />}
                    </button>
                    {passwordInput ? (
                      <CopyButton value={passwordInput} label="password" />
                    ) : null}
                    <button
                      type="button"
                      className="connect-modal__icon-btn"
                      onClick={handleGeneratePassword}
                      title="Generate random password"
                      aria-label="Generate random password"
                    >
                      <Sparkles size={14} aria-hidden="true" />
                    </button>
                    <button
                      type="button"
                      className="btn btn--xs btn--primary connect-modal__save-btn"
                      onClick={handleSavePassword}
                      disabled={patchAccount.isPending || !passwordInput.trim()}
                      title="Save this password to the account immediately"
                    >
                      {patchAccount.isPending ? 'Saving...' : 'Save'}
                    </button>
                  </div>

                  {saveSuccess && (
                    <div className="connect-modal__save-feedback">
                      <Check size={12} aria-hidden="true" />
                      <span>Password saved to account "{baseUsername}"! All snippets updated.</span>
                    </div>
                  )}
                  {saveError && (
                    <div className="connect-modal__save-error">
                      <span>{saveError}</span>
                    </div>
                  )}
                  {!saveSuccess && !saveError && (
                    <span className="account-modal__hint" style={{ fontSize: '11px' }}>
                      Type a password and click Save. Code snippets below update automatically.
                    </span>
                  )}
                </div>
              </div>
            )}
          </div>

          {/* Navigation tabs */}
          <div className="connect-modal__tabs" role="tablist" aria-label="Client integration languages">
            <button
              type="button"
              role="tab"
              aria-selected={activeTab === 'odoo'}
              className={`connect-modal__tab${activeTab === 'odoo' ? ' connect-modal__tab--active' : ''}`}
              onClick={() => setActiveTab('odoo')}
            >
              <Building2 size={13} aria-hidden="true" />
              <span>Odoo ERP</span>
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={activeTab === 'node'}
              className={`connect-modal__tab${activeTab === 'node' ? ' connect-modal__tab--active' : ''}`}
              onClick={() => setActiveTab('node')}
            >
              <Code2 size={13} aria-hidden="true" />
              <span>Node.js</span>
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={activeTab === 'python'}
              className={`connect-modal__tab${activeTab === 'python' ? ' connect-modal__tab--active' : ''}`}
              onClick={() => setActiveTab('python')}
            >
              <Terminal size={13} aria-hidden="true" />
              <span>Python</span>
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={activeTab === 'laravel'}
              className={`connect-modal__tab${activeTab === 'laravel' ? ' connect-modal__tab--active' : ''}`}
              onClick={() => setActiveTab('laravel')}
            >
              <FileText size={13} aria-hidden="true" />
              <span>PHP / Laravel</span>
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={activeTab === 'go'}
              className={`connect-modal__tab${activeTab === 'go' ? ' connect-modal__tab--active' : ''}`}
              onClick={() => setActiveTab('go')}
            >
              <Code2 size={13} aria-hidden="true" />
              <span>Go</span>
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={activeTab === 'spring'}
              className={`connect-modal__tab${activeTab === 'spring' ? ' connect-modal__tab--active' : ''}`}
              onClick={() => setActiveTab('spring')}
            >
              <Server size={13} aria-hidden="true" />
              <span>Spring Boot</span>
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={activeTab === 'desktop'}
              className={`connect-modal__tab${activeTab === 'desktop' ? ' connect-modal__tab--active' : ''}`}
              onClick={() => setActiveTab('desktop')}
            >
              <Mail size={13} aria-hidden="true" />
              <span>Desktop Clients</span>
            </button>
          </div>

          {/* Tab content panel */}
          <div className="connect-modal__panel" role="tabpanel">
            <p className="connect-modal__panel-desc">{current.desc}</p>

            {activeTab === 'odoo' ? (
              <div className="connect-modal__odoo-grid">
                <div className="connect-modal__odoo-box">
                  <div className="connect-modal__odoo-title">
                    <Mail size={14} aria-hidden="true" />
                    <span>Outgoing Mail Server (SMTP)</span>
                  </div>
                  <div className="connect-modal__odoo-field">
                    <span className="connect-modal__odoo-key">SMTP Server:</span>
                    <span className="connect-modal__odoo-val">
                      <span>{smtp.host}</span>
                      <CopyButton value={smtp.host} label="SMTP server" />
                    </span>
                  </div>
                  <div className="connect-modal__odoo-field">
                    <span className="connect-modal__odoo-key">SMTP Port:</span>
                    <span className="connect-modal__odoo-val">
                      <span>{smtp.port}</span>
                      <CopyButton value={smtp.port} label="SMTP port" />
                    </span>
                  </div>
                  <div className="connect-modal__odoo-field">
                    <span className="connect-modal__odoo-key">Connection Security:</span>
                    <span className="connect-modal__odoo-val">None</span>
                  </div>
                  <div className="connect-modal__odoo-field">
                    <span className="connect-modal__odoo-key">Username:</span>
                    <span className="connect-modal__odoo-val">
                      <span>{effectiveUsername}</span>
                      <CopyButton value={effectiveUsername} label="username" />
                    </span>
                  </div>
                  <div className="connect-modal__odoo-field">
                    <span className="connect-modal__odoo-key">Password:</span>
                    <span className="connect-modal__odoo-val">
                      <span>{effectivePassword}</span>
                      {passwordInput.trim() && <CopyButton value={passwordInput.trim()} label="password" />}
                    </span>
                  </div>
                </div>

                <div className="connect-modal__odoo-box">
                  <div className="connect-modal__odoo-title">
                    <Server size={14} aria-hidden="true" />
                    <span>Incoming Mail Server (IMAP)</span>
                  </div>
                  <div className="connect-modal__odoo-field">
                    <span className="connect-modal__odoo-key">Server Type:</span>
                    <span className="connect-modal__odoo-val">IMAP Server</span>
                  </div>
                  <div className="connect-modal__odoo-field">
                    <span className="connect-modal__odoo-key">Server Name:</span>
                    <span className="connect-modal__odoo-val">
                      <span>{imap.host}</span>
                      <CopyButton value={imap.host} label="IMAP server" />
                    </span>
                  </div>
                  <div className="connect-modal__odoo-field">
                    <span className="connect-modal__odoo-key">Port:</span>
                    <span className="connect-modal__odoo-val">
                      <span>{imap.port}</span>
                      <CopyButton value={imap.port} label="IMAP port" />
                    </span>
                  </div>
                  <div className="connect-modal__odoo-field">
                    <span className="connect-modal__odoo-key">SSL/TLS:</span>
                    <span className="connect-modal__odoo-val">No (unchecked)</span>
                  </div>
                  <div className="connect-modal__odoo-field">
                    <span className="connect-modal__odoo-key">Username:</span>
                    <span className="connect-modal__odoo-val">
                      <span>{effectiveUsername}</span>
                      <CopyButton value={effectiveUsername} label="username" />
                    </span>
                  </div>
                  <div className="connect-modal__odoo-field">
                    <span className="connect-modal__odoo-key">Password:</span>
                    <span className="connect-modal__odoo-val">
                      <span>{effectivePassword}</span>
                      {passwordInput.trim() && <CopyButton value={passwordInput.trim()} label="password" />}
                    </span>
                  </div>
                </div>
              </div>
            ) : null}

            <div className="connect-modal__snippet-card">
              <div className="connect-modal__snippet-header">
                <span>{current.filename}</span>
                <CopyButton value={current.code} label="code snippet" />
              </div>
              <pre className="connect-modal__snippet-code">
                <code>{current.code}</code>
              </pre>
            </div>
          </div>
        </div>

        <footer className="ws-modal__footer">
          <button type="button" className="btn btn--primary" onClick={onClose}>
            Done
          </button>
        </footer>
      </div>
    </div>
  )
}
