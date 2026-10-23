/**
 * The message inspector (§14).
 *
 * Tabs over one message, ordered as an inspection ladder: what the message
 * *claims* to be (Preview), what it *says* it is (Headers), how it is *built*
 * (MIME), and finally what it *actually is* (Raw). That ordering is the
 * product's argument — every rendered view is one click from the bytes behind
 * it (§3 of UI_UX_DIRECTION).
 *
 * Each tab fetches only what it needs, and Raw only when opened (§56).
 */

import { useEffect, useMemo, useRef, useState } from 'react'
import {
  AlertOctagon,
  Archive,
  ArrowLeft,
  Binary,
  Circle,
  CircleDot,
  CornerDownRight,
  Download,
  Edit,
  Eye,
  Folder,
  FolderOpen,
  FolderTree,
  Inbox,
  Laptop,
  Link2,
  ListOrdered,
  Paperclip,
  Reply,
  Route,
  RotateCw,
  Smartphone,
  Tablet,
  Trash2,
} from 'lucide-react'

import { getMailboxIcon, isProtectedMailbox } from '../mailboxes/MailboxNav'
import type { Attachment, MessageDetail, MimePart } from '../../api/client'
import { CopyButton } from '../../components/CopyButton'
import { HtmlPreview } from '../../components/HtmlPreview'
import { ErrorState, LoadingState } from '../../components/States'
import { useComposer } from '../composer/ComposerContext'
import {
  useDeleteEntry,
  useDelivery,
  useHeaderAnalysis,
  useHeaders,
  useLinks,
  useMailboxes,
  useMime,
  useMoveEntry,
  useRaw,
  useReplayMessage,
  useSetFlags,
} from '../../hooks/queries'
import { useKeyboardShortcuts } from '../../hooks/useKeyboardShortcuts'
import { attachments as attachmentsApi, messages as api } from '../../api/resources'
import { formatAddress, formatFull, formatSize } from '../../lib/format'
import { AttachmentList } from './AttachmentList'
import {
  AttachmentIcon,
  AttachmentPreviewModal,
  classifyAttachment,
} from './AttachmentPreviewModal'
import { DeliveryPanel } from './DeliveryPanel'
import { HeaderAnalysisPanel } from './HeaderAnalysisPanel'
import { HeaderTable } from './HeaderTable'
import { LinkPanel } from './LinkPanel'
import { MimeDetail, MimeTree } from '../mime/MimeTree'
import { RawViewer } from './RawViewer'
import './MessageViewer.css'

const TABS = ['preview', 'headers', 'mime', 'links', 'raw', 'attachments', 'delivery'] as const
type Tab = (typeof TABS)[number]

const TAB_LABELS: Record<Tab, string> = {
  preview: 'Preview',
  headers: 'Headers',
  mime: 'MIME',
  links: 'Links',
  raw: 'Raw',
  attachments: 'Attachments',
  delivery: 'Delivery',
}

const TAB_ICONS: Record<Tab, React.ReactNode> = {
  preview: <Eye size={13} aria-hidden="true" />,
  headers: <ListOrdered size={13} aria-hidden="true" />,
  mime: <FolderTree size={13} aria-hidden="true" />,
  links: <Link2 size={13} aria-hidden="true" />,
  raw: <Binary size={13} aria-hidden="true" />,
  attachments: <Paperclip size={13} aria-hidden="true" />,
  delivery: <Route size={13} aria-hidden="true" />,
}

const SEEN = '\\Seen'

export function MessageViewer({
  message,
  mailboxId,
  onClose,
}: {
  message: MessageDetail
  mailboxId: string | undefined
  onClose: () => void
}) {
  const [tab, setTab] = useState<Tab>('preview')
  const [movePickerOpen, setMovePickerOpen] = useState(false)
  const [moveSearch, setMoveSearch] = useState('')
  const movePickerRef = useRef<HTMLDivElement>(null)
  const [selectedPart, setSelectedPart] = useState<string | null>(null)
  const [previewAttachment, setPreviewAttachment] = useState<Attachment | null>(null)

  // A malformed message is a finding, not a failure: open it on Raw so the
  // developer sees the bytes that defeated the parser (§13 of the direction).
  useEffect(() => {
    setTab(message.parse_ok ? 'preview' : 'raw')
    setSelectedPart(null)
  }, [message.id, message.parse_ok])

  const headers = useHeaders(message.id)
  // The interpreted view of the same headers, alongside the flat list.
  const analysis = useHeaderAnalysis(message.id, tab === 'headers')
  const mime = useMime(message.id)
  // Raw is fetched for the Raw tab and for the MIME tab, which highlights into it.
  const links = useLinks(message.id, tab === 'links')
  const raw = useRaw(message.id, tab === 'raw' || tab === 'mime')
  const delivery = useDelivery(message.id, tab === 'delivery')

  const parts = mime.data?.parts ?? []
  const activePart: MimePart | undefined = selectedPart === null
    ? undefined
    : parts.find((part) => part.path === selectedPart)

  // The placement in the mailbox being viewed; flags are per-placement, so an
  // action must name which one it applies to (ADR-009).
  const entry = useMemo(() => {
    const entries = message.entries
    return entries.find((candidate) => candidate.mailbox_id === mailboxId) ?? entries[0]
  }, [message.entries, mailboxId])

  const unread = entry ? !entry.flags.includes(SEEN) : false
  const setFlags = useSetFlags()
  const deleteEntry = useDeleteEntry()
  const moveEntry = useMoveEntry()
  const replayMessage = useReplayMessage()
  const mailboxesQuery = useMailboxes()
  const composer = useComposer()

  const currentMailbox = mailboxesQuery.data?.items.find(
    (m) => m.id === (mailboxId ?? entry?.mailbox_id),
  )
  const mailboxName = currentMailbox?.name.toLowerCase() ?? ''
  const isTrash = mailboxName === 'trash' || mailboxName === 'deleted' || mailboxName === 'bin'
  const isArchive = mailboxName === 'archive'
  const isJunk = mailboxName === 'junk' || mailboxName === 'spam'
  const isDrafts = mailboxName === 'drafts' || mailboxName === 'draft'

  const handleEditDraft = async () => {
    let draftBody = ''
    let isHtml = false
    try {
      const mimeTree = mime.data ?? (await api.mime(message.id))
      const htmlPath = mimeTree?.html_parts?.[0]
      const textPath = mimeTree?.text_parts?.[0]

      if (htmlPath !== undefined) {
        draftBody = await api.part(message.id, htmlPath)
        isHtml = true
      } else if (textPath !== undefined) {
        draftBody = await api.part(message.id, textPath)
        isHtml = false
      }
    } catch {
      // ignore
    }

    const ccHeader = message.headers.find((h) => h.name.toLowerCase() === 'cc')?.value
    const bccHeader = message.headers.find((h) => h.name.toLowerCase() === 'bcc')?.value
    const inReplyToHeader = message.headers.find(
      (h) => h.name.toLowerCase() === 'in-reply-to',
    )?.value
    const referencesHeader = message.headers.find(
      (h) => h.name.toLowerCase() === 'references',
    )?.value

    composer.openComposer({
      from: message.from?.address || undefined,
      to: message.to.map((t) => t.address).join(', '),
      cc: ccHeader || '',
      bcc: bccHeader || '',
      subject: message.subject || '',
      body: draftBody,
      isHtml,
      inReplyTo: inReplyToHeader,
      references: referencesHeader,
    })
  }

  const handleReply = async () => {
    // Prefer Reply-To if specified on original message, otherwise From
    const replyToHeader = message.headers.find(
      (h) => h.name.toLowerCase() === 'reply-to'
    )?.value
    const replyToAddress = replyToHeader || message.from?.address || ''

    // The user is replying as the recipient who received this message
    const senderFromAddress = message.to[0]?.address || ''

    const replySubject = message.subject?.startsWith('Re:')
      ? message.subject
      : `Re: ${message.subject || ''}`.trim()

    const rawMsgId = message.headers.find(
      (h) => h.name.toLowerCase() === 'message-id'
    )?.value
    const messageIdHeader = rawMsgId
      ? (rawMsgId.startsWith('<') && rawMsgId.endsWith('>') ? rawMsgId : `<${rawMsgId}>`)
      : undefined

    const rawRefs = message.headers.find(
      (h) => h.name.toLowerCase() === 'references'
    )?.value

    let quotedBody = ''
    try {
      const mimeTree = mime.data ?? (await api.mime(message.id))
      const textPath = mimeTree?.text_parts?.[0]
      const htmlPath = mimeTree?.html_parts?.[0]
      const path = textPath ?? htmlPath
      if (path !== undefined) {
        const content = await api.part(message.id, path)
        const dateStr = message.received_at ? new Date(message.received_at).toLocaleString() : ''
        const senderStr = message.from?.display_name
          ? `${message.from.display_name} <${message.from.address}>`
          : (message.from?.address || 'Unknown')
        quotedBody = `\n\nOn ${dateStr}, ${senderStr} wrote:\n` +
          content.split('\n').map((line) => `> ${line}`).join('\n')
      }
    } catch {
      // Ignore fetch error and proceed with empty quote
    }

    composer.openComposer({
      from: senderFromAddress || undefined,
      to: replyToAddress,
      subject: replySubject,
      inReplyTo: messageIdHeader,
      references: rawRefs ? `${rawRefs} ${messageIdHeader || ''}`.trim() : messageIdHeader,
      body: quotedBody,
      replyContext: {
        from: message.from?.display_name
          ? `${message.from.display_name} <${message.from.address}>`
          : (message.from?.address || 'Unknown'),
        subject: message.subject || '',
      },
    })
  }

  // Inspector shortcuts: tab jumps (1-6), cycle ([ / ]), and reply (r) (§11)
  useKeyboardShortcuts([
    {
      key: '1',
      description: 'Switch to Preview tab',
      category: 'Inspector',
      action: () => setTab('preview'),
    },
    {
      key: '2',
      description: 'Switch to Headers tab',
      category: 'Inspector',
      action: () => setTab('headers'),
    },
    {
      key: '3',
      description: 'Switch to MIME tree tab',
      category: 'Inspector',
      action: () => setTab('mime'),
    },
    {
      key: '4',
      description: 'Switch to Raw source tab',
      category: 'Inspector',
      action: () => setTab('raw'),
    },
    {
      key: '5',
      description: 'Switch to Attachments tab',
      category: 'Inspector',
      action: () => setTab('attachments'),
    },
    {
      key: '6',
      description: 'Switch to Delivery tab',
      category: 'Inspector',
      action: () => setTab('delivery'),
    },
    {
      key: '[',
      description: 'Cycle to previous inspector tab',
      category: 'Inspector',
      action: () => {
        const cur = TABS.indexOf(tab)
        const prev = TABS[(cur - 1 + TABS.length) % TABS.length] ?? 'preview'
        setTab(prev)
      },
    },
    {
      key: ']',
      description: 'Cycle to next inspector tab',
      category: 'Inspector',
      action: () => {
        const cur = TABS.indexOf(tab)
        const next = TABS[(cur + 1) % TABS.length] ?? 'preview'
        setTab(next)
      },
    },
    {
      key: 'r',
      description: 'Reply to message',
      category: 'Actions',
      action: () => {
        if (!isDrafts) {
          void handleReply()
        }
      },
    },
  ])

  return (
    <section className="viewer" aria-label="Message">
      <header className="viewer__header">
        <div className="viewer__headline">
          {onClose && (
            <button
              type="button"
              className="viewer__back-btn"
              onClick={onClose}
              aria-label="Back to messages"
              title="Back to messages"
            >
              <ArrowLeft size={15} aria-hidden="true" />
              <span>Back</span>
            </button>
          )}
          <h1 className="viewer__subject">{message.subject || '(no subject)'}</h1>
          <div className="viewer__actions">
            {isDrafts ? (
              <>
                <button
                  type="button"
                  className="button button--primary"
                  onClick={() => void handleEditDraft()}
                  title="Edit this draft in composer"
                >
                  <Edit size={13} aria-hidden="true" />
                  Edit Draft
                </button>

                {entry ? (
                  <button
                    type="button"
                    className="button button--danger"
                    onClick={() => {
                      deleteEntry.mutate(
                        { entryId: entry.entry_id, permanent: true },
                        { onSuccess: onClose },
                      )
                    }}
                    disabled={deleteEntry.isPending}
                    title="Discard and permanently delete draft"
                  >
                    <Trash2 size={13} aria-hidden="true" />
                    Discard
                  </button>
                ) : null}
              </>
            ) : (
              <>
                <button
                  type="button"
                  className="button"
                  onClick={() => void handleReply()}
                  title="Reply to sender"
                >
                  <Reply size={13} aria-hidden="true" />
                  Reply
                </button>

                {/* Replay delivers the same bytes again as a new message, so
                    an application under test sees a second arrival. It is not
                    a resend to a new recipient — the envelope is the original
                    one — which is why it sits beside Reply rather than in the
                    composer. */}
                <button
                  type="button"
                  className="button"
                  onClick={() => replayMessage.mutate(message.id)}
                  disabled={replayMessage.isPending}
                  title="Deliver this message again, as a new message"
                >
                  <RotateCw size={13} aria-hidden="true" />
                  {replayMessage.isPending ? 'Replaying…' : 'Replay'}
                </button>

                {entry ? (
                  <>
                    <button
                      type="button"
                      className="button"
                      onClick={() =>
                        setFlags.mutate({
                          entryId: entry.entry_id,
                          ...(unread ? { add: [SEEN] } : { remove: [SEEN] }),
                        })
                      }
                      disabled={setFlags.isPending}
                    >
                      {unread ? (
                        <CircleDot size={13} aria-hidden="true" />
                      ) : (
                        <Circle size={13} aria-hidden="true" />
                      )}
                      Mark {unread ? 'read' : 'unread'}
                    </button>

                    {isTrash ? (
                      <>
                        <button
                          type="button"
                          className="button"
                          onClick={() =>
                            moveEntry.mutate(
                              { entryId: entry.entry_id, mailboxName: 'INBOX' },
                              { onSuccess: onClose },
                            )
                          }
                          disabled={moveEntry.isPending}
                          title="Restore back to Inbox"
                        >
                          <Inbox size={13} aria-hidden="true" />
                          Restore
                        </button>

                        <button
                          type="button"
                          className="button button--danger"
                          onClick={() => {
                            deleteEntry.mutate(
                              { entryId: entry.entry_id, permanent: true },
                              { onSuccess: onClose },
                            )
                          }}
                          disabled={deleteEntry.isPending}
                          title="Permanently remove from database"
                        >
                          <Trash2 size={13} aria-hidden="true" />
                          Delete permanently
                        </button>
                      </>
                    ) : isArchive ? (
                      <>
                        <button
                          type="button"
                          className="button"
                          onClick={() =>
                            moveEntry.mutate(
                              { entryId: entry.entry_id, mailboxName: 'INBOX' },
                              { onSuccess: onClose },
                            )
                          }
                          disabled={moveEntry.isPending}
                          title="Move back to Inbox"
                        >
                          <Inbox size={13} aria-hidden="true" />
                          Move to Inbox
                        </button>

                        <button
                          type="button"
                          className="button button--danger"
                          onClick={() => {
                            deleteEntry.mutate(
                              { entryId: entry.entry_id },
                              { onSuccess: onClose },
                            )
                          }}
                          disabled={deleteEntry.isPending}
                          title="Move to Trash"
                        >
                          <Trash2 size={13} aria-hidden="true" />
                          Trash
                        </button>
                      </>
                    ) : isJunk ? (
                      <>
                        <button
                          type="button"
                          className="button"
                          onClick={() =>
                            moveEntry.mutate(
                              { entryId: entry.entry_id, mailboxName: 'INBOX' },
                              { onSuccess: onClose },
                            )
                          }
                          disabled={moveEntry.isPending}
                          title="Mark as not junk and move to Inbox"
                        >
                          <Inbox size={13} aria-hidden="true" />
                          Not Junk
                        </button>

                        <button
                          type="button"
                          className="button button--danger"
                          onClick={() => {
                            deleteEntry.mutate(
                              { entryId: entry.entry_id },
                              { onSuccess: onClose },
                            )
                          }}
                          disabled={deleteEntry.isPending}
                          title="Move to Trash"
                        >
                          <Trash2 size={13} aria-hidden="true" />
                          Trash
                        </button>
                      </>
                    ) : (
                      <>
                        <button
                          type="button"
                          className="button"
                          onClick={() =>
                            moveEntry.mutate(
                              { entryId: entry.entry_id, mailboxName: 'Archive' },
                              { onSuccess: onClose },
                            )
                          }
                          disabled={moveEntry.isPending}
                          title="Move to Archive"
                        >
                          <Archive size={13} aria-hidden="true" />
                          Archive
                        </button>

                        <button
                          type="button"
                          className="button"
                          onClick={() =>
                            moveEntry.mutate(
                              { entryId: entry.entry_id, mailboxName: 'Junk' },
                              { onSuccess: onClose },
                            )
                          }
                          disabled={moveEntry.isPending}
                          title="Mark as Junk"
                        >
                          <AlertOctagon size={13} aria-hidden="true" />
                          Junk
                        </button>

                        <button
                          type="button"
                          className="button button--danger"
                          onClick={() => {
                            deleteEntry.mutate(
                              { entryId: entry.entry_id },
                              { onSuccess: onClose },
                            )
                          }}
                          disabled={deleteEntry.isPending}
                          title="Move to Trash"
                        >
                          <Trash2 size={13} aria-hidden="true" />
                          Delete
                        </button>
                      </>
                    )}
                  </>
                ) : null}
              </>
            )}

            {/* Move to any folder — shown whenever there is a current entry and
                the message is not a draft (drafts have their own workflow). */}
            {entry && !isDrafts && currentMailbox && (() => {
              const accountBoxes = (mailboxesQuery.data?.items ?? []).filter(
                (m) => m.account_id === currentMailbox.account_id && m.id !== currentMailbox.id,
              )
              if (accountBoxes.length === 0) return null

              const q = moveSearch.toLowerCase().trim()
              const matchesSearch = (name: string) => !q || name.toLowerCase().includes(q)

              const standardBoxes = accountBoxes
                .filter((m) => isProtectedMailbox(m.name) && matchesSearch(m.name))
              const customBoxes = accountBoxes
                .filter((m) => !isProtectedMailbox(m.name) && matchesSearch(m.name))
                .sort((a, b) => a.name.localeCompare(b.name))

              return (
                <div className="viewer__move-wrap" ref={movePickerRef}>
                  <button
                    type="button"
                    className="button"
                    aria-label="Move to folder"
                    aria-expanded={movePickerOpen}
                    title="Move to folder"
                    onClick={() => {
                      setMovePickerOpen((v) => !v)
                      setMoveSearch('')
                    }}
                    disabled={moveEntry.isPending}
                  >
                    <FolderOpen size={13} aria-hidden="true" />
                    Move to…
                  </button>

                  {movePickerOpen && (
                    <>
                      <div
                        className="viewer__move-backdrop"
                        onClick={() => {
                          setMovePickerOpen(false)
                          setMoveSearch('')
                        }}
                        aria-hidden="true"
                      />
                      <div className="viewer__move-menu" role="menu">
                        {accountBoxes.length > 4 && (
                          <div className="viewer__move-search-wrap">
                            <input
                              autoFocus
                              type="text"
                              className="viewer__move-search"
                              placeholder="Filter folders..."
                              value={moveSearch}
                              onChange={(e) => setMoveSearch(e.target.value)}
                              onKeyDown={(e) => {
                                if (e.key === 'Escape') {
                                  setMovePickerOpen(false)
                                  setMoveSearch('')
                                }
                              }}
                            />
                          </div>
                        )}

                        {standardBoxes.length > 0 && (
                          <div className="viewer__move-group">
                            <div className="viewer__move-group-title">Standard</div>
                            {standardBoxes.map((mb) => (
                              <button
                                key={mb.id}
                                type="button"
                                role="menuitem"
                                className="viewer__move-item"
                                onClick={() => {
                                  setMovePickerOpen(false)
                                  setMoveSearch('')
                                  moveEntry.mutate(
                                    { entryId: entry.entry_id, targetMailboxId: mb.id },
                                    { onSuccess: onClose },
                                  )
                                }}
                              >
                                {getMailboxIcon(mb.name)}
                                <span>{mb.name}</span>
                              </button>
                            ))}
                          </div>
                        )}

                        {customBoxes.length > 0 && (
                          <div className="viewer__move-group">
                            <div className="viewer__move-group-title">Folders</div>
                            {customBoxes.map((mb) => {
                              const parts = mb.name.split('/')
                              const depth = parts.length - 1
                              const leafName = parts[parts.length - 1] ?? mb.name
                              return (
                                <button
                                  key={mb.id}
                                  type="button"
                                  role="menuitem"
                                  className="viewer__move-item"
                                  style={depth > 0 ? { paddingLeft: `${8 + depth * 12}px` } : undefined}
                                  onClick={() => {
                                    setMovePickerOpen(false)
                                    setMoveSearch('')
                                    moveEntry.mutate(
                                      { entryId: entry.entry_id, targetMailboxId: mb.id },
                                      { onSuccess: onClose },
                                    )
                                  }}
                                >
                                  {depth > 0 ? (
                                    <CornerDownRight size={12} className="viewer__move-branch" aria-hidden="true" />
                                  ) : (
                                    <Folder size={12} aria-hidden="true" />
                                  )}
                                  <span title={mb.name}>{depth > 0 ? leafName : mb.name}</span>
                                </button>
                              )
                            })}
                          </div>
                        )}

                        {standardBoxes.length === 0 && customBoxes.length === 0 && (
                          <div className="viewer__move-empty">No matching folders</div>
                        )}
                      </div>
                    </>
                  )}
                </div>
              )
            })()}
          </div>
        </div>

        <dl className="viewer__envelope">
          <div className="viewer__envelope-item">
            <dt>From</dt>
            <dd>
              <span className="viewer__addr">{formatAddress(message.from)}</span>
              {message.from ? (
                <CopyButton value={message.from.address} label="the sender address" />
              ) : null}
            </dd>
          </div>
          <div className="viewer__envelope-item">
            <dt>To</dt>
            <dd>
              <span className="viewer__addr">{message.to.length > 0 ? message.to.map(formatAddress).join(', ') : '—'}</span>
            </dd>
          </div>
          <div className="viewer__envelope-item">
            <dt>Date</dt>
            <dd>{formatFull(message.received_at)}</dd>
          </div>
          <div className="viewer__envelope-item">
            <dt>Size</dt>
            <dd>{formatSize(message.size)}</dd>
          </div>
          <div className="viewer__envelope-item viewer__envelope-item--id">
            <dt>ID</dt>
            <dd>
              <span className="viewer__id mono">{message.id}</span>
              <CopyButton value={message.id} label="the message id" />
            </dd>
          </div>
        </dl>

        {/* Placements: one message can live in several mailboxes (§23). */}
        {message.entries.length > 0 ? (
          <div className="viewer__placements">
            <span className="viewer__placements-label">In</span>
            {message.entries.map((placement) => (
              <span
                key={placement.entry_id}
                className={`chip chip--mailbox${placement.mailbox_id === mailboxId ? ' chip--current' : ''}`}
              >
                <Folder size={11} aria-hidden="true" className="chip__icon" />
                <span>{placement.mailbox_name ?? placement.mailbox_id}</span>
                <span className="chip__uid">#{placement.uid}</span>
              </span>
            ))}
          </div>
        ) : null}

        {(message.diagnostics ?? []).length > 0 ? (
          <ul className="viewer__diagnostics">
            {message.diagnostics.map((diagnostic, index) => (
              <li key={index} className={`diagnostic diagnostic--${diagnostic.severity}`}>
                <strong>{diagnostic.severity}</strong> {diagnostic.message}
              </li>
            ))}
          </ul>
        ) : null}
      </header>

      <div className="viewer__tabs" role="tablist" aria-label="Message views">
        {TABS.map((name) => (
          <button
            key={name}
            type="button"
            role="tab"
            id={`tab-${name}`}
            aria-selected={tab === name}
            aria-controls={`panel-${name}`}
            tabIndex={tab === name ? 0 : -1}
            className={`viewer__tab${tab === name ? ' viewer__tab--active' : ''}`}
            onClick={() => setTab(name)}
            onKeyDown={(event) => {
              const index = TABS.indexOf(tab)
              if (event.key === 'ArrowRight') {
                event.preventDefault()
                setTab(TABS[(index + 1) % TABS.length] as Tab)
              } else if (event.key === 'ArrowLeft') {
                event.preventDefault()
                setTab(TABS[(index - 1 + TABS.length) % TABS.length] as Tab)
              }
            }}
          >
            {TAB_ICONS[name]}
            <span>{TAB_LABELS[name]}</span>
            {name === 'attachments' && message.attachments.length > 0 ? (
              <span className="viewer__tab-count">{message.attachments.length}</span>
            ) : null}
          </button>
        ))}
      </div>

      <div
        className="viewer__panel"
        role="tabpanel"
        id={`panel-${tab}`}
        aria-labelledby={`tab-${tab}`}
        tabIndex={0}
      >
        {tab === 'preview' ? (
          <>
            <PreviewPanel messageId={message.id} mime={mime} />
            {message.attachments.length > 0 && (
              <InlineAttachmentStrip
                attachments={message.attachments}
                onSelect={(att) => setPreviewAttachment(att)}
                onViewAll={() => setTab('attachments')}
              />
            )}
          </>
        ) : null}

        {tab === 'headers' ? (
          headers.isPending ? (
            <LoadingState label="Loading headers" />
          ) : headers.isError ? (
            <ErrorState error={headers.error} onRetry={() => void headers.refetch()} />
          ) : (
            // One scroll container: the analysis and the flat table are two
            // views of the same header block and read as one document.
            <div className="viewer__headers">
              {analysis.data ? <HeaderAnalysisPanel analysis={analysis.data} /> : null}
              <HeaderTable headers={headers.data ?? []} />
            </div>
          )
        ) : null}

        {tab === 'links' ? (
          links.isPending ? (
            <LoadingState label="Extracting links" />
          ) : links.isError ? (
            <ErrorState error={links.error} onRetry={() => void links.refetch()} />
          ) : links.data ? (
            <LinkPanel report={links.data} />
          ) : null
        ) : null}

        {tab === 'mime' ? (
          mime.isPending ? (
            <LoadingState label="Loading MIME structure" />
          ) : mime.isError ? (
            <ErrorState error={mime.error} onRetry={() => void mime.refetch()} />
          ) : mime.data ? (
            <div className="viewer__split">
              <div className="viewer__split-tree">
                <MimeTree
                  tree={mime.data}
                  selectedPath={selectedPart}
                  onSelect={setSelectedPart}
                />
                {activePart ? <MimeDetail part={activePart} /> : null}
              </div>

              {/*
                The signature interaction: the selected part's byte range
                highlighted in the authoritative source, side by side.
              */}
              <div className="viewer__split-raw">
                {raw.isPending ? (
                  <LoadingState label="Loading source" />
                ) : raw.isError ? (
                  <ErrorState error={raw.error} onRetry={() => void raw.refetch()} />
                ) : (
                  <RawViewer
                    raw={raw.data ?? ''}
                    highlight={
                      activePart
                        ? { offset: activePart.body_offset, length: activePart.body_length }
                        : null
                    }
                  />
                )}
              </div>
            </div>
          ) : null
        ) : null}

        {tab === 'raw' ? (
          raw.isPending ? (
            <LoadingState label="Loading source" />
          ) : raw.isError ? (
            <ErrorState error={raw.error} onRetry={() => void raw.refetch()} />
          ) : (
            <RawViewer raw={raw.data ?? ''} />
          )
        ) : null}

        {tab === 'attachments' ? (
          <AttachmentList
            messageId={message.id}
            attachments={message.attachments}
            onInspect={(partPath) => {
              setSelectedPart(partPath)
              setTab('mime')
            }}
          />
        ) : null}

        {tab === 'delivery' ? (
          delivery.isPending ? (
            <LoadingState label="Loading delivery" />
          ) : delivery.isError ? (
            // A message injected before delivery recording existed genuinely
            // has no record, and the endpoint answers 404. That is a real
            // state rather than a failure, so it is not an error panel.
            <p className="delivery__empty">
              No delivery record was found for this message.
            </p>
          ) : delivery.data ? (
            <DeliveryPanel delivery={delivery.data} />
          ) : null
        ) : null}
      </div>

      {previewAttachment && (
        <AttachmentPreviewModal
          attachment={previewAttachment}
          onClose={() => setPreviewAttachment(null)}
        />
      )}
    </section>
  )
}

/**
 * The rendered body.
 *
 * Prefers HTML when the message carries it, falling back to plain text. Text is
 * rendered into a `<pre>`, never as HTML (§16), so a text part that happens to
 * contain markup is shown as the characters it actually is.
 */
function PreviewPanel({
  messageId,
  mime,
}: {
  messageId: string
  mime: ReturnType<typeof useMime>
}) {
  const [allowRemote, setAllowRemote] = useState(false)
  const [body, setBody] = useState<{ kind: 'html' | 'text'; content: string } | null>(null)
  const [error, setError] = useState<unknown>(null)
  const [viewport, setViewport] = useState<'desktop' | 'tablet' | 'mobile'>('desktop')
  const [orientation, setOrientation] = useState<'portrait' | 'landscape'>('portrait')

  const htmlPath = mime.data?.html_parts?.[0]
  const textPath = mime.data?.text_parts?.[0]

  useEffect(() => {
    let cancelled = false
    const path = htmlPath ?? textPath
    if (path === undefined) {
      setBody(null)
      return
    }

    setError(null)
    api
      .part(messageId, path)
      .then((content) => {
        if (!cancelled) {
          setBody({ kind: htmlPath !== undefined ? 'html' : 'text', content })
        }
      })
      .catch((cause: unknown) => {
        if (!cancelled) setError(cause)
      })

    return () => {
      cancelled = true
    }
  }, [messageId, htmlPath, textPath])

  if (mime.isPending) return <LoadingState label="Loading message" />
  if (mime.isError) return <ErrorState error={mime.error} onRetry={() => void mime.refetch()} />
  if (error) return <ErrorState error={error} />

  if (htmlPath === undefined && textPath === undefined) {
    return (
      <div className="viewer__notice">
        This message has no text or HTML body. Use the MIME tab to inspect its parts.
      </div>
    )
  }

  if (!body) return <LoadingState label="Loading message" />

  if (body.kind === 'text') {
    return (
      <div className="viewer__text-container">
        <pre className="viewer__text">{body.content}</pre>
      </div>
    )
  }

  return (
    <div className="viewer__html">
      <div className="viewer__html-bar">
        <label className="headers__toggle">
          <input
            type="checkbox"
            checked={allowRemote}
            onChange={(event) => setAllowRemote(event.target.checked)}
          />
          Load remote images
        </label>

        <div className="viewer__viewport-controls">
          <div className="viewer__viewport-switch" role="group" aria-label="Preview viewport size">
            <button
              type="button"
              className={`viewport-btn${viewport === 'desktop' ? ' viewport-btn--active' : ''}`}
              onClick={() => setViewport('desktop')}
              title="Desktop width (100%)"
              aria-label="Desktop view"
            >
              <Laptop size={13} aria-hidden="true" />
              <span>Desktop</span>
            </button>
            <button
              type="button"
              className={`viewport-btn${viewport === 'tablet' ? ' viewport-btn--active' : ''}`}
              onClick={() => setViewport('tablet')}
              title="Tablet view (768px)"
              aria-label="Tablet view"
            >
              <Tablet size={13} aria-hidden="true" />
              <span>Tablet</span>
            </button>
            <button
              type="button"
              className={`viewport-btn${viewport === 'mobile' ? ' viewport-btn--active' : ''}`}
              onClick={() => setViewport('mobile')}
              title="Mobile view (390px)"
              aria-label="Mobile view"
            >
              <Smartphone size={13} aria-hidden="true" />
              <span>Mobile</span>
            </button>
          </div>

          {viewport !== 'desktop' ? (
            <div className="viewer__viewport-extras">
              <button
                type="button"
                className="viewport-btn viewport-btn--rotate"
                onClick={() => setOrientation((o) => (o === 'portrait' ? 'landscape' : 'portrait'))}
                title={`Switch to ${orientation === 'portrait' ? 'Landscape' : 'Portrait'} mode`}
                aria-label={`Switch orientation (currently ${orientation})`}
              >
                <RotateCw size={12} aria-hidden="true" />
                <span>{orientation === 'portrait' ? 'Landscape' : 'Portrait'}</span>
              </button>

              <span className="viewer__dimension-badge mono">
                {viewport === 'mobile'
                  ? orientation === 'portrait'
                    ? '390 × 800'
                    : '800 × 390'
                  : orientation === 'portrait'
                    ? '768 × 960'
                    : '960 × 768'}
              </span>
            </div>
          ) : null}
        </div>

        <span className="viewer__html-note">
          Sandboxed iframe (scripts disabled)
        </span>
      </div>

      <div className={`viewer__html-stage viewer__html-stage--${viewport}`}>
        {viewport === 'desktop' ? (
          <div className="viewer__html-frame">
            <HtmlPreview
              html={body.content}
              allowRemote={allowRemote}
              title="Message body"
              viewport="desktop"
            />
          </div>
        ) : viewport === 'mobile' ? (
          <div
            className={`viewer__device viewer__device--mobile viewer__device--${orientation}`}
            role="region"
            aria-label={`Mobile preview (${orientation})`}
          >
            <div className="viewer__device-bezel">
              <div className="viewer__device-notch" aria-hidden="true">
                <span className="viewer__device-notch-island" />
              </div>
              <div className="viewer__device-screen">
                <div className="viewer__device-status-bar" aria-hidden="true">
                  <span className="viewer__device-time">9:41</span>
                  <span className="viewer__device-status-icons">
                    <span className="viewer__device-signal" />
                    <span className="viewer__device-wifi" />
                    <span className="viewer__device-battery" />
                  </span>
                </div>
                <div className="viewer__device-content">
                  <HtmlPreview
                    html={body.content}
                    allowRemote={allowRemote}
                    title="Message body - Mobile"
                    viewport="mobile"
                    orientation={orientation}
                  />
                </div>
                <span className="viewer__device-home-indicator" aria-hidden="true" />
              </div>
            </div>
          </div>
        ) : (
          <div
            className={`viewer__device viewer__device--tablet viewer__device--${orientation}`}
            role="region"
            aria-label={`Tablet preview (${orientation})`}
          >
            <div className="viewer__device-bezel">
              <div className="viewer__device-camera-dot" aria-hidden="true" />
              <div className="viewer__device-screen">
                <div className="viewer__device-content">
                  <HtmlPreview
                    html={body.content}
                    allowRemote={allowRemote}
                    title="Message body - Tablet"
                    viewport="tablet"
                    orientation={orientation}
                  />
                </div>
                <span className="viewer__device-home-indicator" aria-hidden="true" />
              </div>
            </div>
          </div>
        )}
      </div>
    </div>
  )
}

function InlineAttachmentStrip({
  attachments,
  onSelect,
  onViewAll,
}: {
  attachments: Attachment[]
  onSelect: (attachment: Attachment) => void
  onViewAll: () => void
}) {
  const [thumbErrors, setThumbErrors] = useState<Record<string, boolean>>({})

  return (
    <div className="viewer__attachments-strip">
      <div className="viewer__attachments-header">
        <span className="viewer__attachments-title">
          <Paperclip size={13} aria-hidden="true" />
          Attachments ({attachments.length})
        </span>
        <button
          type="button"
          className="viewer__attachments-view-all"
          onClick={onViewAll}
        >
          View all
        </button>
      </div>
      <div className="viewer__attachments-grid">
        {attachments.map((att) => {
          const fileType = classifyAttachment(att)
          const isImage = fileType === 'image'
          const filename = att.filename ?? `part-${att.part_path}`
          const hasThumbError = thumbErrors[att.id]

          return (
            <div
              key={att.id}
              className="viewer__attachment-card"
              onClick={() => onSelect(att)}
              role="button"
              tabIndex={0}
              onKeyDown={(e) => {
                if (e.key === 'Enter' || e.key === ' ') {
                  e.preventDefault()
                  onSelect(att)
                }
              }}
              title={`Click to preview: ${filename}`}
            >
              <div className="viewer__attachment-thumb">
                {isImage && !hasThumbError ? (
                  <img
                    src={attachmentsApi.thumbnailUrl(att.id)}
                    alt=""
                    className="viewer__attachment-thumb-img"
                    loading="lazy"
                    onError={() =>
                      setThumbErrors((prev) => ({ ...prev, [att.id]: true }))
                    }
                  />
                ) : (
                  <span
                    className={`viewer__attachment-badge viewer__attachment-badge--${fileType}`}
                  >
                    <AttachmentIcon fileType={fileType} size={15} />
                  </span>
                )}
              </div>
              <div className="viewer__attachment-info">
                <span className="viewer__attachment-name" title={filename}>
                  {filename}
                </span>
                <span className="viewer__attachment-size">
                  {formatSize(att.size)}
                </span>
              </div>
              <a
                href={attachmentsApi.url(att.id, false)}
                download={filename}
                className="viewer__attachment-download-btn"
                title="Download"
                onClick={(e) => e.stopPropagation()}
              >
                <Download size={13} aria-hidden="true" />
              </a>
            </div>
          )
        })}
      </div>
    </div>
  )
}
