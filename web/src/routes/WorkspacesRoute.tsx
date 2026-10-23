import { useMemo, useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import {
  AlertTriangle,
  Check,
  Database,
  ExternalLink,
  Eye,
  EyeOff,
  HardDrive,
  KeyRound,
  Layers,
  Lock,
  Mail,
  Plug,
  Plus,
  RefreshCw,
  Sparkles,
  Trash2,
  Unlock,
  Users,
  X,
} from 'lucide-react'

import {
  useAccounts,
  useActivateWorkspace,
  useCreateWorkspace,
  useDeleteWorkspace,
  usePurgeWorkspace,
  useUpdateWorkspace,
  useWorkspaces,
} from '../hooks/queries'
import { useDialog } from '../context/DialogContext'
import type { Account, Workspace } from '../api/client'
import { CopyButton } from '../components/CopyButton'
import { LoadingState } from '../components/States'
import { ConnectClientModal } from '../features/workspaces/ConnectClientModal'
import { AccountModal } from '../features/settings/AccountModal'
import './WorkspacesRoute.css'

function formatBytes(bytes: number): string {
  if (bytes <= 0) return '0 B'
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}

function formatDate(timestamp: string): string {
  try {
    const d = new Date(timestamp)
    return d.toLocaleDateString(undefined, {
      year: 'numeric',
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
      minute: '2-digit',
    })
  } catch {
    return timestamp
  }
}

function slugify(text: string): string {
  return text
    .toLowerCase()
    .trim()
    .replace(/[^a-z0-9_-]+/g, '-')
    .replace(/^-+|-+$/g, '')
}

export function WorkspacesRoute() {
  const workspacesQuery = useWorkspaces()
  const activateWorkspace = useActivateWorkspace()
  const createWorkspace = useCreateWorkspace()
  const updateWorkspace = useUpdateWorkspace()
  const purgeWorkspace = usePurgeWorkspace()
  const deleteWorkspace = useDeleteWorkspace()
  const dialog = useDialog()

  // Creation modal state
  const [createModalOpen, setCreateModalOpen] = useState(false)
  const [newWsName, setNewWsName] = useState('')
  const [newWsId, setNewWsId] = useState('')
  const [newWsDesc, setNewWsDesc] = useState('')
  const [newWsSetActive, setNewWsSetActive] = useState(true)
  const [cloneAccounts, setCloneAccounts] = useState(true)
  const [newWsAuthRequired, setNewWsAuthRequired] = useState(false)
  const [newWsPassword, setNewWsPassword] = useState('')
  const [showNewWsPassword, setShowNewWsPassword] = useState(false)
  const [initialUsername, setInitialUsername] = useState('dev')
  const [initialAddresses, setInitialAddresses] = useState('')
  const [initialCatchAll, setInitialCatchAll] = useState(true)
  const [slugManuallyEdited, setSlugManuallyEdited] = useState(false)
  const [createError, setCreateError] = useState<string | null>(null)

  // Connect client modal state
  const [connectTarget, setConnectTarget] = useState<Workspace | null>(null)

  // Purge modal state
  const [purgeTarget, setPurgeTarget] = useState<Workspace | null>(null)
  const [purgeMode, setPurgeMode] = useState<'mail_and_traces' | 'factory_reset'>('mail_and_traces')
  const [clearTraces, setClearTraces] = useState(true)
  const [clearWebhooks, setClearWebhooks] = useState(false)
  const [purgeNotice, setPurgeNotice] = useState<string | null>(null)
  const [actionError, setActionError] = useState<string | null>(null)

  const navigate = useNavigate()
  const [activatingWorkspaceId, setActivatingWorkspaceId] = useState<string | null>(null)

  // Direct Account & Password manager states
  const accountsQuery = useAccounts()
  const [createAccountOpen, setCreateAccountOpen] = useState(false)
  const [editingAccount, setEditingAccount] = useState<Account | null>(null)

  const handleOpenAccounts = async (ws: Workspace) => {
    if (!ws.is_active) {
      try {
        setActivatingWorkspaceId(ws.id)
        await activateWorkspace.mutateAsync(ws.id)
      } catch (err) {
        console.error('Failed to activate workspace before opening accounts:', err)
      } finally {
        setActivatingWorkspaceId(null)
      }
    }
    navigate(`/settings?tab=accounts&workspace=${encodeURIComponent(ws.id)}`)
  }

  const accounts = useMemo(
    () => accountsQuery.data?.items ?? [],
    [accountsQuery.data],
  )

  const workspaces = workspacesQuery.data ?? []
  const activeWorkspace = useMemo(
    () => workspaces.find((w) => w.is_active) ?? workspaces[0],
    [workspaces],
  )

  // Global aggregate metrics
  const totalMessages = useMemo(
    () => workspaces.reduce((acc, ws) => acc + ws.message_count, 0),
    [workspaces],
  )
  const totalDiskBytes = useMemo(
    () => workspaces.reduce((acc, ws) => acc + ws.size_bytes, 0),
    [workspaces],
  )

  const handleNameChange = (name: string) => {
    setNewWsName(name)
    if (!slugManuallyEdited) {
      setNewWsId(slugify(name))
    }
  }

  const handleCreateSubmit = async (e: React.FormEvent) => {
    e.preventDefault()
    setCreateError(null)

    const name = newWsName.trim()
    const id = newWsId.trim() || undefined
    const description = newWsDesc.trim() || undefined

    if (!name) {
      setCreateError('Workspace name is required.')
      return
    }

    try {
      const addresses = initialAddresses
        .split(',')
        .map((a) => a.trim())
        .filter(Boolean)

      await createWorkspace.mutateAsync({
        name,
        id,
        description,
        clone_from: cloneAccounts && activeWorkspace ? activeWorkspace.id : undefined,
        password: newWsPassword.trim() || undefined,
        auth_required: newWsAuthRequired,
        initial_account: !cloneAccounts
          ? {
            username: initialUsername.trim() || 'dev',
            addresses: addresses.length > 0 ? addresses : undefined,
            is_catch_all: initialCatchAll,
          }
          : undefined,
        set_active: newWsSetActive,
      })

      // Reset & close
      setCreateModalOpen(false)
      setNewWsName('')
      setNewWsId('')
      setNewWsDesc('')
      setNewWsSetActive(true)
      setCloneAccounts(true)
      setNewWsAuthRequired(false)
      setNewWsPassword('')
      setInitialUsername('dev')
      setInitialAddresses('')
      setInitialCatchAll(true)
      setSlugManuallyEdited(false)
    } catch (err: unknown) {
      const msg = err instanceof Error ? err.message : 'Failed to create workspace'
      setCreateError(msg)
    }
  }

  const handleToggleAuth = async (ws: Workspace) => {
    setActionError(null)
    setPurgeNotice(null)
    try {
      await updateWorkspace.mutateAsync({
        id: ws.id,
        auth_required: !ws.auth_required,
      })
      setPurgeNotice(
        !ws.auth_required
          ? `Authentication is now required for "${ws.name}". Clients must use username & password.`
          : `Authentication requirement disabled for "${ws.name}". Workspace is now in permissive dev mode.`
      )
    } catch (err: unknown) {
      const msg = err instanceof Error ? err.message : 'Failed to update authentication requirement'
      setActionError(msg)
    }
  }

  const handleActivate = async (id: string) => {
    setActionError(null)
    setPurgeNotice(null)
    try {
      await activateWorkspace.mutateAsync(id)
    } catch (err: unknown) {
      const msg = err instanceof Error ? err.message : 'Failed to switch workspace'
      setActionError(msg)
    }
  }

  const handleOpenPurge = (ws: Workspace, mode: 'mail_and_traces' | 'factory_reset' = 'mail_and_traces') => {
    setPurgeTarget(ws)
    setPurgeMode(mode)
    setClearTraces(true)
    setClearWebhooks(false)
  }

  const handleExecutePurge = async () => {
    if (!purgeTarget) return

    setActionError(null)
    try {
      const res = await purgeWorkspace.mutateAsync({
        id: purgeTarget.id,
        mode: purgeMode,
        clear_traces: clearTraces,
        clear_webhooks: clearWebhooks,
      })

      setPurgeNotice(
        `Workspace "${purgeTarget.name}" purged: ${res.deleted_messages} messages deleted, ${formatBytes(
          res.reclaimed_bytes,
        )} reclaimed.`,
      )
      setPurgeTarget(null)
    } catch (err: unknown) {
      const msg = err instanceof Error ? err.message : 'Failed to purge workspace'
      setActionError(msg)
    }
  }

  const handleDelete = async (ws: Workspace) => {
    if (ws.id === 'default') {
      await dialog.alert({
        title: 'Cannot Delete Default Workspace',
        message: 'The "default" workspace cannot be deleted as it serves as the system baseline.',
        variant: 'info',
      })
      return
    }

    const confirmed = await dialog.confirm({
      title: `Delete Workspace "${ws.name}"?`,
      message: (
        <div>
          <p>
            Are you sure you want to delete <strong>{ws.name}</strong> (<code>{ws.id}</code>)?
          </p>
          <p style={{ marginTop: '8px', color: 'var(--error)' }}>
            This will permanently erase its database file (<code>{ws.database_path}</code>) and all messages, accounts, and traces stored in it.
          </p>
        </div>
      ),
      confirmText: 'Delete Permanently',
      cancelText: 'Cancel',
      variant: 'danger',
    })

    if (!confirmed) return

    setActionError(null)
    try {
      await deleteWorkspace.mutateAsync(ws.id)
    } catch (err: unknown) {
      const msg = err instanceof Error ? err.message : 'Failed to delete workspace'
      setActionError(msg)
    }
  }

  if (workspacesQuery.isLoading && !workspaces.length) {
    return <LoadingState label="Loading workspaces..." />
  }

  return (
    <div className="workspaces-view">
      <header className="workspaces-bar">
        <div className="workspaces-bar__titles">
          <h1 className="workspaces-bar__title">
            <Layers size={18} aria-hidden="true" />
            <span>Workspaces & Databases</span>
          </h1>
          <p className="workspaces-bar__subtitle">
            Isolated SQLite databases for multi-project development, testing pipelines, and clean slates.
            Switching active workspace immediately swaps all data without restarting the server.
          </p>
        </div>

        <div className="workspaces-bar__actions">
          <button
            type="button"
            className="btn btn--primary"
            onClick={() => setCreateModalOpen(true)}
          >
            <Plus size={14} aria-hidden="true" />
            <span>New Workspace</span>
          </button>
        </div>
      </header>

      <div className="workspaces-content">
        {purgeNotice && (
          <div className="ws-alert ws-alert--success">
            <Check size={16} aria-hidden="true" />
            <span>{purgeNotice}</span>
          </div>
        )}

        {actionError && (
          <div className="ws-alert ws-alert--warn">
            <AlertTriangle size={16} aria-hidden="true" />
            <span>{actionError}</span>
          </div>
        )}

        {/* Aggregate metrics */}
        <section className="workspaces-metrics" aria-label="Workspaces summary metrics">
          <div className="workspaces-metric-card">
            <span className="workspaces-metric-card__label">Workspaces</span>
            <span className="workspaces-metric-card__value">{workspaces.length}</span>
            <span className="workspaces-metric-card__meta">Active: {activeWorkspace?.name ?? 'None'}</span>
          </div>

          <div className="workspaces-metric-card">
            <span className="workspaces-metric-card__label">Active Workspace</span>
            <span className="workspaces-metric-card__value">{activeWorkspace?.name ?? 'default'}</span>
            <span className="workspaces-metric-card__meta">ID: {activeWorkspace?.id ?? 'default'}</span>
          </div>

          <div className="workspaces-metric-card">
            <span className="workspaces-metric-card__label">Total Messages</span>
            <span className="workspaces-metric-card__value">{totalMessages}</span>
            <span className="workspaces-metric-card__meta">Across all databases</span>
          </div>

          <div className="workspaces-metric-card">
            <span className="workspaces-metric-card__label">Total Disk Size</span>
            <span className="workspaces-metric-card__value">{formatBytes(totalDiskBytes)}</span>
            <span className="workspaces-metric-card__meta">SQLite + WAL files</span>
          </div>
        </section>

        {/* Active workspace hero card */}
        {activeWorkspace && (
          <section className="active-ws-card" aria-label="Currently active workspace">
            <div className="active-ws-card__header">
              <div>
                <div className="active-ws-card__title-row">
                  <h2 className="active-ws-card__name">{activeWorkspace.name}</h2>
                  <span className="active-ws-card__slug">{activeWorkspace.id}</span>
                  <span className="active-ws-card__badge">
                    <span className="active-ws-card__badge-dot" />
                    <span>Currently Active</span>
                  </span>
                  {activeWorkspace.auth_required ? (
                    <span className="ws-auth-badge ws-auth-badge--required" title="Authentication strictly required for SMTP/IMAP/POP3">
                      <Lock size={10} aria-hidden="true" />
                      <span>Auth Required</span>
                    </span>
                  ) : (
                    <span className="ws-auth-badge ws-auth-badge--optional" title="Permissive developer mode: Zero authentication friction">
                      <Unlock size={10} aria-hidden="true" />
                      <span>No Auth (Dev Mode)</span>
                    </span>
                  )}
                </div>
                {activeWorkspace.description && (
                  <p className="active-ws-card__desc">{activeWorkspace.description}</p>
                )}
              </div>
            </div>

            <div className="active-ws-card__details">
              <div className="active-ws-card__detail-item">
                <span className="active-ws-card__detail-label">Database File</span>
                <span className="active-ws-card__detail-value">
                  <span>{activeWorkspace.database_path}</span>
                  <CopyButton value={activeWorkspace.database_path} label="database path" />
                </span>
              </div>

              <div className="active-ws-card__detail-item">
                <span className="active-ws-card__detail-label">Messages</span>
                <span className="active-ws-card__detail-value">
                  <Mail size={13} aria-hidden="true" />
                  <span>{activeWorkspace.message_count} stored</span>
                </span>
              </div>

              <div className="active-ws-card__detail-item">
                <span className="active-ws-card__detail-label">Storage Usage</span>
                <span className="active-ws-card__detail-value">
                  <HardDrive size={13} aria-hidden="true" />
                  <span>{formatBytes(activeWorkspace.size_bytes)}</span>
                </span>
              </div>

              <div className="active-ws-card__detail-item">
                <span className="active-ws-card__detail-label">Created At</span>
                <span className="active-ws-card__detail-value">
                  {formatDate(activeWorkspace.created_at)}
                </span>
              </div>
            </div>

            {/* Direct Accounts & Login Credentials Section */}
            <div className="active-ws-accounts">
              <div className="active-ws-accounts__header">
                <div className="active-ws-accounts__title">
                  <KeyRound size={15} aria-hidden="true" style={{ color: 'var(--accent)' }} />
                  <span>Workspace Accounts &amp; Login Credentials</span>
                </div>
                <div className="active-ws-accounts__header-actions">
                  <button
                    type="button"
                    className="btn btn--xs btn--primary-subtle"
                    onClick={() => setCreateAccountOpen(true)}
                    title="Add a new account to this workspace"
                  >
                    <Plus size={12} aria-hidden="true" />
                    <span>Add Account</span>
                  </button>
                  <Link
                    to={`/settings?tab=accounts&workspace=${encodeURIComponent(activeWorkspace.id)}`}
                    className="btn btn--xs"
                    title="Open full routing & mailbox settings in Settings"
                  >
                    <ExternalLink size={12} aria-hidden="true" />
                    <span>Advanced Settings</span>
                  </Link>
                </div>
              </div>

              {accountsQuery.isLoading ? (
                <div className="active-ws-accounts__empty">Loading workspace accounts...</div>
              ) : accounts.length === 0 ? (
                <div className="active-ws-accounts__empty">
                  <span>No accounts in this workspace. Create an initial account to start authenticating clients.</span>
                  <button
                    type="button"
                    className="btn btn--xs btn--primary"
                    onClick={() => setCreateAccountOpen(true)}
                  >
                    <Plus size={12} aria-hidden="true" />
                    <span>Create Initial Account</span>
                  </button>
                </div>
              ) : (
                <div className="active-ws-accounts__grid">
                  {accounts.map((acc) => (
                    <div key={acc.id} className="active-ws-acc-card">
                      <div className="active-ws-acc-card__top">
                        <div className="active-ws-acc-card__user-row">
                          <span className="active-ws-acc-card__username">{acc.username}</span>
                          {acc.is_catch_all && (
                            <span className="active-ws-badge active-ws-badge--catchall">Catch-all</span>
                          )}
                          {acc.has_password ? (
                            <span className="active-ws-badge active-ws-badge--has-pw" title="Password configured for this account">
                              <Check size={11} aria-hidden="true" />
                              <span>Password Set</span>
                            </span>
                          ) : (
                            <span className="active-ws-badge active-ws-badge--no-pw" title="No password set (permissive dev mode)">
                              <span>No Password</span>
                            </span>
                          )}
                        </div>
                        <button
                          type="button"
                          className="btn btn--xs"
                          onClick={() => setEditingAccount(acc)}
                          title={acc.has_password ? 'Change or update password' : 'Set a password for this account'}
                        >
                          <KeyRound size={12} aria-hidden="true" />
                          <span>{acc.has_password ? 'Edit Password' : 'Set Password'}</span>
                        </button>
                      </div>

                      <div className="active-ws-acc-card__creds">
                        <div className="active-ws-acc-card__cred-item">
                          <span className="active-ws-acc-card__cred-key">Bare Username:</span>
                          <span className="active-ws-acc-card__cred-val">
                            <code>{acc.username}</code>
                            <CopyButton value={acc.username} label="bare username" />
                          </span>
                        </div>
                        <div className="active-ws-acc-card__cred-item">
                          <span className="active-ws-acc-card__cred-key">Scoped Username:</span>
                          <span className="active-ws-acc-card__cred-val">
                            <code>{activeWorkspace.id}/{acc.username}</code>
                            <CopyButton value={`${activeWorkspace.id}/${acc.username}`} label="scoped username" />
                          </span>
                        </div>
                      </div>
                    </div>
                  ))}
                </div>
              )}
            </div>

            <div className="active-ws-card__actions">
              <button
                type="button"
                className="btn btn--primary-subtle"
                onClick={() => setConnectTarget(activeWorkspace)}
                title="View code snippets and connection credentials for all frameworks"
              >
                <Plug size={13} aria-hidden="true" />
                <span>Connect Client</span>
              </button>

              <button
                type="button"
                className="btn"
                onClick={() => setCreateAccountOpen(true)}
                title="Add a new account to this workspace"
              >
                <Users size={13} aria-hidden="true" />
                <span>Add Account</span>
              </button>

              <button
                type="button"
                className="btn"
                onClick={() => void handleToggleAuth(activeWorkspace)}
                disabled={updateWorkspace.isPending}
                title={
                  activeWorkspace.auth_required
                    ? 'Switch to permissive developer mode (no password required)'
                    : 'Enforce authentication for this workspace'
                }
              >
                {activeWorkspace.auth_required ? (
                  <Unlock size={13} aria-hidden="true" />
                ) : (
                  <Lock size={13} aria-hidden="true" />
                )}
                <span>{activeWorkspace.auth_required ? 'Disable Auth' : 'Require Auth'}</span>
              </button>

              <button
                type="button"
                className="btn btn--danger"
                onClick={() => handleOpenPurge(activeWorkspace, 'mail_and_traces')}
                title="Purge all emails and connection traces while keeping user accounts and rules"
              >
                <RefreshCw size={13} aria-hidden="true" />
                <span>Purge Mail & Traces</span>
              </button>

              <button
                type="button"
                className="btn btn--danger"
                onClick={() => handleOpenPurge(activeWorkspace, 'factory_reset')}
                title="Full reset: Erase all messages, user accounts, and routing rules"
              >
                <Trash2 size={13} aria-hidden="true" />
                <span>Factory Reset</span>
              </button>
            </div>
          </section>
        )}

        {/* All Workspaces Grid */}
        <section className="workspaces-section" aria-label="All workspaces">
          <h2 className="workspaces-section__title">All Workspaces ({workspaces.length})</h2>

          <div className="workspaces-grid">
            {workspaces.map((ws) => {
              const isActive = ws.is_active
              return (
                <div
                  key={ws.id}
                  className={`ws-item-card${isActive ? ' ws-item-card--active' : ''}`}
                >
                  <div className="ws-item-card__main">
                    <div className="ws-item-card__header">
                      <div className="ws-item-card__identity">
                        <div className="ws-item-card__title-row">
                          <h3 className="ws-item-card__title" title={ws.name}>{ws.name}</h3>
                          {ws.auth_required ? (
                            <span className="ws-auth-badge ws-auth-badge--required" title="Authentication required">
                              <Lock size={9} aria-hidden="true" />
                              <span>Auth</span>
                            </span>
                          ) : (
                            <span className="ws-auth-badge ws-auth-badge--optional" title="Permissive dev mode">
                              <Unlock size={9} aria-hidden="true" />
                              <span>Open</span>
                            </span>
                          )}
                        </div>
                        <div className="ws-item-card__sub-row">
                          <span className="ws-item-card__slug" title={`Workspace ID: ${ws.id}`}>{ws.id}</span>
                          <span className={`ws-type-tag ${ws.id === 'default' ? 'ws-type-tag--primary' : 'ws-type-tag--custom'}`}>
                            {ws.id === 'default' ? 'Primary Workspace' : 'Custom Workspace'}
                          </span>
                        </div>
                      </div>

                      <div className="ws-item-card__header-action">
                        {isActive ? (
                          <span className="active-ws-card__badge" title="This workspace is currently active">
                            <span className="active-ws-card__badge-dot" />
                            <span>Active</span>
                          </span>
                        ) : (
                          <button
                            type="button"
                            className="btn btn--sm ws-item-card__switch-btn"
                            onClick={() => void handleActivate(ws.id)}
                            disabled={activateWorkspace.isPending}
                            title="Switch to make this your active workspace"
                          >
                            <Sparkles size={11} aria-hidden="true" />
                            <span>Switch to this</span>
                          </button>
                        )}
                      </div>
                    </div>

                    {ws.description ? (
                      <p className="ws-item-card__desc" title={ws.description}>{ws.description}</p>
                    ) : null}
                  </div>

                  <div className="ws-item-card__bottom">
                    <div className="ws-item-card__stats">
                      <div className="ws-item-card__stat-group">
                        <div className="ws-item-card__stat-item" title="Stored messages">
                          <Mail size={12} aria-hidden="true" />
                          <span>{ws.message_count} mails</span>
                        </div>
                        <span className="ws-item-card__stat-sep" aria-hidden="true">•</span>
                        <div className="ws-item-card__stat-item" title="Database disk size">
                          <HardDrive size={12} aria-hidden="true" />
                          <span>{formatBytes(ws.size_bytes)}</span>
                        </div>
                      </div>
                      <span className="ws-item-card__stat-date" title={`Created: ${formatDate(ws.created_at)}`}>
                        {formatDate(ws.created_at)}
                      </span>
                    </div>

                    <div className="ws-item-card__path" title={ws.database_path}>
                      <Database size={11} aria-hidden="true" className="ws-item-card__path-icon" />
                      <span className="ws-item-card__path-text">{ws.database_path}</span>
                      <CopyButton value={ws.database_path} label="database path" />
                    </div>

                    <div className="ws-item-card__footer">
                      <div className="ws-item-card__footer-group">
                        <button
                          type="button"
                          className="btn btn--sm btn--primary-subtle"
                          onClick={() => setConnectTarget(ws)}
                          title="View application connection settings"
                        >
                          <Plug size={12} aria-hidden="true" />
                          <span>Connect</span>
                        </button>

                        <button
                          type="button"
                          className="btn btn--sm"
                          onClick={() => void handleOpenAccounts(ws)}
                          disabled={activatingWorkspaceId === ws.id}
                          title={`Manage accounts and credentials for ${ws.name}`}
                        >
                          <Users size={12} aria-hidden="true" />
                          <span>Accounts</span>
                        </button>
                      </div>

                      <div className="ws-item-card__footer-group ws-item-card__footer-group--end">
                        <button
                          type="button"
                          className="btn btn--sm"
                          onClick={() => handleOpenPurge(ws)}
                          title="Purge messages in this database"
                        >
                          <RefreshCw size={12} aria-hidden="true" />
                          <span>Purge</span>
                        </button>

                        {ws.id !== 'default' && (
                          <button
                            type="button"
                            className="btn btn--sm btn--danger"
                            onClick={() => void handleDelete(ws)}
                            disabled={deleteWorkspace.isPending || isActive}
                            title={
                              isActive
                                ? 'Cannot delete currently active workspace. Switch to another workspace first.'
                                : 'Delete this workspace and remove its database from disk'
                            }
                          >
                            <Trash2 size={12} aria-hidden="true" />
                            <span>Delete</span>
                          </button>
                        )}
                      </div>
                    </div>
                  </div>
                </div>
              )
            })}
          </div>
        </section>
      </div>

      {/* Create Workspace Modal */}
      {createModalOpen && (
        <div className="ws-modal-overlay" role="dialog" aria-modal="true" aria-labelledby="create-ws-title">
          <div className="ws-modal">
            <header className="ws-modal__header">
              <h2 id="create-ws-title" className="ws-modal__title">
                <Plus size={16} aria-hidden="true" />
                <span>Create New Workspace</span>
              </h2>
              <button
                type="button"
                className="ws-modal__close"
                onClick={() => setCreateModalOpen(false)}
                aria-label="Close"
              >
                <X size={16} aria-hidden="true" />
              </button>
            </header>

            <form onSubmit={handleCreateSubmit}>
              <div className="ws-modal__body">
                {createError && (
                  <div className="ws-alert ws-alert--warn">
                    <AlertTriangle size={14} aria-hidden="true" />
                    <span>{createError}</span>
                  </div>
                )}

                <div className="ws-form-group">
                  <label htmlFor="ws-name" className="ws-form-label">
                    Workspace Name *
                  </label>
                  <input
                    id="ws-name"
                    name="name"
                    type="text"
                    className="ws-form-input"
                    placeholder="e.g. Billing Service, Staging Tests"
                    value={newWsName}
                    onChange={(e) => handleNameChange(e.target.value)}
                    required
                    autoFocus
                  />
                  <span className="ws-form-hint">
                    Human-readable display name for this workspace.
                  </span>
                </div>

                <div className="ws-form-group">
                  <label htmlFor="ws-id" className="ws-form-label">
                    Slug ID (Optional)
                  </label>
                  <input
                    id="ws-id"
                    name="id"
                    type="text"
                    className="ws-form-input ws-form-input--mono"
                    placeholder="e.g. billing-service"
                    value={newWsId}
                    onChange={(e) => {
                      setNewWsId(e.target.value)
                      setSlugManuallyEdited(true)
                    }}
                  />
                  <span className="ws-form-hint">
                    Unique identifier used for folder naming and CLI switching. Auto-derived if left empty.
                  </span>
                </div>

                <div className="ws-form-group">
                  <label htmlFor="ws-desc" className="ws-form-label">
                    Description (Optional)
                  </label>
                  <input
                    id="ws-desc"
                    name="description"
                    type="text"
                    className="ws-form-input"
                    placeholder="e.g. Dedicated inbox for end-to-end checkout emails"
                    value={newWsDesc}
                    onChange={(e) => setNewWsDesc(e.target.value)}
                  />
                </div>

                <div className="ws-form-group">
                  <label htmlFor="ws-auth-required" className="ws-checkbox-label">
                    <input
                      id="ws-auth-required"
                      name="auth_required"
                      type="checkbox"
                      checked={newWsAuthRequired}
                      onChange={(e) => setNewWsAuthRequired(e.target.checked)}
                    />
                    <span>Require SMTP / IMAP / POP3 authentication for this workspace</span>
                  </label>
                  <span className="ws-form-hint">
                    Enforces credentials verification when clients connect to send or receive mail.
                  </span>
                </div>

                <div className="ws-form-group">
                  <label htmlFor="ws-password" className="ws-form-label">
                    Account Password (Optional)
                  </label>
                  <div className="ws-form-password-wrapper">
                    <input
                      id="ws-password"
                      name="password"
                      type={showNewWsPassword ? 'text' : 'password'}
                      className="ws-form-input"
                      placeholder="Enter password for initial/dev account"
                      value={newWsPassword}
                      onChange={(e) => setNewWsPassword(e.target.value)}
                    />
                    <button
                      type="button"
                      className="ws-form-password-toggle"
                      onClick={() => setShowNewWsPassword((prev) => !prev)}
                      title={showNewWsPassword ? 'Hide password' : 'Show password'}
                      aria-label={showNewWsPassword ? 'Hide password' : 'Show password'}
                      tabIndex={-1}
                    >
                      {showNewWsPassword ? <EyeOff size={14} aria-hidden="true" /> : <Eye size={14} aria-hidden="true" />}
                    </button>
                  </div>
                  <span className="ws-form-hint">
                    Sets the initial password for account <code>{initialUsername.trim() || 'dev'}</code>. Used when authenticating clients.
                  </span>
                </div>

                <label htmlFor="ws-clone-accounts" className="ws-checkbox-label">
                  <input
                    id="ws-clone-accounts"
                    name="clone_accounts"
                    type="checkbox"
                    checked={cloneAccounts}
                    onChange={(e) => setCloneAccounts(e.target.checked)}
                  />
                  <span>
                    Clone accounts & delivery routing from current workspace ({activeWorkspace?.name || activeWorkspace?.id || 'Active'})
                  </span>
                </label>

                {cloneAccounts ? (
                  <div className="ws-hint-box">
                    <Sparkles size={14} aria-hidden="true" />
                    <div>
                      <strong>Inherits from {activeWorkspace?.name || activeWorkspace?.id || 'Active'}:</strong>
                      <span>
                        {' '}All configured user accounts, bound email addresses, default mailboxes, and delivery rules will be copied (starting with 0 messages).
                      </span>
                    </div>
                  </div>
                ) : (
                  <div className="ws-initial-account-card">
                    <div className="ws-initial-account-card__head">
                      <Users size={14} aria-hidden="true" />
                      <span>Initial Account & Bound Email Addresses (Optional)</span>
                    </div>

                    <div className="ws-form-group">
                      <label htmlFor="ws-init-username" className="ws-form-label">
                        Account Username
                      </label>
                      <input
                        id="ws-init-username"
                        name="init_username"
                        type="text"
                        className="ws-form-input"
                        placeholder="dev"
                        value={initialUsername}
                        onChange={(e) => setInitialUsername(e.target.value)}
                      />
                    </div>

                    <div className="ws-form-group">
                      <label htmlFor="ws-init-addresses" className="ws-form-label">
                        Bound Email Addresses
                      </label>
                      <input
                        id="ws-init-addresses"
                        name="init_addresses"
                        type="text"
                        className="ws-form-input"
                        placeholder="e.g. orders@example.com, billing@example.com"
                        value={initialAddresses}
                        onChange={(e) => setInitialAddresses(e.target.value)}
                      />
                      <span className="ws-form-hint">
                        Comma-separated email addresses that route into this account inbox.
                      </span>
                    </div>

                    <label htmlFor="ws-init-catch-all" className="ws-checkbox-label">
                      <input
                        id="ws-init-catch-all"
                        name="init_catch_all"
                        type="checkbox"
                        checked={initialCatchAll}
                        onChange={(e) => setInitialCatchAll(e.target.checked)}
                      />
                      <span>Catch-All (receives all unmatched incoming recipient addresses)</span>
                    </label>
                  </div>
                )}

                <label htmlFor="ws-set-active" className="ws-checkbox-label">
                  <input
                    id="ws-set-active"
                    name="set_active"
                    type="checkbox"
                    checked={newWsSetActive}
                    onChange={(e) => setNewWsSetActive(e.target.checked)}
                  />
                  <span>Switch to this workspace immediately after creation</span>
                </label>
              </div>

              <footer className="ws-modal__footer">
                <button
                  type="button"
                  className="btn"
                  onClick={() => setCreateModalOpen(false)}
                >
                  Cancel
                </button>
                <button
                  type="submit"
                  className="btn btn--primary"
                  disabled={createWorkspace.isPending || !newWsName.trim()}
                >
                  {createWorkspace.isPending ? 'Creating...' : 'Create Workspace'}
                </button>
              </footer>
            </form>
          </div>
        </div>
      )}

      {/* Purge Workspace Modal */}
      {purgeTarget && (
        <div className="ws-modal-overlay" role="dialog" aria-modal="true" aria-labelledby="purge-ws-title">
          <div className="ws-modal">
            <header className="ws-modal__header">
              <h2 id="purge-ws-title" className="ws-modal__title">
                <RefreshCw size={16} aria-hidden="true" />
                <span>Purge Workspace "{purgeTarget.name}"</span>
              </h2>
              <button
                type="button"
                className="ws-modal__close"
                onClick={() => setPurgeTarget(null)}
                aria-label="Close"
              >
                <X size={16} aria-hidden="true" />
              </button>
            </header>

            <div className="ws-modal__body">
              <div className="ws-alert ws-alert--warn">
                <AlertTriangle size={16} aria-hidden="true" />
                <span>
                  Purging deletes records from <code>{purgeTarget.id}</code> and executes a SQLite VACUUM to reclaim disk space.
                  This action is irreversible.
                </span>
              </div>

              <div className="ws-form-group">
                <span className="ws-form-label">Purge Scope</span>
                <div className="ws-radio-group">
                  <label
                    htmlFor="purge-scope-traces"
                    className={`ws-radio-option${purgeMode === 'mail_and_traces' ? ' ws-radio-option--selected' : ''
                      }`}
                  >
                    <input
                      id="purge-scope-traces"
                      type="radio"
                      name="purge-mode"
                      value="mail_and_traces"
                      checked={purgeMode === 'mail_and_traces'}
                      onChange={() => setPurgeMode('mail_and_traces')}
                    />
                    <div className="ws-radio-text">
                      <span className="ws-radio-title">Purge Mail & Traces (Recommended)</span>
                      <span className="ws-radio-desc">
                        Deletes all stored messages, attachments, search index, and connection traces.
                        Preserves user accounts, credentials, and custom routing rules.
                      </span>
                    </div>
                  </label>

                  <label
                    htmlFor="purge-scope-reset"
                    className={`ws-radio-option${purgeMode === 'factory_reset' ? ' ws-radio-option--selected' : ''
                      }`}
                  >
                    <input
                      id="purge-scope-reset"
                      type="radio"
                      name="purge-mode"
                      value="factory_reset"
                      checked={purgeMode === 'factory_reset'}
                      onChange={() => setPurgeMode('factory_reset')}
                    />
                    <div className="ws-radio-text">
                      <span className="ws-radio-title">Factory Reset</span>
                      <span className="ws-radio-desc">
                        Complete reset of this workspace. Erases everything including user accounts,
                        webhooks, and routing rules, returning it to a brand new database state.
                      </span>
                    </div>
                  </label>
                </div>
              </div>

              <div className="ws-form-group">
                <span className="ws-form-label">Options</span>
                <label htmlFor="purge-clear-traces" className="ws-checkbox-label">
                  <input
                    id="purge-clear-traces"
                    name="clear_traces"
                    type="checkbox"
                    checked={clearTraces}
                    onChange={(e) => setClearTraces(e.target.checked)}
                  />
                  <span>Clear connection audit traces</span>
                </label>

                <label htmlFor="purge-clear-webhooks" className="ws-checkbox-label">
                  <input
                    id="purge-clear-webhooks"
                    name="clear_webhooks"
                    type="checkbox"
                    checked={clearWebhooks}
                    onChange={(e) => setClearWebhooks(e.target.checked)}
                  />
                  <span>Clear webhook delivery logs</span>
                </label>
              </div>
            </div>

            <footer className="ws-modal__footer">
              <button
                type="button"
                className="btn"
                onClick={() => setPurgeTarget(null)}
              >
                Cancel
              </button>
              <button
                type="button"
                className="btn btn--danger-filled"
                onClick={() => void handleExecutePurge()}
                disabled={purgeWorkspace.isPending}
              >
                {purgeWorkspace.isPending ? 'Purging & Reclaiming...' : 'Execute Purge'}
              </button>
            </footer>
          </div>
        </div>
      )}

      {/* Connect Client Modal */}
      {connectTarget && (
        <ConnectClientModal
          workspace={connectTarget}
          onClose={() => setConnectTarget(null)}
        />
      )}

      {/* Direct Account Modals */}
      {createAccountOpen && (
        <AccountModal onClose={() => setCreateAccountOpen(false)} />
      )}
      {editingAccount && (
        <AccountModal
          account={editingAccount}
          onClose={() => setEditingAccount(null)}
        />
      )}
    </div>
  )
}
