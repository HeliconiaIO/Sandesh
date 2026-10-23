  /**
 * Settings view (§62).
 *
 * Full production-grade developer configuration:
 *   • Accounts & delivery routing management (create, edit credentials, configure addresses, remove, toggle catch-all)
 *   • Summary metrics (total accounts, catch-all status, identities, mailbox count)
 *   • Protocol endpoints reference (SMTP, IMAP, POP3, REST API) with live addresses, status badges, and CLI test commands
 *   • Visual appearance & theme selection (System, Dark, Light)
 *   • Grouped keyboard shortcuts reference
 *   • Top category tabs with deep-linking (?tab=...)
 */

import { useEffect, useState } from 'react'
import { Link, useSearchParams } from 'react-router-dom'
import {
  AtSign,
  Folder,
  Keyboard,
  Layers,
  Lock,
  Mail,
  Palette,
  Pencil,
  Plus,
  Radio,
  Shield,
  Terminal,
  Trash2,
  Unlock,
  User,
  UserPlus,
  Users,
  X,
} from 'lucide-react'

import { useAccounts, useActivateWorkspace, useInfo, useMailboxes, usePatchAccount, useWorkspaces } from '../hooks/queries'
import { useTheme, type ThemePreference } from '../hooks/useTheme'
import type { Account } from '../api/client'
import { CopyButton } from '../components/CopyButton'
import { AccountModal } from '../features/settings/AccountModal'
import { DeleteAccountDialog } from '../features/settings/DeleteAccountDialog'
import './SettingsRoute.css'

type SettingsTab = 'accounts' | 'protocols' | 'appearance' | 'shortcuts'

const THEMES: Array<{
  value: ThemePreference
  label: string
  hint: string
  previewClass: string
}> = [
  {
    value: 'system',
    label: 'System Theme',
    hint: 'Follows your operating system preferences automatically',
    previewClass: 'theme-preview--system',
  },
  {
    value: 'dark',
    label: 'Dark Theme',
    hint: 'Optimized for low-light environments and extended use',
    previewClass: 'theme-preview--dark',
  },
  {
    value: 'light',
    label: 'Light Theme',
    hint: 'High-contrast bright mode with crisp borders',
    previewClass: 'theme-preview--light',
  },
]

export function SettingsRoute() {
  const [searchParams, setSearchParams] = useSearchParams()
  const activeTab: SettingsTab =
    (searchParams.get('tab') as SettingsTab) || 'accounts'

  const setActiveTab = (tab: SettingsTab) => {
    setSearchParams(
      (prev) => {
        const next = new URLSearchParams(prev)
        if (tab === 'accounts') {
          next.delete('tab')
        } else {
          next.set('tab', tab)
        }
        return next
      },
      { replace: true },
    )
  }

  const { preference, setTheme } = useTheme()
  const accountsQuery = useAccounts()
  const mailboxesQuery = useMailboxes()
  const { data: info } = useInfo()
  const patchAccount = usePatchAccount()
  const workspacesQuery = useWorkspaces()
  const activateWorkspace = useActivateWorkspace()

  const [createModalOpen, setCreateModalOpen] = useState(false)
  const [editingAccount, setEditingAccount] = useState<Account | null>(null)
  const [deletingAccount, setDeletingAccount] = useState<Account | null>(null)

  const workspaces = workspacesQuery.data ?? []
  const activeWorkspace = workspaces.find((w) => w.is_active) ?? workspaces[0]

  const workspaceParam = searchParams.get('workspace')

  // Synchronize active workspace if URL parameter ?workspace=... is specified
  useEffect(() => {
    if (
      workspaceParam &&
      workspaces.length > 0 &&
      activeWorkspace &&
      activeWorkspace.id !== workspaceParam
    ) {
      const match = workspaces.find((w) => w.id === workspaceParam)
      if (match) {
        void activateWorkspace.mutate(match.id)
      }
    }
  }, [workspaceParam, workspaces, activeWorkspace?.id, activateWorkspace])

  // Map account_id -> input string for quick adding an address inline
  const [newAddresses, setNewAddresses] = useState<Record<string, string>>({})

  const accounts = accountsQuery.data?.items ?? []
  const allMailboxes = mailboxesQuery.data?.items ?? []

  // Summary statistics
  const totalAddresses = accounts.reduce((sum, acc) => sum + acc.addresses.length, 0)
  const catchAllAccount = accounts.find((acc) => acc.is_catch_all)

  const handleInlineAddAddress = (accountId: string) => {
    const raw = newAddresses[accountId]?.trim()
    if (!raw) return
    patchAccount.mutate(
      { id: accountId, add_addresses: [raw] },
      {
        onSuccess: () => {
          setNewAddresses((prev) => ({ ...prev, [accountId]: '' }))
        },
      },
    )
  }

  const handleInlineRemoveAddress = (accountId: string, address: string) => {
    patchAccount.mutate({ id: accountId, remove_addresses: [address] })
  }

  const handleToggleCatchAll = (account: Account) => {
    patchAccount.mutate({ id: account.id, is_catch_all: !account.is_catch_all })
  }

  const showAccounts = activeTab === 'accounts'
  const showProtocols = activeTab === 'protocols'
  const showAppearance = activeTab === 'appearance'
  const showShortcuts = activeTab === 'shortcuts'

  return (
    <div className="settings-page">
      <div className="settings-page__inner">
        {/* ===================================================================
            Page Header & Actions
            =================================================================== */}
        <header className="settings-header">
          <div className="settings-header__titles">
            <h1 className="settings-header__title">Settings</h1>
            <p className="settings-header__lede">
              Manage accounts, delivery routing, protocol endpoints, and workspace preferences.
            </p>
          </div>
          <button
            type="button"
            className="button button--primary"
            onClick={() => setCreateModalOpen(true)}
          >
            <UserPlus size={15} aria-hidden="true" />
            <span>Add Account</span>
          </button>
        </header>

        {/* ===================================================================
            Navigation Tabs
            =================================================================== */}
        <nav className="settings-tabs" role="tablist" aria-label="Settings categories">

          <button
            type="button"
            role="tab"
            aria-selected={activeTab === 'accounts'}
            className={`settings-tab${activeTab === 'accounts' ? ' settings-tab--active' : ''}`}
            onClick={() => setActiveTab('accounts')}
          >
            <Users size={14} aria-hidden="true" />
            <span>Accounts &amp; Routing</span>
            {accounts.length > 0 && (
              <span className="settings-tab__badge">{accounts.length}</span>
            )}
          </button>

          <button
            type="button"
            role="tab"
            aria-selected={activeTab === 'protocols'}
            className={`settings-tab${activeTab === 'protocols' ? ' settings-tab--active' : ''}`}
            onClick={() => setActiveTab('protocols')}
          >
            <Radio size={14} aria-hidden="true" />
            <span>Protocols &amp; Endpoints</span>
          </button>

          <button
            type="button"
            role="tab"
            aria-selected={activeTab === 'appearance'}
            className={`settings-tab${activeTab === 'appearance' ? ' settings-tab--active' : ''}`}
            onClick={() => setActiveTab('appearance')}
          >
            <Palette size={14} aria-hidden="true" />
            <span>Appearance</span>
          </button>

          <button
            type="button"
            role="tab"
            aria-selected={activeTab === 'shortcuts'}
            className={`settings-tab${activeTab === 'shortcuts' ? ' settings-tab--active' : ''}`}
            onClick={() => setActiveTab('shortcuts')}
          >
            <Keyboard size={14} aria-hidden="true" />
            <span>Shortcuts</span>
          </button>
        </nav>

        {/* ===================================================================
            Overview Metrics Banner (Shown in 'all' or 'accounts')
            =================================================================== */}
        {showAccounts && (
          <div className="settings-metrics">
            <div className="settings-metric-card">
              <div className="settings-metric-card__icon" aria-hidden="true">
                <Users size={18} />
              </div>
              <div className="settings-metric-card__content">
                <span className="settings-metric-card__label">Configured Accounts</span>
                <span className="settings-metric-card__value">
                  {accountsQuery.isPending ? '...' : accounts.length}
                </span>
              </div>
            </div>

            <div className="settings-metric-card">
              <div className="settings-metric-card__icon" aria-hidden="true">
                <Shield size={18} />
              </div>
              <div className="settings-metric-card__content">
                <span className="settings-metric-card__label">Catch-All Target</span>
                <span className="settings-metric-card__value" title={catchAllAccount?.username}>
                  {catchAllAccount ? catchAllAccount.username : 'None (Strict)'}
                </span>
              </div>
            </div>

            <div className="settings-metric-card">
              <div className="settings-metric-card__icon" aria-hidden="true">
                <AtSign size={18} />
              </div>
              <div className="settings-metric-card__content">
                <span className="settings-metric-card__label">Bound Addresses</span>
                <span className="settings-metric-card__value">{totalAddresses}</span>
              </div>
            </div>

            <div className="settings-metric-card">
              <div className="settings-metric-card__icon" aria-hidden="true">
                <Folder size={18} />
              </div>
              <div className="settings-metric-card__content">
                <span className="settings-metric-card__label">Total Mailboxes</span>
                <span className="settings-metric-card__value">{allMailboxes.length}</span>
              </div>
            </div>
          </div>
        )}

        {/* ===================================================================
            Section 1: Accounts & Delivery Routing
            =================================================================== */}
        {showAccounts && (
          <>
            <div className="settings-auth-banner">
              <div className="settings-auth-banner__info">
                <div className="settings-auth-banner__title-row">
                  <Shield size={16} aria-hidden="true" />
                  <span className="settings-auth-banner__title">
                    Active Workspace:
                  </span>
                  {workspaces.length > 1 ? (
                    <select
                      className="settings-workspace-select"
                      value={activeWorkspace?.id || 'default'}
                      onChange={(e) => {
                        const targetId = e.target.value
                        void activateWorkspace.mutate(targetId)
                        setSearchParams(
                          (prev) => {
                            const next = new URLSearchParams(prev)
                            next.set('workspace', targetId)
                            return next
                          },
                          { replace: true },
                        )
                      }}
                      title="Switch workspace to view and manage its accounts"
                      aria-label="Switch active workspace"
                    >
                      {workspaces.map((w) => (
                        <option key={w.id} value={w.id}>
                          {w.name} ({w.id}){w.is_active ? ' • Active' : ''}
                        </option>
                      ))}
                    </select>
                  ) : (
                    <strong>{activeWorkspace?.name || 'default'}</strong>
                  )}
                  {activeWorkspace?.auth_required ? (
                    <span className="ws-auth-badge ws-auth-badge--required" title="Authentication is required for this workspace">
                      <Lock size={10} aria-hidden="true" />
                      <span>Auth Required</span>
                    </span>
                  ) : (
                    <span className="ws-auth-badge ws-auth-badge--optional" title="Permissive developer mode: any or no password accepted">
                      <Unlock size={10} aria-hidden="true" />
                      <span>Permissive Dev Mode (Open)</span>
                    </span>
                  )}
                </div>
                <p className="settings-auth-banner__desc">
                  Accounts configured here provide credentials for <strong>SMTP (port 1025)</strong>, <strong>IMAP (port 1143)</strong>, and <strong>POP3 (port 1110)</strong>. Click <strong>Edit</strong> on any account to set or reset its password. Connect using scoped username <code>{activeWorkspace?.id || 'default'}/&lt;username&gt;</code>.
                </p>
              </div>
              <Link to="/workspaces" className="button button--ghost button--sm settings-auth-banner__link">
                <Layers size={13} aria-hidden="true" />
                <span>Manage Workspaces</span>
              </Link>
            </div>

            <section className="settings-section" aria-labelledby="section-accounts-title">
              <div className="settings-section__header">
                <div className="settings-section__titles">
                  <h2 id="section-accounts-title" className="settings-section__title">
                    <Users size={16} aria-hidden="true" />
                    <span>Accounts &amp; Delivery Routing</span>
                  </h2>
                  <p className="settings-section__lede">
                    Accounts partition mailboxes and provide login credentials for SMTP, IMAP, and POP3. Incoming mail
                    delivers to the matching account address, or falls back to the Catch-All inbox.
                  </p>
                </div>
              </div>

              {accountsQuery.isPending ? (
                <div className="settings-accounts__loading">Loading accounts...</div>
              ) : accounts.length === 0 ? (
                <div className="settings-accounts__empty">
                  <p>No accounts configured yet.</p>
                  <button
                    type="button"
                    className="button button--primary"
                    onClick={() => setCreateModalOpen(true)}
                  >
                    <UserPlus size={14} aria-hidden="true" />
                    <span>Create First Account</span>
                  </button>
                </div>
              ) : (
                <div className="settings-accounts__grid">
                  {accounts.map((acc) => {
                    const accountMailboxes = allMailboxes.filter(
                      (m) => m.account_id === acc.id,
                    )
                    const isOnlyAccount = accounts.length === 1

                    return (
                      <div
                        key={acc.id}
                        className={`settings-card ${acc.is_catch_all ? 'settings-card--catchall' : ''}`}
                      >
                        <div className="settings-card__header">
                          <div className="settings-card__ident">
                            <span className="settings-card__avatar" aria-hidden="true">
                              {acc.is_catch_all ? <Shield size={18} /> : <User size={18} />}
                            </span>
                            <div>
                              <div className="settings-card__username-row">
                                <span className="settings-card__username">{acc.username}</span>
                                {acc.is_catch_all ? (
                                  <span className="settings-badge settings-badge--accent">
                                    Catch-All
                                  </span>
                                ) : (
                                  <span className="settings-badge">User</span>
                                )}
                                {acc.has_password ? (
                                  <span className="settings-badge settings-badge--success" title="Password configured for SMTP/IMAP/POP3">
                                    ✓ Password Set
                                  </span>
                                ) : (
                                  <span className="settings-badge" title="No password set (permissive dev mode)">
                                    No Password
                                  </span>
                                )}
                              </div>
                              <div className="settings-card__id">
                                <span>ID:</span>
                                <code>{acc.id}</code>
                                <CopyButton value={acc.id} label="Account ID" />
                              </div>
                              <div className="settings-card__auth-row">
                                <span>Login:</span>
                                <code>{activeWorkspace ? `${activeWorkspace.id}/${acc.username}` : acc.username}</code>
                                <CopyButton
                                  value={activeWorkspace ? `${activeWorkspace.id}/${acc.username}` : acc.username}
                                  label="scoped username"
                                />
                              </div>
                            </div>
                          </div>

                        <div className="settings-card__actions">
                          <button
                            type="button"
                            className="button button--ghost button--sm"
                            onClick={() => setEditingAccount(acc)}
                            title="Edit account credentials & properties"
                            aria-label={`Edit ${acc.username}`}
                          >
                            <Pencil size={13} aria-hidden="true" />
                            <span>Edit</span>
                          </button>
                          {!isOnlyAccount && (
                            <button
                              type="button"
                              className="button button--danger button--sm"
                              onClick={() => setDeletingAccount(acc)}
                              title="Delete account"
                              aria-label={`Delete ${acc.username}`}
                            >
                              <Trash2 size={13} aria-hidden="true" />
                              <span>Delete</span>
                            </button>
                          )}
                        </div>
                      </div>

                      <div className="settings-card__body">
                        {/* Left Column: Routing & Folders */}
                        <div className="settings-card__column">
                          <div className="settings-card__rule-box">
                            <label className="settings-card__catchall-toggle">
                              <input
                                type="checkbox"
                                checked={acc.is_catch_all}
                                onChange={() => handleToggleCatchAll(acc)}
                                disabled={patchAccount.isPending}
                              />
                              <div>
                                <span className="settings-card__catchall-title">
                                  Catch-All Fallback Destination
                                </span>
                                <div className="settings-card__catchall-desc">
                                  Routes any incoming email with an unmatched recipient address into
                                  this account's INBOX.
                                </div>
                              </div>
                            </label>
                          </div>

                          <div className="settings-card__folders">
                            <div className="settings-card__folders-header">
                              <span className="settings-card__label">
                                <Folder size={12} aria-hidden="true" />
                                <span>Folders ({accountMailboxes.length})</span>
                              </span>
                            </div>

                            <div className="settings-card__folder-chips">
                              {accountMailboxes.length === 0 ? (
                                <span className="settings-card__no-chips">No folders yet</span>
                              ) : (
                                accountMailboxes.map((m) => (
                                  <span key={m.id} className="settings-folder-pill">
                                    <Folder size={11} aria-hidden="true" />
                                    <span>{m.name}</span>
                                  </span>
                                ))
                              )}
                            </div>
                          </div>
                        </div>

                        {/* Right Column: Bound Email Addresses */}
                        <div className="settings-card__column settings-card__column--bordered">
                          <div className="settings-card__field">
                            <label className="settings-card__label">
                              <AtSign size={13} aria-hidden="true" />
                              <span>Bound Email Addresses</span>
                            </label>

                            <div className="settings-card__chips">
                              {acc.addresses.length === 0 ? (
                                <span className="settings-card__no-chips">
                                  {acc.is_catch_all
                                    ? 'Receives all unmatched incoming recipient addresses (Catch-All).'
                                    : 'No explicit addresses bound to this account.'}
                                </span>
                              ) : (
                                acc.addresses.map((addr) => (
                                  <span key={addr} className="settings-chip">
                                    <code>{addr}</code>
                                    <button
                                      type="button"
                                      className="settings-chip__remove"
                                      onClick={() => handleInlineRemoveAddress(acc.id, addr)}
                                      title={`Remove ${addr}`}
                                      aria-label={`Remove ${addr}`}
                                    >
                                      <X size={11} />
                                    </button>
                                  </span>
                                ))
                              )}
                            </div>

                            {/* Inline add address bar */}
                            <div className="settings-card__add-address-row">
                              <input
                                type="email"
                                className="settings-card__add-address-input"
                                placeholder="Add address (e.g. user@test.local)..."
                                value={newAddresses[acc.id] ?? ''}
                                onChange={(e) =>
                                  setNewAddresses((prev) => ({
                                    ...prev,
                                    [acc.id]: e.target.value,
                                  }))
                                }
                                onKeyDown={(e) => {
                                  if (e.key === 'Enter') {
                                    e.preventDefault()
                                    handleInlineAddAddress(acc.id)
                                  }
                                }}
                              />
                              <button
                                type="button"
                                className="button button--sm"
                                onClick={() => handleInlineAddAddress(acc.id)}
                                disabled={!newAddresses[acc.id]?.trim() || patchAccount.isPending}
                              >
                                <Plus size={12} aria-hidden="true" />
                                <span>Add</span>
                              </button>
                            </div>
                          </div>
                        </div>
                      </div>
                    </div>
                  )
                })}
              </div>
            )}
          </section>
        </>
      )}

        {/* ===================================================================
            Section 2: Server Protocol Endpoints Reference (Balanced 2x2 Grid)
            =================================================================== */}
        {showProtocols && (
          <section className="settings-section" aria-labelledby="section-protocols-title">
            <div className="settings-section__titles">
              <h2 id="section-protocols-title" className="settings-section__title">
                <Radio size={16} aria-hidden="true" />
                <span>Protocol Endpoints &amp; Connections</span>
              </h2>
              <p className="settings-section__lede">
                Sandesh exposes standard email protocol listeners on localhost. Point your
                applications, testing suites, or real mail clients directly at these ports.
              </p>
            </div>

            <div className="settings-endpoints__grid">
              {/* SMTP */}
              <div className="settings-endpoint-card">
                <div className="settings-endpoint-card__header">
                  <div className="settings-endpoint-card__tag-wrap">
                    <span className="settings-endpoint-card__tag settings-endpoint-card__tag--smtp">
                      SMTP
                    </span>
                  </div>
                  <span className="settings-endpoint-card__status">
                    <span className="settings-endpoint-card__status-dot" />
                    Accepting Inbound
                  </span>
                </div>

                <div className="settings-endpoint-card__value">
                  <code>{info?.smtp_address ?? '127.0.0.1:1025'}</code>
                  <CopyButton
                    value={info?.smtp_address ?? '127.0.0.1:1025'}
                    label="SMTP listener address"
                  />
                </div>

                <p className="settings-endpoint-card__desc">
                  Deliver test emails from your apps, backends, or frameworks without credentials or TLS.
                </p>

                <div className="settings-endpoint-card__hint">
                  <Terminal size={12} aria-hidden="true" />
                  <code>nc {info?.smtp_address?.replace(':', ' ') ?? '127.0.0.1 1025'}</code>
                </div>
              </div>

              {/* IMAP */}
              <div className="settings-endpoint-card">
                <div className="settings-endpoint-card__header">
                  <div className="settings-endpoint-card__tag-wrap">
                    <span className="settings-endpoint-card__tag settings-endpoint-card__tag--imap">
                      IMAP
                    </span>
                  </div>
                  <span className="settings-endpoint-card__status">
                    <span className="settings-endpoint-card__status-dot" />
                    Mailbox Client
                  </span>
                </div>

                <div className="settings-endpoint-card__value">
                  <code>{info?.imap_address ?? '127.0.0.1:1143'}</code>
                  <CopyButton
                    value={info?.imap_address ?? '127.0.0.1:1143'}
                    label="IMAP listener address"
                  />
                </div>

                <p className="settings-endpoint-card__desc">
                  Connect mail clients (Thunderbird, Apple Mail, mutt, mbsync). Login with account credentials.
                </p>

                <div className="settings-endpoint-card__hint">
                  <Terminal size={12} aria-hidden="true" />
                  <code>nc {info?.imap_address?.replace(':', ' ') ?? '127.0.0.1 1143'}</code>
                </div>
              </div>

              {/* POP3 */}
              <div className="settings-endpoint-card">
                <div className="settings-endpoint-card__header">
                  <div className="settings-endpoint-card__tag-wrap">
                    <span className="settings-endpoint-card__tag settings-endpoint-card__tag--pop3">
                      POP3
                    </span>
                  </div>
                  <span className="settings-endpoint-card__status">
                    <span className="settings-endpoint-card__status-dot" />
                    Inbox Retrieval
                  </span>
                </div>

                <div className="settings-endpoint-card__value">
                  <code>{info?.pop3_address ?? '127.0.0.1:1110'}</code>
                  <CopyButton
                    value={info?.pop3_address ?? '127.0.0.1:1110'}
                    label="POP3 listener address"
                  />
                </div>

                <p className="settings-endpoint-card__desc">
                  Retrieve inbox messages via POP3 protocol (RFC 1939) using account credentials.
                </p>

                <div className="settings-endpoint-card__hint">
                  <Terminal size={12} aria-hidden="true" />
                  <code>nc {info?.pop3_address?.replace(':', ' ') ?? '127.0.0.1 1110'}</code>
                </div>
              </div>

              {/* REST API */}
              <div className="settings-endpoint-card">
                <div className="settings-endpoint-card__header">
                  <div className="settings-endpoint-card__tag-wrap">
                    <span className="settings-endpoint-card__tag settings-endpoint-card__tag--api">
                      REST API
                    </span>
                  </div>
                  <span className="settings-endpoint-card__status">
                    <span className="settings-endpoint-card__status-dot" />
                    HTTP &amp; WebSocket
                  </span>
                </div>

                <div className="settings-endpoint-card__value">
                  <code>
                    {info?.http_address
                      ? `${info.http_address}/api/v1`
                      : 'http://127.0.0.1:8080/api/v1'}
                  </code>
                  <CopyButton
                    value={
                      info?.http_address
                        ? `${info.http_address}/api/v1`
                        : 'http://127.0.0.1:8080/api/v1'
                    }
                    label="REST API URL"
                  />
                </div>

                <p className="settings-endpoint-card__desc">
                  Automate message queries, assertions, account/mailbox mutations, and subscribe to live events.
                </p>

                <div className="settings-endpoint-card__hint">
                  <Terminal size={12} aria-hidden="true" />
                  <code>curl {info?.http_address ?? 'http://127.0.0.1:8080'}/api/v1/health</code>
                </div>
              </div>
            </div>
          </section>
        )}

        {/* ===================================================================
            Section 3: Appearance & Visual Theme Selection
            =================================================================== */}
        {showAppearance && (
          <section className="settings-section" aria-labelledby="section-theme-title">
            <div className="settings-section__titles">
              <h2 id="section-theme-title" className="settings-section__title">
                <Palette size={16} aria-hidden="true" />
                <span>Appearance &amp; Theme</span>
              </h2>
              <p className="settings-section__lede">
                Customize how Sandesh looks on your screen. The choice persists across browser sessions.
              </p>
            </div>

            <fieldset className="settings__field">
              <legend className="sr-only">Theme preferences</legend>
              <div className="settings-theme__grid">
                {THEMES.map((theme) => {
                  const isSelected = preference === theme.value
                  return (
                    <button
                      key={theme.value}
                      type="button"
                      className={`settings-theme-card${isSelected ? ' settings-theme-card--active' : ''}`}
                      onClick={() => setTheme(theme.value)}
                      aria-pressed={isSelected}
                    >
                      <div className={`settings-theme-card__preview ${theme.previewClass}`}>
                        <div className="settings-theme-card__preview-header">
                          <span className="settings-theme-card__preview-dot" />
                        </div>
                        <div className="settings-theme-card__preview-body">
                          <div className="settings-theme-card__preview-sidebar" />
                          <div className="settings-theme-card__preview-content">
                            <div className="settings-theme-card__preview-bar" style={{ width: '80%' }} />
                            <div className="settings-theme-card__preview-bar" style={{ width: '60%' }} />
                            <div className="settings-theme-card__preview-bar" style={{ width: '40%' }} />
                          </div>
                        </div>
                      </div>

                      <div className="settings-theme-card__meta">
                        <div className="settings-theme-card__info">
                          <span className="settings-theme-card__label">{theme.label}</span>
                          <span className="settings-theme-card__hint">{theme.hint}</span>
                        </div>
                        <div className="settings-theme-card__radio-indicator" aria-hidden="true">
                          {isSelected && <span className="settings-theme-card__radio-dot" />}
                        </div>
                      </div>
                    </button>
                  )
                })}
              </div>
            </fieldset>
          </section>
        )}

        {/* ===================================================================
            Section 4: Grouped Keyboard Shortcuts Reference
            =================================================================== */}
        {showShortcuts && (
          <section className="settings-section" aria-labelledby="section-shortcuts-title">
            <div className="settings-section__titles">
              <h2 id="section-shortcuts-title" className="settings-section__title">
                <Keyboard size={16} aria-hidden="true" />
                <span>Keyboard Shortcuts</span>
              </h2>
              <p className="settings-section__lede">
                Speed up your mail inspection workflow with built-in hotkeys.
              </p>
            </div>

            <div className="settings-shortcuts__grid">
              <div className="settings-shortcuts-card">
                <h3 className="settings-shortcuts-card__title">
                  <Mail size={14} aria-hidden="true" />
                  <span>Message List Navigation</span>
                </h3>
                <dl className="settings-shortcuts-card__list">
                  <div className="settings-shortcuts-card__item">
                    <dt>
                      <kbd>↑</kbd> <kbd>↓</kbd> or <kbd>j</kbd> <kbd>k</kbd>
                    </dt>
                    <dd>Move selection between messages</dd>
                  </div>
                  <div className="settings-shortcuts-card__item">
                    <dt>
                      <kbd>Enter</kbd>
                    </dt>
                    <dd>Open selected message</dd>
                  </div>
                </dl>
              </div>

              <div className="settings-shortcuts-card">
                <h3 className="settings-shortcuts-card__title">
                  <Terminal size={14} aria-hidden="true" />
                  <span>Global Controls &amp; Search</span>
                </h3>
                <dl className="settings-shortcuts-card__list">
                  <div className="settings-shortcuts-card__item">
                    <dt>
                      <kbd>/</kbd> or <kbd>Ctrl</kbd>+<kbd>K</kbd>
                    </dt>
                    <dd>Focus global search input</dd>
                  </div>
                  <div className="settings-shortcuts-card__item">
                    <dt>
                      <kbd>Esc</kbd>
                    </dt>
                    <dd>Close viewer, modals, or clear multi-select</dd>
                  </div>
                </dl>
              </div>

              <div className="settings-shortcuts-card">
                <h3 className="settings-shortcuts-card__title">
                  <Layers size={14} aria-hidden="true" />
                  <span>Message Inspector</span>
                </h3>
                <dl className="settings-shortcuts-card__list">
                  <div className="settings-shortcuts-card__item">
                    <dt>
                      <kbd>←</kbd> <kbd>→</kbd>
                    </dt>
                    <dd>Cycle tabs (Preview, Headers, MIME, Raw)</dd>
                  </div>
                </dl>
              </div>
            </div>
          </section>
        )}
      </div>

      {/* Modals */}
      {createModalOpen && (
        <AccountModal onClose={() => setCreateModalOpen(false)} />
      )}
      {editingAccount && (
        <AccountModal
          account={editingAccount}
          onClose={() => setEditingAccount(null)}
        />
      )}
      {deletingAccount && (
        <DeleteAccountDialog
          account={deletingAccount}
          onClose={() => setDeletingAccount(null)}
        />
      )}
    </div>
  )
}
