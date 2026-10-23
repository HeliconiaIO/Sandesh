/**
 * The message list (§11, §12).
 *
 * Compact rows built for scanning many test messages, not cards. Each row
 * carries only what `GET /api/v1/messages` already returns — no row fetches a
 * body (§11), so a page of messages costs one request.
 *
 * Keyboard model (§47): the list is a `listbox`, so arrow keys move the active
 * row, Enter opens it, and the whole list is one tab stop rather than N.
 *
 * ## Virtualised, and why that is not merely an optimisation
 *
 * The Phase 4 criterion is that the list stays smooth at 10,000 messages, which
 * is a real shape here: a CI suite delivering all day produces exactly that.
 * Ten thousand rows is ~60,000 DOM nodes, and no amount of row-level tuning
 * makes that scroll — so only the visible window is rendered, inside a spacer
 * of the full height so the scrollbar still describes the whole list.
 *
 * Two consequences the accessibility model forces, and neither is optional:
 *
 * 1. `aria-activedescendant` must name an element that *exists*. Keyboard
 *    navigation to an off-screen row therefore scrolls it into view first and
 *    lets the virtualiser mount it, rather than pointing at an id that is not
 *    in the DOM — which would silently break screen-reader announcement while
 *    looking correct on screen.
 * 2. Each row carries `aria-setsize` and `aria-posinset`, because a listbox
 *    holding forty mounted children otherwise announces "3 of 40" in a list of
 *    ten thousand.
 */

import { useCallback, useEffect, useMemo, useRef } from 'react'
import { Paperclip, TriangleAlert } from 'lucide-react'
import { useVirtualizer } from '@tanstack/react-virtual'

import type { MessageSummary } from '../../api/client'
import { formatAddressShort, formatAge, formatSize } from '../../lib/format'
import './MessageList.css'

const SEEN = '\\Seen'

/** Whether every placement of this message has been read. */
function isUnread(message: MessageSummary): boolean {
  return message.entries.some((entry) => !entry.flags.includes(SEEN))
}

interface MessageListProps {
  messages: MessageSummary[]
  selectedId: string | undefined
  onSelect: (id: string, event?: React.MouseEvent) => void
  /** The mailbox being viewed, so a multi-placement message can show the others. */
  mailboxId?: string | undefined
  /** Set of entry IDs currently multi-selected with Ctrl/Shift or selection mode */
  selectedEntryIds?: Set<string> | undefined
  /** Triggered when a row is held/long-pressed on touch devices */
  onLongPress?: ((id: string) => void) | undefined
  /** Fetches the next page when the end of the loaded rows comes into view. */
  onEndReached?: (() => void) | undefined
  /** Whether another page exists, so the sentinel row is only shown when true. */
  hasMore?: boolean | undefined
}

export function MessageList({
  messages,
  selectedId,
  onSelect,
  mailboxId,
  selectedEntryIds,
  onLongPress,
  onEndReached,
  hasMore,
}: MessageListProps) {
  const listRef = useRef<HTMLUListElement>(null)

  const virtualizer = useVirtualizer({
    count: messages.length,
    getScrollElement: () => listRef.current,
    // Rows are two lines plus padding. An estimate is enough: the virtualiser
    // measures each mounted row and corrects the spacer, so a row that wraps to
    // a third line (a long subject, a multi-placement chip) is accommodated
    // rather than clipped.
    estimateSize: () => 64,
    // Enough rows above and below the viewport that a fast scroll does not
    // reach unmounted space, and few enough that the DOM stays small.
    overscan: 8,
    getItemKey: (index) => messages[index]?.id ?? index,
  })

  const virtualRows = virtualizer.getVirtualItems()

  // Paging is driven by what has been *rendered*, not by a scroll offset: the
  // question "is the user near the end of what we hold" is exactly "is the last
  // virtual row near the last loaded row", and that stays correct when row
  // heights vary.
  const lastRenderedIndex = virtualRows[virtualRows.length - 1]?.index ?? 0
  useEffect(() => {
    if (!hasMore || !onEndReached) return
    if (lastRenderedIndex >= messages.length - 5) {
      onEndReached()
    }
  }, [hasMore, onEndReached, lastRenderedIndex, messages.length])

  const move = useCallback(
    (delta: number) => {
      if (messages.length === 0) return
      const current = messages.findIndex((message) => message.id === selectedId)
      // From no selection, Down opens the first row and Up the last.
      const next =
        current === -1
          ? delta > 0
            ? 0
            : messages.length - 1
          : Math.min(messages.length - 1, Math.max(0, current + delta))
      const target = messages[next]
      if (!target) return
      // Scroll first, then select: the row must be mounted for
      // `aria-activedescendant` to name a real element.
      virtualizer.scrollToIndex(next, { align: 'auto' })
      onSelect(target.id)
    },
    [messages, selectedId, onSelect, virtualizer],
  )

  const onKeyDown = (event: React.KeyboardEvent<HTMLUListElement>) => {
    switch (event.key) {
      case 'ArrowDown':
      case 'j':
        event.preventDefault()
        move(1)
        break
      case 'ArrowUp':
      case 'k':
        event.preventDefault()
        move(-1)
        break
      case 'Home':
        event.preventDefault()
        if (messages[0]) {
          virtualizer.scrollToIndex(0, { align: 'auto' })
          onSelect(messages[0].id)
        }
        break
      case 'End': {
        event.preventDefault()
        const lastIndex = messages.length - 1
        const last = messages[lastIndex]
        if (last) {
          virtualizer.scrollToIndex(lastIndex, { align: 'auto' })
          onSelect(last.id)
        }
        break
      }
    }
  }

  // Keep the active row in view when selection changes from outside the list —
  // opening a message by URL, or the viewer's own next/previous. Guarded
  // because scrolling is a convenience, not a correctness property: where it is
  // unavailable the list must still select and render normally.
  const selectedIndex = useMemo(
    () => (selectedId ? messages.findIndex((message) => message.id === selectedId) : -1),
    [messages, selectedId],
  )
  useEffect(() => {
    if (selectedIndex < 0) return
    virtualizer.scrollToIndex(selectedIndex, { align: 'auto' })
  }, [selectedIndex, virtualizer])

  const isMultiSelecting = Boolean(selectedEntryIds && selectedEntryIds.size > 0)

  return (
    <ul
      ref={listRef}
      className={`messages${isMultiSelecting ? ' messages--multi-selecting' : ''}`}
      role="listbox"
      aria-label="Messages"
      tabIndex={0}
      onKeyDown={onKeyDown}
      aria-activedescendant={selectedId ? `message-${selectedId}` : undefined}
    >
      {/* The spacer holds the full scroll height, so the scrollbar describes
          the whole list rather than only the mounted window. */}
      <li
        aria-hidden="true"
        className="messages__spacer"
        style={{ height: `${virtualizer.getTotalSize()}px` }}
      />

      {virtualRows.map((virtualRow) => {
        const message = messages[virtualRow.index]
        if (!message) return null

        const relevantEntry =
          (mailboxId ? message.entries.find((e) => e.mailbox_id === mailboxId) : null) ??
          message.entries[0]
        const entryId = relevantEntry?.entry_id
        const isMultiSelected = entryId ? selectedEntryIds?.has(entryId) : false

        return (
          <MessageRow
            key={virtualRow.key}
            // The virtualiser measures each row it mounts, so a row that wraps
            // to a third line corrects the spacer instead of being clipped.
            ref={virtualizer.measureElement}
            dataIndex={virtualRow.index}
            offset={virtualRow.start}
            setSize={messages.length}
            message={message}
            selected={message.id === selectedId}
            multiSelected={isMultiSelected}
            onSelect={onSelect}
            onLongPress={onLongPress}
            mailboxId={mailboxId}
          />
        )
      })}
    </ul>
  )
}

function MessageRow({
  message,
  selected,
  multiSelected,
  onSelect,
  onLongPress,
  mailboxId,
  ref,
  dataIndex,
  offset,
  setSize,
}: {
  message: MessageSummary
  selected: boolean
  multiSelected?: boolean | undefined
  onSelect: (id: string, event?: React.MouseEvent) => void
  onLongPress?: ((id: string) => void) | undefined
  mailboxId?: string | undefined
  ref?: ((node: Element | null) => void) | undefined
  dataIndex: number
  offset: number
  setSize: number
}) {
  const unread = isUnread(message)
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const touchStartPos = useRef<{ x: number; y: number } | null>(null)

  const handleTouchStart = (e: React.TouchEvent) => {
    const touch = e.touches[0]
    if (!touch) return
    touchStartPos.current = { x: touch.clientX, y: touch.clientY }
    timerRef.current = setTimeout(() => {
      onLongPress?.(message.id)
    }, 450)
  }

  const handleTouchMove = (e: React.TouchEvent) => {
    const touch = e.touches[0]
    if (!touch || !touchStartPos.current) return
    const dx = Math.abs(touch.clientX - touchStartPos.current.x)
    const dy = Math.abs(touch.clientY - touchStartPos.current.y)
    // If the touch moved noticeably, it's a scroll gesture — cancel long press
    if (dx > 10 || dy > 10) {
      if (timerRef.current) {
        clearTimeout(timerRef.current)
        timerRef.current = null
      }
    }
  }

  const handleTouchEnd = () => {
    if (timerRef.current) {
      clearTimeout(timerRef.current)
      timerRef.current = null
    }
  }

  // A message can sit in several mailboxes (ADR-009). When viewing one mailbox,
  // name the others so the placement is visible rather than implied.
  const otherMailboxes = mailboxId
    ? message.entries
        .filter((entry) => entry.mailbox_id !== mailboxId)
        .map((entry) => entry.mailbox_name)
        .filter((name): name is string => Boolean(name))
    : []

  return (
    <li
      id={`message-${message.id}`}
      data-id={message.id}
      // The virtualiser keys its measurements by this attribute.
      data-index={dataIndex}
      ref={ref}
      role="option"
      aria-selected={selected || multiSelected}
      // Without these, a listbox holding forty mounted children announces
      // "3 of 40" in a list of ten thousand.
      aria-setsize={setSize}
      aria-posinset={dataIndex + 1}
      className={`message-row${selected || multiSelected ? ' message-row--selected' : ''}${
        unread ? ' message-row--unread' : ''
      }`}
      style={{ transform: `translateY(${offset}px)` }}
      onClick={(event) => onSelect(message.id, event)}
      onTouchStart={handleTouchStart}
      onTouchMove={handleTouchMove}
      onTouchEnd={handleTouchEnd}
      onTouchCancel={handleTouchEnd}
    >
      <span className="message-row__marker" aria-hidden="true" />

      <div className="message-row__main">
        <div className="message-row__top">
          <span className="message-row__from">
            {formatAddressShort(message.from)}
          </span>
          <time className="message-row__age" dateTime={message.received_at}>
            {formatAge(message.received_at)}
          </time>
        </div>

        <div className="message-row__bottom">
          <span className="message-row__subject">
            {message.subject || <span className="message-row__nosubject">(no subject)</span>}
          </span>

          <span className="message-row__badges">
            {/* Badges carry a label as well as an icon: never colour or shape
                alone (§11). */}
            {!message.parse_ok ? (
              <span className="badge badge--warn" title="This message did not parse cleanly">
                <TriangleAlert size={11} aria-hidden="true" />
                <span className="sr-only">Parse warning</span>
              </span>
            ) : null}
            {message.has_attachments ? (
              <span className="badge" title="Has attachments">
                <Paperclip size={11} aria-hidden="true" />
                <span className="sr-only">Has attachments</span>
              </span>
            ) : null}
            <span className="message-row__size">{formatSize(message.size)}</span>
          </span>
        </div>

        {otherMailboxes.length > 0 ? (
          <div className="message-row__placements">
            <span className="sr-only">Also in </span>
            {otherMailboxes.map((name) => (
              <span key={name} className="chip">
                {name}
              </span>
            ))}
          </div>
        ) : null}
      </div>

      {unread ? <span className="sr-only">Unread</span> : null}
    </li>
  )
}
