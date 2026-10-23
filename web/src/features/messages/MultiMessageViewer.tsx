/**
 * Displays a summary and list of all multi-selected messages.
 * Allows bulk actions (read, unread, delete, clear) and inspecting/removing individual messages.
 */

import { useMemo } from 'react'
import { MailCheck, MailX, Paperclip, Trash2, TriangleAlert, X } from 'lucide-react'

import type { MessageSummary } from '../../api/client'
import { formatAddressShort, formatAge, formatSize } from '../../lib/format'
import './MultiMessageViewer.css'

interface MultiMessageViewerProps {
  messages: MessageSummary[]
  selectedIds: Set<string>
  onSelectMessage: (id: string) => void
  onDeselectEntry: (entryId: string) => void
  onClearSelection: () => void
  onBulkRead: () => void
  onBulkUnread: () => void
  onBulkDelete: () => void
  isPending: boolean
  mailboxId?: string | undefined
}

export function MultiMessageViewer({
  messages,
  selectedIds,
  onSelectMessage,
  onDeselectEntry,
  onClearSelection,
  onBulkRead,
  onBulkUnread,
  onBulkDelete,
  isPending,
  mailboxId,
}: MultiMessageViewerProps) {
  const stats = useMemo(() => {
    let totalBytes = 0
    let unreadCount = 0
    for (const msg of messages) {
      totalBytes += msg.size
      const relevant =
        (mailboxId ? msg.entries.find((e) => e.mailbox_id === mailboxId) : null) ??
        msg.entries[0]
      if (relevant?.flags && !relevant.flags.includes('\\Seen')) {
        unreadCount++
      }
    }
    return {
      totalBytes,
      unreadCount,
      readCount: messages.length - unreadCount,
    }
  }, [messages, mailboxId])

  return (
    <div className="multi-viewer" role="region" aria-label="Multiple messages selected">
      <header className="multi-viewer__header">
        <div className="multi-viewer__headline">
          <div className="multi-viewer__title-group">
            <h1 className="multi-viewer__title">
              {messages.length} {messages.length === 1 ? 'Message' : 'Messages'} Selected
            </h1>
            <span className="multi-viewer__subtitle">
              Click any message below to view, or apply batch actions to all selected.
            </span>
          </div>

          <div className="multi-viewer__actions">
            <button
              type="button"
              className="button button--secondary button--sm"
              onClick={onBulkRead}
              disabled={isPending}
              title="Mark all selected as read"
            >
              <MailCheck size={14} aria-hidden="true" />
              <span>Mark Read</span>
            </button>
            <button
              type="button"
              className="button button--secondary button--sm"
              onClick={onBulkUnread}
              disabled={isPending}
              title="Mark all selected as unread"
            >
              <MailX size={14} aria-hidden="true" />
              <span>Mark Unread</span>
            </button>
            <button
              type="button"
              className="button button--danger button--sm"
              onClick={onBulkDelete}
              disabled={isPending}
              title="Delete all selected"
            >
              <Trash2 size={14} aria-hidden="true" />
              <span>Delete ({messages.length})</span>
            </button>
            <button
              type="button"
              className="button button--ghost button--sm"
              onClick={onClearSelection}
              title="Clear selection (Esc)"
            >
              <X size={14} aria-hidden="true" />
              <span>Deselect All</span>
            </button>
          </div>
        </div>

        <div className="multi-viewer__envelope">
          <div className="multi-viewer__envelope-stat">
            <span className="multi-viewer__envelope-label">Total:</span>
            <span className="multi-viewer__envelope-val">{messages.length} messages</span>
          </div>
          <div className="multi-viewer__envelope-stat">
            <span className="multi-viewer__envelope-label">Unread:</span>
            <span className="multi-viewer__envelope-val">{stats.unreadCount}</span>
          </div>
          <div className="multi-viewer__envelope-stat">
            <span className="multi-viewer__envelope-label">Combined Size:</span>
            <span className="multi-viewer__envelope-val">{formatSize(stats.totalBytes)}</span>
          </div>
        </div>
      </header>

      <div className="multi-viewer__content" role="list" aria-label="Selected messages list">
        {messages.map((msg) => {
          const relevant =
            (mailboxId ? msg.entries.find((e) => e.mailbox_id === mailboxId) : null) ??
            msg.entries[0]
          const entryId = relevant?.entry_id
          const isUnread = relevant?.flags ? !relevant.flags.includes('\\Seen') : false

          return (
            <div key={msg.id} className="multi-viewer__item" role="listitem">
              <div
                className="multi-viewer__item-main"
                onClick={() => onSelectMessage(msg.id)}
                title="Click to view this message"
              >
                <div className="multi-viewer__item-top">
                  <span
                    className={`multi-viewer__dot ${isUnread ? 'multi-viewer__dot--unread' : ''}`}
                    title={isUnread ? 'Unread' : 'Read'}
                  />
                  <span className="multi-viewer__item-from">
                    {formatAddressShort(msg.from)}
                  </span>
                  {msg.from?.display_name && msg.from?.address ? (
                    <span className="multi-viewer__item-addr">&lt;{msg.from.address}&gt;</span>
                  ) : null}
                </div>

                <div className="multi-viewer__item-subject">
                  {msg.subject || <em>(no subject)</em>}
                </div>
              </div>

              <div className="multi-viewer__item-meta">
                {!msg.parse_ok ? (
                  <span className="badge badge--warn" title="Parse warning">
                    <TriangleAlert size={11} aria-hidden="true" />
                  </span>
                ) : null}
                {msg.has_attachments ? (
                  <span className="badge" title="Has attachments">
                    <Paperclip size={11} aria-hidden="true" />
                  </span>
                ) : null}

                {msg.entries.map((entry) => (
                  <span key={entry.entry_id} className="multi-viewer__tag">
                    {entry.mailbox_name}
                  </span>
                ))}

                <time className="multi-viewer__age" dateTime={msg.received_at}>
                  {formatAge(msg.received_at)}
                </time>

                <span className="multi-viewer__size">{formatSize(msg.size)}</span>

                {entryId && selectedIds.has(entryId) ? (
                  <button
                    type="button"
                    className="multi-viewer__remove"
                    title="Remove from selection"
                    aria-label={`Deselect message from ${msg.from?.address ?? 'sender'}`}
                    onClick={(e) => {
                      e.stopPropagation()
                      onDeselectEntry(entryId)
                    }}
                  >
                    <X size={14} aria-hidden="true" />
                  </button>
                ) : null}
              </div>
            </div>
          )
        })}
      </div>

      <footer className="multi-viewer__footer">
        <p className="multi-viewer__hint">
          Tip: Hold <kbd>Ctrl</kbd> / <kbd>Cmd</kbd> to toggle individual messages, <kbd>Shift</kbd> to select ranges, or press <kbd>Esc</kbd> to deselect all.
        </p>
      </footer>
    </div>
  )
}
