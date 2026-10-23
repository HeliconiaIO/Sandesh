/**
 * The mailbox list in the sidebar.
 *
 * Names and counts come from `GET /api/v1/mailboxes` — never assumed (§89).
 *
 * Multiple accounts: when more than one account exists, mailboxes are grouped
 * by account with distinct headers, badges, and quick-add actions.
 *
 * Ordering & Hierarchy:
 *   • System folders (INBOX, Drafts, Sent, Archive, Junk, Trash) appear first
 *     in canonical email order and are protected against rename/delete.
 *   • Custom folders appear under a distinct "Folders" subsection, formatted
 *     hierarchically if path delimiters (`/`) are present.
 *   • Context menu (⋯) provides Rename, Delete (with inline confirmation),
 *     and "New subfolder..." actions.
 */

import { useState } from 'react'
import { NavLink, useLocation, useNavigate } from 'react-router-dom'
import {
  AlertOctagon,
  Archive,
  Check,
  ChevronDown,
  ChevronRight,
  CornerDownRight,
  FileText,
  Folder,
  FolderPlus,
  Inbox,
  MoreHorizontal,
  Pencil,
  Plus,
  Send,
  Trash2,
  User,
  X,
} from 'lucide-react'

import {
  useAccounts,
  useCreateMailbox,
  useDeleteMailbox,
  useMailboxes,
  useRenameMailbox,
} from '../../hooks/queries'
import type { Mailbox } from '../../api/client'
import './MailboxNav.css'

export function getMailboxIcon(name: string) {
  const lower = name.toLowerCase()
  if (lower === 'inbox')
    return <Inbox size={14} aria-hidden="true" className="mailbox-nav__icon" />
  if (lower === 'sent' || lower === 'sent items')
    return <Send size={14} aria-hidden="true" className="mailbox-nav__icon" />
  if (lower === 'drafts' || lower === 'draft')
    return <FileText size={14} aria-hidden="true" className="mailbox-nav__icon" />
  if (lower === 'archive')
    return <Archive size={14} aria-hidden="true" className="mailbox-nav__icon" />
  if (lower === 'trash' || lower === 'deleted' || lower === 'bin')
    return <Trash2 size={14} aria-hidden="true" className="mailbox-nav__icon" />
  if (lower === 'junk' || lower === 'spam')
    return <AlertOctagon size={14} aria-hidden="true" className="mailbox-nav__icon" />
  return <Folder size={14} aria-hidden="true" className="mailbox-nav__icon" />
}

export const PROTECTED_SYSTEM_MAILBOXES = [
  'inbox',
  'drafts',
  'draft',
  'sent',
  'sent items',
  'archive',
  'junk',
  'spam',
  'trash',
  'deleted',
  'bin',
]

export const isProtectedMailbox = (name: string) =>
  PROTECTED_SYSTEM_MAILBOXES.includes(name.toLowerCase())

const SYSTEM_FOLDER_ORDER: Record<string, number> = {
  inbox: 0,
  drafts: 1,
  draft: 1,
  sent: 2,
  'sent items': 2,
  archive: 3,
  junk: 4,
  spam: 4,
  trash: 5,
  deleted: 5,
  bin: 5,
}

// ---------------------------------------------------------------------------
// Inline new-folder row
// ---------------------------------------------------------------------------

function NewFolderRow({
  accountId,
  initialValue = '',
  onDone,
}: {
  accountId: string | undefined
  initialValue?: string
  onDone: () => void
}) {
  const [value, setValue] = useState(initialValue)
  const [errorMsg, setErrorMsg] = useState<string | null>(null)
  const create = useCreateMailbox()

  const submit = () => {
    const name = value.trim()
    if (!name) {
      onDone()
      return
    }
    setErrorMsg(null)
    create.mutate(
      { name, ...(accountId !== undefined ? { account_id: accountId } : {}) },
      {
        onSuccess: onDone,
        onError: (err: unknown) => {
          const detail =
            (err as { detail?: string })?.detail ||
            (err as Error)?.message ||
            'Failed to create folder'
          setErrorMsg(detail)
        },
      },
    )
  }

  return (
    <li className="mailbox-nav__new-folder-wrapper">
      <div className="mailbox-nav__new-folder-row">
        <Folder size={14} aria-hidden="true" className="mailbox-nav__icon" />
        <input
          autoFocus
          className={`mailbox-nav__new-folder-input ${errorMsg ? 'mailbox-nav__new-folder-input--error' : ''}`}
          placeholder="Folder name"
          value={value}
          onChange={(e) => {
            setValue(e.target.value)
            if (errorMsg) setErrorMsg(null)
          }}
          onKeyDown={(e) => {
            if (e.key === 'Enter') submit()
            if (e.key === 'Escape') onDone()
          }}
          disabled={create.isPending}
          aria-label="New folder name"
        />
        <button
          type="button"
          className="mailbox-nav__new-folder-ok"
          onClick={submit}
          disabled={create.isPending || !value.trim()}
          aria-label="Create folder"
          title="Create (Enter)"
        >
          <Check size={11} />
        </button>
        <button
          type="button"
          className="mailbox-nav__new-folder-cancel"
          onClick={onDone}
          aria-label="Cancel"
          title="Cancel (Esc)"
        >
          <X size={11} />
        </button>
      </div>
      {errorMsg && (
        <div className="mailbox-nav__input-error" role="alert">
          {errorMsg}
        </div>
      )}
    </li>
  )
}

// ---------------------------------------------------------------------------
// Rename inline row
// ---------------------------------------------------------------------------

function RenameRow({
  mailbox,
  onDone,
}: {
  mailbox: Mailbox
  onDone: () => void
}) {
  const [value, setValue] = useState(mailbox.name)
  const [errorMsg, setErrorMsg] = useState<string | null>(null)
  const rename = useRenameMailbox()

  const submit = () => {
    const name = value.trim()
    if (!name || name === mailbox.name) {
      onDone()
      return
    }
    setErrorMsg(null)
    rename.mutate(
      { id: mailbox.id, name },
      {
        onSuccess: onDone,
        onError: (err: unknown) => {
          const detail =
            (err as { detail?: string })?.detail ||
            (err as Error)?.message ||
            'Failed to rename folder'
          setErrorMsg(detail)
        },
      },
    )
  }

  return (
    <li className="mailbox-nav__new-folder-wrapper">
      <div className="mailbox-nav__new-folder-row">
        {getMailboxIcon(mailbox.name)}
        <input
          autoFocus
          className={`mailbox-nav__new-folder-input ${errorMsg ? 'mailbox-nav__new-folder-input--error' : ''}`}
          value={value}
          onChange={(e) => {
            setValue(e.target.value)
            if (errorMsg) setErrorMsg(null)
          }}
          onKeyDown={(e) => {
            if (e.key === 'Enter') submit()
            if (e.key === 'Escape') onDone()
          }}
          disabled={rename.isPending}
          aria-label="Rename folder"
        />
        <button
          type="button"
          className="mailbox-nav__new-folder-ok"
          onClick={submit}
          disabled={rename.isPending || !value.trim() || value.trim() === mailbox.name}
          aria-label="Save"
          title="Save (Enter)"
        >
          <Check size={11} />
        </button>
        <button
          type="button"
          className="mailbox-nav__new-folder-cancel"
          onClick={onDone}
          aria-label="Cancel"
          title="Cancel (Esc)"
        >
          <X size={11} />
        </button>
      </div>
      {errorMsg && (
        <div className="mailbox-nav__input-error" role="alert">
          {errorMsg}
        </div>
      )}
    </li>
  )
}

// ---------------------------------------------------------------------------
// Single mailbox row with context menu & delete confirmation popover
// ---------------------------------------------------------------------------

function MailboxRow({
  mailbox,
  accountInboxes,
  depth = 0,
  displayName,
  onAddSubfolder,
}: {
  mailbox: Mailbox
  accountInboxes: Map<string, string>
  depth?: number
  displayName?: string
  onAddSubfolder?: (parentName: string) => void
}) {
  const [menuOpen, setMenuOpen] = useState(false)
  const [renaming, setRenaming] = useState(false)
  const [confirmDelete, setConfirmDelete] = useState(false)
  const deleteMailbox = useDeleteMailbox()
  const navigate = useNavigate()
  const location = useLocation()
  const protected_ = isProtectedMailbox(mailbox.name)

  if (renaming) {
    return <RenameRow mailbox={mailbox} onDone={() => setRenaming(false)} />
  }

  const isCurrent = location.pathname.includes(`/mailbox/${mailbox.id}`)

  const handleConfirmDelete = () => {
    setConfirmDelete(false)
    deleteMailbox.mutate(mailbox.id, {
      onSuccess: () => {
        if (isCurrent) {
          const inboxId = accountInboxes.get(mailbox.account_id)
          navigate(inboxId ? `/mailbox/${inboxId}` : '/')
        }
      },
    })
  }

  const nameToShow = displayName ?? mailbox.name

  return (
    <li
      className={`mailbox-nav__item-wrapper ${depth > 0 ? 'mailbox-nav__item-wrapper--nested' : ''}`}
      style={depth > 0 ? { paddingLeft: `${depth * 14}px` } : undefined}
    >
      <NavLink to={`/mailbox/${mailbox.id}`} className="nav-item">
        <span className="mailbox-nav__label">
          {depth > 0 ? (
            <CornerDownRight size={12} className="mailbox-nav__branch" aria-hidden="true" />
          ) : null}
          {getMailboxIcon(mailbox.name)}
          <span className="mailbox-nav__name" title={mailbox.name}>
            {nameToShow}
          </span>
        </span>
        {mailbox.unseen > 0 ? (
          <span className="mailbox-nav__count mailbox-nav__count--unseen">
            {mailbox.unseen}
            <span className="sr-only"> unread</span>
          </span>
        ) : mailbox.total > 0 ? (
          <span className="mailbox-nav__count">
            {mailbox.total}
            <span className="sr-only"> messages</span>
          </span>
        ) : null}
      </NavLink>

      {!protected_ && (
        <div className="mailbox-nav__ctx-wrap">
          <button
            type="button"
            className="mailbox-nav__ctx-btn"
            aria-label={`Options for ${mailbox.name}`}
            aria-expanded={menuOpen}
            onClick={(e) => {
              e.preventDefault()
              setMenuOpen((v) => !v)
            }}
          >
            <MoreHorizontal size={13} aria-hidden="true" />
          </button>

          {menuOpen && (
            <>
              <div
                className="mailbox-nav__ctx-backdrop"
                onClick={() => setMenuOpen(false)}
                aria-hidden="true"
              />
              <div className="mailbox-nav__ctx-menu" role="menu">
                {onAddSubfolder && (
                  <button
                    type="button"
                    role="menuitem"
                    className="mailbox-nav__ctx-item"
                    onClick={() => {
                      setMenuOpen(false)
                      onAddSubfolder(mailbox.name)
                    }}
                  >
                    <FolderPlus size={12} aria-hidden="true" />
                    New subfolder
                  </button>
                )}
                <button
                  type="button"
                  role="menuitem"
                  className="mailbox-nav__ctx-item"
                  onClick={() => {
                    setMenuOpen(false)
                    setRenaming(true)
                  }}
                >
                  <Pencil size={12} aria-hidden="true" />
                  Rename
                </button>
                <button
                  type="button"
                  role="menuitem"
                  className="mailbox-nav__ctx-item mailbox-nav__ctx-item--danger"
                  onClick={() => {
                    setMenuOpen(false)
                    setConfirmDelete(true)
                  }}
                >
                  <Trash2 size={12} aria-hidden="true" />
                  Delete
                </button>
              </div>
            </>
          )}

          {confirmDelete && (
            <>
              <div
                className="mailbox-nav__ctx-backdrop"
                onClick={() => setConfirmDelete(false)}
                aria-hidden="true"
              />
              <div className="mailbox-nav__confirm-dialog" role="alertdialog">
                <div className="mailbox-nav__confirm-title">Delete folder?</div>
                <p className="mailbox-nav__confirm-text">
                  Delete &ldquo;{mailbox.name}&rdquo;
                  {mailbox.total > 0
                    ? ` and all ${mailbox.total} message${mailbox.total === 1 ? '' : 's'} inside`
                    : ''}
                  ?
                </p>
                <div className="mailbox-nav__confirm-actions">
                  <button
                    type="button"
                    className="mailbox-nav__confirm-btn mailbox-nav__confirm-btn--danger"
                    onClick={handleConfirmDelete}
                    disabled={deleteMailbox.isPending}
                  >
                    {deleteMailbox.isPending ? 'Deleting...' : 'Delete'}
                  </button>
                  <button
                    type="button"
                    className="mailbox-nav__confirm-btn mailbox-nav__confirm-btn--cancel"
                    onClick={() => setConfirmDelete(false)}
                    disabled={deleteMailbox.isPending}
                  >
                    Cancel
                  </button>
                </div>
              </div>
            </>
          )}
        </div>
      )}
    </li>
  )
}

// ---------------------------------------------------------------------------
// Account group with System Folders & Hierarchical Custom Folders
// ---------------------------------------------------------------------------

function AccountGroup({
  accountLabel,
  accountEmail,
  mailboxes,
  accountId,
  multiAccount,
  accountInboxes,
}: {
  accountLabel: string | undefined
  accountEmail?: string | undefined
  mailboxes: Mailbox[]
  accountId: string | undefined
  multiAccount: boolean
  accountInboxes: Map<string, string>
}) {
  const [addingFolder, setAddingFolder] = useState(false)
  const [initialFolderName, setInitialFolderName] = useState('')

  const [collapsed, setCollapsed] = useState(() => {
    try {
      return localStorage.getItem(`sandesh_account_collapsed_${accountId}`) === 'true'
    } catch {
      return false
    }
  })

  const toggleCollapsed = () => {
    setCollapsed((prev) => {
      const next = !prev
      try {
        localStorage.setItem(`sandesh_account_collapsed_${accountId}`, String(next))
      } catch {
        // Storage can be denied outright in private mode; the toggle still works,
        // it just will not survive a reload.
      }
      return next
    })
  }

  const accountUnseen = mailboxes.reduce((sum, mb) => sum + mb.unseen, 0)

  // Partition mailboxes into system mailboxes and custom user folders
  const systemMailboxes: Mailbox[] = []
  const customMailboxes: Mailbox[] = []

  for (const mb of mailboxes) {
    if (isProtectedMailbox(mb.name)) {
      systemMailboxes.push(mb)
    } else {
      customMailboxes.push(mb)
    }
  }

  // Sort system mailboxes in canonical email order
  systemMailboxes.sort((a, b) => {
    const orderA = SYSTEM_FOLDER_ORDER[a.name.toLowerCase()] ?? 99
    const orderB = SYSTEM_FOLDER_ORDER[b.name.toLowerCase()] ?? 99
    return orderA - orderB
  })

  // Sort custom mailboxes alphabetically
  customMailboxes.sort((a, b) => a.name.localeCompare(b.name))

  const handleStartAddFolder = (prefix?: string) => {
    setInitialFolderName(prefix ? `${prefix}/` : '')
    setAddingFolder(true)
  }

  return (
    <div className={`mailbox-nav__account-section ${multiAccount ? 'mailbox-nav__account-section--multi' : ''}`}>
      {multiAccount && (
        <div className="mailbox-nav__account-header">
          <button
            type="button"
            className="mailbox-nav__account-info"
            onClick={toggleCollapsed}
            title={accountEmail ? `${accountLabel} (${accountEmail})` : accountLabel}
            aria-expanded={!collapsed}
          >
            <span className="mailbox-nav__account-chevron" aria-hidden="true">
              {collapsed ? <ChevronRight size={12} /> : <ChevronDown size={12} />}
            </span>
            <span className="mailbox-nav__account-avatar" aria-hidden="true">
              <User size={12} />
            </span>
            <span className="mailbox-nav__account-name">{accountLabel}</span>
            {collapsed && accountUnseen > 0 && (
              <span className="mailbox-nav__count mailbox-nav__count--unseen">
                {accountUnseen}
              </span>
            )}
          </button>
          <button
            type="button"
            className="mailbox-nav__account-add-btn"
            onClick={() => {
              if (collapsed) setCollapsed(false)
              handleStartAddFolder()
            }}
            title={`Create new folder in ${accountLabel}`}
            aria-label={`Create new folder in ${accountLabel}`}
          >
            <Plus size={13} aria-hidden="true" />
          </button>
        </div>
      )}

      {!collapsed && (
        <>
          {/* System mailboxes: INBOX, Drafts, Sent, Archive, Junk, Trash */}
          <ul className="mailbox-nav__list">
            {systemMailboxes.map((mailbox) => (
              <MailboxRow
                key={mailbox.id}
                mailbox={mailbox}
                accountInboxes={accountInboxes}
              />
            ))}
          </ul>

          {/* Custom folders section */}
          {(customMailboxes.length > 0 || addingFolder) && (
            <div className="mailbox-nav__custom-section">
              <div className="mailbox-nav__custom-header">
                <span className="mailbox-nav__custom-title">Folders</span>
                {!multiAccount && (
                  <button
                    type="button"
                    className="mailbox-nav__custom-add-btn"
                    onClick={() => handleStartAddFolder()}
                    title="Create folder"
                    aria-label="Create folder"
                  >
                    <Plus size={12} aria-hidden="true" />
                  </button>
                )}
              </div>

              <ul className="mailbox-nav__list">
                {customMailboxes.map((mailbox) => {
                  const parts = mailbox.name.split('/')
                  const depth = parts.length - 1
                  const leafName = parts[parts.length - 1] ?? mailbox.name
                  return (
                    <MailboxRow
                      key={mailbox.id}
                      mailbox={mailbox}
                      accountInboxes={accountInboxes}
                      depth={depth}
                      displayName={depth > 0 ? leafName : mailbox.name}
                      onAddSubfolder={handleStartAddFolder}
                    />
                  )
                })}

                {addingFolder && (
                  <NewFolderRow
                    accountId={accountId}
                    initialValue={initialFolderName}
                    onDone={() => setAddingFolder(false)}
                  />
                )}
              </ul>
            </div>
          )}

          {!addingFolder && customMailboxes.length === 0 && (
            <button
              type="button"
              className="mailbox-nav__add-btn"
              onClick={() => handleStartAddFolder()}
            >
              <Plus size={13} aria-hidden="true" />
              <span>New folder</span>
            </button>
          )}
        </>
      )}
    </div>
  )
}

// ---------------------------------------------------------------------------
// Root component
// ---------------------------------------------------------------------------

export function MailboxNav() {
  const mailboxesQuery = useMailboxes()
  const accountsQuery = useAccounts()

  if (mailboxesQuery.isPending || accountsQuery.isPending) {
    return <p className="mailbox-nav__note">Loading mailboxes</p>
  }

  if (mailboxesQuery.isError) {
    return <p className="mailbox-nav__note mailbox-nav__note--error">Mailboxes unavailable</p>
  }

  const allMailboxes = mailboxesQuery.data?.items ?? []
  const accounts = accountsQuery.data?.items ?? []
  const multiAccount = accounts.length > 1

  if (allMailboxes.length === 0 && accounts.length === 0) {
    return <p className="mailbox-nav__note">No mailboxes</p>
  }

  // Map account_id -> INBOX mailbox id for clean redirect on delete
  const accountInboxes = new Map<string, string>()
  for (const mb of allMailboxes) {
    if (mb.name.toLowerCase() === 'inbox') {
      accountInboxes.set(mb.account_id, mb.id)
    }
  }

  // Group mailboxes by account_id
  const grouped = new Map<string, Mailbox[]>()
  for (const mb of allMailboxes) {
    const list = grouped.get(mb.account_id) ?? []
    list.push(mb)
    grouped.set(mb.account_id, list)
  }

  // Ensure accounts with no mailboxes yet still appear
  for (const acc of accounts) {
    if (!grouped.has(acc.id)) grouped.set(acc.id, [])
  }

  // Preserve account order from accounts list; fallback to grouped keys
  const orderedAccountIds =
    accounts.length > 0 ? accounts.map((a) => a.id) : Array.from(grouped.keys())

  return (
    <div className="mailbox-nav">
      {orderedAccountIds.map((accountId) => {
        const mbList = grouped.get(accountId) ?? []
        const account = accounts.find((a) => a.id === accountId)
        const label = multiAccount
          ? (account?.username ?? `Account (${accountId.slice(0, 6)})`)
          : undefined
        return (
          <AccountGroup
            key={accountId}
            accountLabel={label}
            accountEmail={account?.addresses?.[0]}
            mailboxes={mbList}
            accountId={accountId}
            multiAccount={multiAccount}
            accountInboxes={accountInboxes}
          />
        )
      })}
    </div>
  )
}
