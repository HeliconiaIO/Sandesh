/**
 * The mail view: list plus inspector.
 *
 * Selection lives in the URL (§13, §58): `/mailbox/:mailboxId/message/:messageId`
 * survives a refresh, works with back/forward, and can be pasted to a colleague.
 * Keeping it in component state would make all three impossible.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useNavigate, useParams } from 'react-router-dom'
import { FolderOpen, MailCheck, MailX, Search, SlidersHorizontal, Trash2, X } from 'lucide-react'

import { EmptyState, ErrorState, LoadingState } from '../components/States'
import { MessageList } from '../features/messages/MessageList'
import { MessageViewer } from '../features/messages/MessageViewer'
import { MultiMessageViewer } from '../features/messages/MultiMessageViewer'
import { ConnectionDetails } from '../features/settings/ConnectionDetails'
import {
  useBulkEntries,
  useMailboxes,
  useMessage,
  useMessages,
  useSetFlags,
} from '../hooks/queries'
import { useKeyboardShortcuts } from '../hooks/useKeyboardShortcuts'
import { getMailboxIcon } from '../features/mailboxes/MailboxNav'
import {
  buildFilterQuery,
  matchesMessage,
  parseFilterQuery,
} from '../features/messages/filterUtils'
import './MailboxRoute.css'

const SEEN = '\\Seen'

export function MailboxRoute() {
  const { mailboxId, messageId } = useParams<{ mailboxId: string; messageId: string }>()
  const navigate = useNavigate()
  const [filter, setFilter] = useState('')
  const [unreadOnly, setUnreadOnly] = useState(false)
  const [attachmentsOnly, setAttachmentsOnly] = useState(false)
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set())
  const [isSelectionMode, setIsSelectionMode] = useState(false)
  const [lastClickedIndex, setLastClickedIndex] = useState<number>(-1)
  const [bulkMoveOpen, setBulkMoveOpen] = useState(false)
  const [filterOptionsOpen, setFilterOptionsOpen] = useState(false)
  const [showSyntaxHelper, setShowSyntaxHelper] = useState(false)
  const searchInputRef = useRef<HTMLInputElement>(null)

  // Builder modal/popover state
  const [builderFrom, setBuilderFrom] = useState('')
  const [builderTo, setBuilderTo] = useState('')
  const [builderSubject, setBuilderSubject] = useState('')
  const [builderUnread, setBuilderUnread] = useState(false)
  const [builderAttachments, setBuilderAttachments] = useState(false)

  const mailboxes = useMailboxes()
  const list = useMessages(mailboxId)
  const message = useMessage(messageId)
  const bulk = useBulkEntries()
  const setFlags = useSetFlags()

  const mailbox = mailboxes.data?.items.find((candidate) => candidate.id === mailboxId)

  const targetBoxes = useMemo(() => {
    if (!mailbox) return []
    return (mailboxes.data?.items ?? []).filter(
      (m) => m.account_id === mailbox.account_id && m.id !== mailbox.id,
    )
  }, [mailboxes.data?.items, mailbox])

  const parsed = useMemo(() => parseFilterQuery(filter), [filter])

  const isUnreadActive = useMemo(() => {
    return unreadOnly || parsed.is === 'unread' || parsed.is === 'unseen'
  }, [unreadOnly, parsed.is])

  const isAttachmentActive = useMemo(() => {
    return attachmentsOnly || parsed.has === 'attachment' || parsed.has === 'attachments'
  }, [attachmentsOnly, parsed.has])

  const isFromActive = Boolean(parsed.from)
  const isToActive = Boolean(parsed.to)
  const isSubjectActive = Boolean(parsed.subject)

  const toggleUnreadPill = useCallback(() => {
    if (isUnreadActive) {
      setUnreadOnly(false)
      const next = filter
        .replace(/\bis:(?:unread|unseen)\b/gi, '')
        .replace(/\s+/g, ' ')
        .trim()
      setFilter(next)
    } else {
      setUnreadOnly(true)
      const next = filter.trim() ? `${filter.trim()} is:unread` : 'is:unread'
      setFilter(next)
    }
  }, [isUnreadActive, filter])

  const toggleAttachmentPill = useCallback(() => {
    if (isAttachmentActive) {
      setAttachmentsOnly(false)
      const next = filter
        .replace(/\bhas:(?:attachment|attachments)\b/gi, '')
        .replace(/\s+/g, ' ')
        .trim()
      setFilter(next)
    } else {
      setAttachmentsOnly(true)
      const next = filter.trim() ? `${filter.trim()} has:attachment` : 'has:attachment'
      setFilter(next)
    }
  }, [isAttachmentActive, filter])

  const insertOrFocusPrefix = useCallback(
    (prefix: 'from:' | 'to:' | 'subject:') => {
      let next = filter.trim()
      const lower = next.toLowerCase()
      const prefixIndex = lower.indexOf(prefix)

      if (prefixIndex === -1) {
        next = next ? `${next} ${prefix}` : prefix
        setFilter(next)
      }

      setTimeout(() => {
        if (searchInputRef.current) {
          searchInputRef.current.focus()
          const targetPos = prefixIndex === -1 ? next.length : prefixIndex + prefix.length
          searchInputRef.current.setSelectionRange(targetPos, targetPos)
        }
      }, 0)
    },
    [filter],
  )

  // Every row loaded so far, across pages. The list is virtualised, so holding
  // all of them costs memory but not DOM.
  const loaded = list.items

  // Stable identity: `MessageList` calls this from an effect keyed on the last
  // rendered row, so a new function each render would re-fire it.
  const fetchMore = useCallback(() => {
    if (list.hasNextPage && !list.isFetchingNextPage) {
      void list.fetchNextPage()
    }
  }, [list])

  const items = useMemo(() => {
    const raw = loaded
    if (!filter.trim() && !unreadOnly && !attachmentsOnly) return raw

    const effective = { ...parsed }
    if (unreadOnly && !effective.is) effective.is = 'unread'
    if (attachmentsOnly && !effective.has) effective.has = 'attachment'

    return raw.filter((msg) => matchesMessage(msg, effective, mailboxId))
  }, [loaded, filter, parsed, unreadOnly, attachmentsOnly, mailboxId])

  const currentEntryIds = useMemo(() => {
    return items
      .map((msg) => {
        const relevant =
          (mailboxId ? msg.entries.find((e) => e.mailbox_id === mailboxId) : null) ??
          msg.entries[0]
        return relevant?.entry_id
      })
      .filter((id): id is string => Boolean(id))
  }, [items, mailboxId])

  const selectedMessages = useMemo(() => {
    return items.filter((msg) => {
      const relevant =
        (mailboxId ? msg.entries.find((e) => e.mailbox_id === mailboxId) : null) ??
        msg.entries[0]
      return Boolean(relevant?.entry_id && selectedIds.has(relevant.entry_id))
    })
  }, [items, mailboxId, selectedIds])

  const handleDeselectEntry = useCallback((entryId: string) => {
    setSelectedIds((prev) => {
      const next = new Set(prev)
      next.delete(entryId)
      return next
    })
  }, [])

  const handleSelectAll = useCallback(() => {
    if (selectedIds.size === currentEntryIds.length) {
      setSelectedIds(new Set())
    } else {
      setSelectedIds(new Set(currentEntryIds))
    }
  }, [selectedIds.size, currentEntryIds])

  const handleBulkRead = () => {
    if (selectedIds.size === 0) return
    bulk.mutate(
      { entry_ids: Array.from(selectedIds), action: 'mark_read' },
      {
        onSuccess: () => {
          setSelectedIds(new Set())
          setIsSelectionMode(false)
        },
      },
    )
  }

  const handleBulkUnread = () => {
    if (selectedIds.size === 0) return
    bulk.mutate(
      { entry_ids: Array.from(selectedIds), action: 'mark_unread' },
      {
        onSuccess: () => {
          setSelectedIds(new Set())
          setIsSelectionMode(false)
        },
      },
    )
  }

  const handleBulkDelete = () => {
    if (selectedIds.size === 0) return
    bulk.mutate(
      { entry_ids: Array.from(selectedIds), action: 'delete' },
      {
        onSuccess: () => {
          setSelectedIds(new Set())
          setIsSelectionMode(false)
        },
      },
    )
  }

  const handleBulkMove = (targetMailboxId: string) => {
    if (selectedIds.size === 0) return
    setBulkMoveOpen(false)
    bulk.mutate(
      {
        entry_ids: Array.from(selectedIds),
        action: {
          move: {
            target_mailbox_id: targetMailboxId,
          },
        },
      },
      {
        onSuccess: () => {
          setSelectedIds(new Set())
          setIsSelectionMode(false)
        },
      },
    )
  }

  const activeIndex = useMemo(() => {
    if (!messageId) return -1
    return items.findIndex((m) => m.id === messageId)
  }, [items, messageId])

  const activeEntryId = useMemo(() => {
    if (activeIndex < 0) return undefined
    const activeMsg = items[activeIndex]
    const relevant =
      (mailboxId ? activeMsg?.entries.find((e) => e.mailbox_id === mailboxId) : null) ??
      activeMsg?.entries[0]
    return relevant?.entry_id
  }, [items, activeIndex, mailboxId])

  const handleToggleSelect = useCallback(
    (id: string) => {
      const targetMsg = items.find((m) => m.id === id)
      const relevant =
        (mailboxId ? targetMsg?.entries.find((e) => e.mailbox_id === mailboxId) : null) ??
        targetMsg?.entries[0]
      const entryId = relevant?.entry_id
      if (!entryId) return

      setSelectedIds((prev) => {
        const next = new Set(prev)
        if (next.has(entryId)) {
          next.delete(entryId)
        } else {
          next.add(entryId)
        }
        return next
      })
    },
    [items, mailboxId],
  )

  const handleLongPress = useCallback(
    (id: string) => {
      setIsSelectionMode(true)
      handleToggleSelect(id)
    },
    [handleToggleSelect],
  )

  const select = useCallback(
    (id: string, event?: React.MouseEvent) => {
      const clickedIndex = items.findIndex((m) => m.id === id)
      const clickedMsg = items[clickedIndex]
      const relevant =
        (mailboxId ? clickedMsg?.entries.find((e) => e.mailbox_id === mailboxId) : null) ??
        clickedMsg?.entries[0]
      const entryId = relevant?.entry_id

      // When in selection mode or when items are multi-selected:
      // Clicking/tapping a row toggles its highlight rather than opening reader
      if (
        (isSelectionMode || selectedIds.size > 0) &&
        !event?.ctrlKey &&
        !event?.metaKey &&
        !event?.shiftKey
      ) {
        handleToggleSelect(id)
        setLastClickedIndex(clickedIndex)
        return
      }

      if (event && (event.ctrlKey || event.metaKey)) {
        // Ctrl/Cmd + click: toggle in selectedIds
        setIsSelectionMode(true)
        setSelectedIds((prev) => {
          const next = new Set(prev)
          // If starting multi-selection while an active message is currently open in route:
          // seed the set with the active message as well!
          if (next.size === 0 && activeEntryId && activeEntryId !== entryId) {
            next.add(activeEntryId)
          }

          if (entryId) {
            if (next.has(entryId)) {
              next.delete(entryId)
            } else {
              next.add(entryId)
            }
          }
          return next
        })
        setLastClickedIndex(clickedIndex)
      } else if (event && event.shiftKey) {
        // Shift + click: range select
        setIsSelectionMode(true)
        const baseIndex =
          lastClickedIndex >= 0 ? lastClickedIndex : activeIndex >= 0 ? activeIndex : clickedIndex
        const start = Math.min(baseIndex, clickedIndex)
        const end = Math.max(baseIndex, clickedIndex)
        const next = new Set(selectedIds)
        for (let i = start; i <= end; i++) {
          const m = items[i]
          const ent =
            (mailboxId ? m?.entries.find((e) => e.mailbox_id === mailboxId) : null) ??
            m?.entries[0]
          if (ent?.entry_id) next.add(ent.entry_id)
        }
        setSelectedIds(next)
      } else {
        // Normal click: single select and view
        setIsSelectionMode(false)
        setSelectedIds(new Set())
        setLastClickedIndex(clickedIndex)
        if (relevant && !relevant.flags.includes(SEEN)) {
          setFlags.mutate({ entryId: relevant.entry_id, add: [SEEN] })
        }
        navigate(`/mailbox/${mailboxId}/message/${id}`)
      }
    },
    [
      items,
      mailboxId,
      lastClickedIndex,
      selectedIds,
      isSelectionMode,
      activeIndex,
      activeEntryId,
      navigate,
      setFlags,
      handleToggleSelect,
    ],
  )

  const close = useCallback(() => {
    navigate(`/mailbox/${mailboxId}`)
  }, [navigate, mailboxId])

  // Reset filter and selection when switching mailboxes
  useEffect(() => {
    setFilter('')
    setUnreadOnly(false)
    setAttachmentsOnly(false)
    setSelectedIds(new Set())
    setIsSelectionMode(false)
    setLastClickedIndex(-1)
    setFilterOptionsOpen(false)
    setShowSyntaxHelper(false)
  }, [mailboxId])

  // Message list navigation, selection, and action keyboard shortcuts (§11)
  useKeyboardShortcuts([
    {
      key: 'j',
      description: 'Select next message',
      category: 'Message List',
      action: () => {
        if (items.length === 0) return
        const nextIdx =
          activeIndex + 1 < items.length ? (activeIndex < 0 ? 0 : activeIndex + 1) : activeIndex
        const nextMsg = items[nextIdx]
        if (nextMsg) select(nextMsg.id)
      },
    },
    {
      key: 'ArrowDown',
      description: 'Select next message',
      category: 'Message List',
      action: () => {
        if (items.length === 0) return
        const nextIdx =
          activeIndex + 1 < items.length ? (activeIndex < 0 ? 0 : activeIndex + 1) : activeIndex
        const nextMsg = items[nextIdx]
        if (nextMsg) select(nextMsg.id)
      },
    },
    {
      key: 'k',
      description: 'Select previous message',
      category: 'Message List',
      action: () => {
        if (items.length === 0) return
        const prevIdx =
          activeIndex - 1 >= 0 ? activeIndex - 1 : activeIndex < 0 ? 0 : activeIndex
        const prevMsg = items[prevIdx]
        if (prevMsg) select(prevMsg.id)
      },
    },
    {
      key: 'ArrowUp',
      description: 'Select previous message',
      category: 'Message List',
      action: () => {
        if (items.length === 0) return
        const prevIdx =
          activeIndex - 1 >= 0 ? activeIndex - 1 : activeIndex < 0 ? 0 : activeIndex
        const prevMsg = items[prevIdx]
        if (prevMsg) select(prevMsg.id)
      },
    },
    {
      key: 'x',
      description: 'Toggle message selection',
      category: 'Message List',
      action: () => {
        if (activeEntryId) {
          setIsSelectionMode(true)
          setSelectedIds((prev) => {
            const next = new Set(prev)
            if (next.has(activeEntryId)) {
              next.delete(activeEntryId)
            } else {
              next.add(activeEntryId)
            }
            return next
          })
        }
      },
    },
    {
      key: '*',
      shift: true,
      description: 'Select all loaded messages',
      category: 'Message List',
      action: () => {
        setIsSelectionMode(true)
        setSelectedIds(new Set(currentEntryIds))
      },
    },
    {
      key: 'o',
      description: 'Open active message',
      category: 'Message List',
      action: () => {
        if (activeIndex >= 0 && items[activeIndex]) {
          const m = items[activeIndex]
          if (m) navigate(`/mailbox/${mailboxId}/message/${m.id}`)
        }
      },
    },
    {
      key: 'Enter',
      description: 'Open active message',
      category: 'Message List',
      action: () => {
        if (activeIndex >= 0 && items[activeIndex]) {
          const m = items[activeIndex]
          if (m) navigate(`/mailbox/${mailboxId}/message/${m.id}`)
        }
      },
    },
    {
      key: '/',
      description: 'Search messages',
      category: 'Global & Navigation',
      action: () => {
        if (searchInputRef.current) {
          searchInputRef.current.focus()
          searchInputRef.current.select()
        }
      },
    },
    {
      key: 'm',
      description: 'Move selected message(s)',
      category: 'Actions',
      action: () => {
        if (targetBoxes.length > 0) {
          setBulkMoveOpen(true)
        }
      },
    },
    {
      key: 'v',
      description: 'Move selected message(s)',
      category: 'Actions',
      action: () => {
        if (targetBoxes.length > 0) {
          setBulkMoveOpen(true)
        }
      },
    },
    {
      key: 's',
      description: 'Star / Flag message',
      category: 'Actions',
      action: () => {
        if (activeEntryId) {
          const activeMsg = items[activeIndex]
          const relevant =
            (mailboxId ? activeMsg?.entries.find((e) => e.mailbox_id === mailboxId) : null) ??
            activeMsg?.entries[0]
          if (relevant) {
            const hasFlag = relevant.flags.includes('\\Flagged')
            setFlags.mutate({
              entryId: activeEntryId,
              [hasFlag ? 'remove' : 'add']: ['\\Flagged'],
            })
          }
        }
      },
    },
    {
      key: 'I',
      shift: true,
      description: 'Mark as read',
      category: 'Actions',
      action: () => {
        if (selectedIds.size > 0) handleBulkRead()
        else if (activeEntryId) setFlags.mutate({ entryId: activeEntryId, add: [SEEN] })
      },
    },
    {
      key: 'U',
      shift: true,
      description: 'Mark as unread',
      category: 'Actions',
      action: () => {
        if (selectedIds.size > 0) handleBulkUnread()
        else if (activeEntryId) setFlags.mutate({ entryId: activeEntryId, remove: [SEEN] })
      },
    },
    {
      key: 'e',
      description: 'Archive selected message(s)',
      category: 'Actions',
      action: () => {
        const archiveBox = mailboxes.data?.items.find(
          (m) => m.account_id === mailbox?.account_id && m.name.toLowerCase() === 'archive',
        )
        if (archiveBox) {
          if (selectedIds.size > 0) {
            handleBulkMove(archiveBox.id)
          } else if (activeEntryId) {
            bulk.mutate(
              {
                entry_ids: [activeEntryId],
                action: { move: { target_mailbox_id: archiveBox.id } },
              },
              { onSuccess: () => close() },
            )
          }
        }
      },
    },
    {
      key: 'y',
      description: 'Archive selected message(s)',
      category: 'Actions',
      action: () => {
        const archiveBox = mailboxes.data?.items.find(
          (m) => m.account_id === mailbox?.account_id && m.name.toLowerCase() === 'archive',
        )
        if (archiveBox) {
          if (selectedIds.size > 0) {
            handleBulkMove(archiveBox.id)
          } else if (activeEntryId) {
            bulk.mutate(
              {
                entry_ids: [activeEntryId],
                action: { move: { target_mailbox_id: archiveBox.id } },
              },
              { onSuccess: () => close() },
            )
          }
        }
      },
    },
    {
      key: '#',
      description: 'Move to Trash / Delete',
      category: 'Actions',
      action: () => {
        if (selectedIds.size > 0) {
          handleBulkDelete()
        } else if (activeEntryId) {
          bulk.mutate(
            { entry_ids: [activeEntryId], action: 'delete' },
            { onSuccess: () => close() },
          )
        }
      },
    },
    {
      key: 'Delete',
      description: 'Move to Trash / Delete',
      category: 'Actions',
      action: () => {
        if (selectedIds.size > 0) {
          handleBulkDelete()
        } else if (activeEntryId) {
          bulk.mutate(
            { entry_ids: [activeEntryId], action: 'delete' },
            { onSuccess: () => close() },
          )
        }
      },
    },
    {
      key: 'Escape',
      description: 'Close message or clear selection',
      category: 'Global & Navigation',
      action: () => {
        if (filterOptionsOpen) {
          setFilterOptionsOpen(false)
        } else if (isSelectionMode || selectedIds.size > 0) {
          setIsSelectionMode(false)
          setSelectedIds(new Set())
        } else if (messageId) {
          close()
        }
      },
    },
  ])

  // On mobile (< 768px), master-detail only switches to the viewer pane when an
  // individual message is active AND we are not in multi-selection mode.
  const hasMessageOpen = Boolean(messageId && selectedIds.size === 0 && !isSelectionMode)

  return (
    <div className={`mail ${hasMessageOpen ? 'mail--has-selection' : ''}`}>
      <section className="mail__list" aria-label={mailbox ? `${mailbox.name} messages` : 'Messages'}>
        <header className="mail__list-header">
          <div className="mail__list-header-top">
            <h1 className="mail__title">{mailbox?.name ?? 'Messages'}</h1>
            <div className="mail__header-meta">
              {list.data ? (
                <span className="mail__count">
                  {(filter.trim() || unreadOnly || attachmentsOnly) && items.length !== loaded.length
                    ? `${items.length} of ${loaded.length}`
                    : loaded.length}
                  {/* A trailing "+" means more pages exist, not a count we know:
                      the cursor says whether there is more, never how much. */}
                  {list.hasNextPage ? '+' : ''}
                </span>
              ) : null}
              {loaded.length > 0 && (
                <button
                  type="button"
                  className={`mail__select-mode-toggle ${
                    isSelectionMode || selectedIds.size > 0 ? 'mail__select-mode-toggle--active' : ''
                  }`}
                  onClick={() => {
                    if (isSelectionMode || selectedIds.size > 0) {
                      setIsSelectionMode(false)
                      setSelectedIds(new Set())
                    } else {
                      setIsSelectionMode(true)
                    }
                  }}
                  aria-pressed={isSelectionMode || selectedIds.size > 0}
                >
                  {isSelectionMode || selectedIds.size > 0 ? 'Done' : 'Select'}
                </button>
              )}
            </div>
          </div>
          {selectedIds.size > 0 || isSelectionMode ? (
            <div className="mail__bulk-toolbar" role="toolbar" aria-label="Bulk actions">
              <div className="mail__bulk-count">
                <span className="mail__bulk-badge">
                  {selectedIds.size > 0 ? `${selectedIds.size} selected` : 'Select items'}
                </span>
                <button
                  type="button"
                  className="mail__bulk-link"
                  onClick={handleSelectAll}
                >
                  {selectedIds.size === currentEntryIds.length && currentEntryIds.length > 0
                    ? 'Deselect all'
                    : 'Select all'}
                </button>
              </div>
              <div className="mail__bulk-actions">
                <button
                  type="button"
                  className="button button--ghost button--sm"
                  onClick={handleBulkRead}
                  disabled={bulk.isPending || selectedIds.size === 0}
                  title="Mark selected as read"
                  aria-label="Mark selected as read"
                >
                  <MailCheck size={14} aria-hidden="true" />
                </button>
                <button
                  type="button"
                  className="button button--ghost button--sm"
                  onClick={handleBulkUnread}
                  disabled={bulk.isPending || selectedIds.size === 0}
                  title="Mark selected as unread"
                  aria-label="Mark selected as unread"
                >
                  <MailX size={14} aria-hidden="true" />
                </button>
                {targetBoxes.length > 0 && (
                  <div className="mail__bulk-move-wrap">
                    <button
                      type="button"
                      className="button button--ghost button--sm"
                      onClick={() => setBulkMoveOpen((v) => !v)}
                      disabled={bulk.isPending || selectedIds.size === 0}
                      title="Move selected to folder"
                      aria-label="Move selected to folder"
                      aria-expanded={bulkMoveOpen}
                    >
                      <FolderOpen size={14} aria-hidden="true" />
                    </button>
                    {bulkMoveOpen && (
                      <>
                        <div
                          className="mail__bulk-move-backdrop"
                          onClick={() => setBulkMoveOpen(false)}
                          aria-hidden="true"
                        />
                        <div
                          className="mail__bulk-move-popover"
                          role="menu"
                          aria-label="Choose destination folder"
                        >
                          <div className="mail__bulk-move-title">Move to folder</div>
                          {targetBoxes.map((target) => {
                            return (
                              <button
                                key={target.id}
                                type="button"
                                className="mail__bulk-move-item"
                                onClick={() => handleBulkMove(target.id)}
                                role="menuitem"
                              >
                                {getMailboxIcon(target.name)}
                                <span>{target.name}</span>
                              </button>
                            )
                          })}
                        </div>
                      </>
                    )}
                  </div>
                )}
                <button
                  type="button"
                  className="button button--danger button--sm"
                  onClick={handleBulkDelete}
                  disabled={bulk.isPending || selectedIds.size === 0}
                  title="Delete selected"
                  aria-label="Delete selected"
                >
                  <Trash2 size={14} aria-hidden="true" />
                </button>
                <button
                  type="button"
                  className="button button--ghost button--sm"
                  onClick={() => {
                    setSelectedIds(new Set())
                    setIsSelectionMode(false)
                    setBulkMoveOpen(false)
                  }}
                  title="Done / Clear selection (Esc)"
                  aria-label="Clear selection"
                >
                  <X size={14} aria-hidden="true" />
                </button>
              </div>
            </div>
          ) : loaded.length > 0 ? (
            <div className="mail__filter-bar">
              <div className="mail__filter-wrapper">
                <div className="mail__filter">
                  <Search size={13} className="mail__filter-icon" aria-hidden="true" />
                  <input
                    ref={searchInputRef}
                    type="search"
                    className="mail__filter-input"
                    placeholder="Filter sender or subject (e.g. from:, to:, is:unread)..."
                    value={filter}
                    onChange={(event) => setFilter(event.target.value)}
                    onFocus={() => setShowSyntaxHelper(true)}
                    onBlur={() => setTimeout(() => setShowSyntaxHelper(false), 200)}
                    aria-label="Filter messages"
                  />
                  {filter && (
                    <button
                      type="button"
                      className="mail__filter-clear"
                      onClick={() => {
                        setFilter('')
                        setUnreadOnly(false)
                        setAttachmentsOnly(false)
                      }}
                      aria-label="Clear filter"
                      title="Clear filter"
                    >
                      <X size={12} aria-hidden="true" />
                    </button>
                  )}
                  <button
                    type="button"
                    className={`mail__filter-options-btn ${filterOptionsOpen ? 'mail__filter-options-btn--active' : ''}`}
                    onClick={() => {
                      if (!filterOptionsOpen) {
                        const current = parseFilterQuery(filter)
                        setBuilderFrom(current.from ?? '')
                        setBuilderTo(current.to ?? '')
                        setBuilderSubject(current.subject ?? '')
                        setBuilderUnread(Boolean(current.is === 'unread' || current.is === 'unseen' || unreadOnly))
                        setBuilderAttachments(Boolean(current.has === 'attachment' || current.has === 'attachments' || attachmentsOnly))
                      }
                      setFilterOptionsOpen((v) => !v)
                      setShowSyntaxHelper(false)
                    }}
                    title="Filter options & builder"
                    aria-label="Filter options"
                    aria-expanded={filterOptionsOpen}
                  >
                    <SlidersHorizontal size={13} aria-hidden="true" />
                  </button>
                </div>

                {filterOptionsOpen && (
                  <>
                    <div
                      className="mail__filter-popover-backdrop"
                      onClick={() => setFilterOptionsOpen(false)}
                      aria-hidden="true"
                    />
                    <div className="mail__filter-popover" role="dialog" aria-label="Filter options">
                      <div className="mail__filter-popover-header">
                        <span className="mail__filter-popover-title">Filter Options</span>
                        <button
                          type="button"
                          className="mail__filter-popover-close"
                          onClick={() => setFilterOptionsOpen(false)}
                          aria-label="Close filter options"
                        >
                          <X size={13} aria-hidden="true" />
                        </button>
                      </div>

                      <div className="mail__filter-popover-body">
                        <label className="mail__filter-field">
                          <span className="mail__filter-label">From:</span>
                          <input
                            type="text"
                            className="mail__filter-field-input"
                            placeholder="e.g. alice@example.com or Alice"
                            value={builderFrom}
                            onChange={(e) => setBuilderFrom(e.target.value)}
                          />
                        </label>
                        <label className="mail__filter-field">
                          <span className="mail__filter-label">To:</span>
                          <input
                            type="text"
                            className="mail__filter-field-input"
                            placeholder="e.g. bob@example.com"
                            value={builderTo}
                            onChange={(e) => setBuilderTo(e.target.value)}
                          />
                        </label>
                        <label className="mail__filter-field">
                          <span className="mail__filter-label">Subject:</span>
                          <input
                            type="text"
                            className="mail__filter-field-input"
                            placeholder="e.g. invoice, urgent"
                            value={builderSubject}
                            onChange={(e) => setBuilderSubject(e.target.value)}
                          />
                        </label>

                        <div className="mail__filter-checkboxes">
                          <label className="mail__filter-checkbox-label">
                            <input
                              type="checkbox"
                              checked={builderUnread}
                              onChange={(e) => setBuilderUnread(e.target.checked)}
                            />
                            <span>Unread only (<code>is:unread</code>)</span>
                          </label>
                          <label className="mail__filter-checkbox-label">
                            <input
                              type="checkbox"
                              checked={builderAttachments}
                              onChange={(e) => setBuilderAttachments(e.target.checked)}
                            />
                            <span>Has attachments (<code>has:attachment</code>)</span>
                          </label>
                        </div>

                        <div className="mail__filter-hint">
                          Tip: You can also type <code>from:</code>, <code>to:</code>, <code>subject:</code> directly in the filter box.
                        </div>
                      </div>

                      <div className="mail__filter-popover-footer">
                        <button
                          type="button"
                          className="button button--ghost button--sm"
                          onClick={() => {
                            setBuilderFrom('')
                            setBuilderTo('')
                            setBuilderSubject('')
                            setBuilderUnread(false)
                            setBuilderAttachments(false)
                            setFilter('')
                            setUnreadOnly(false)
                            setAttachmentsOnly(false)
                            setFilterOptionsOpen(false)
                          }}
                        >
                          Reset
                        </button>
                        <button
                          type="button"
                          className="button button--primary button--sm"
                          onClick={() => {
                            const compiled = buildFilterQuery({
                              from: builderFrom,
                              to: builderTo,
                              subject: builderSubject,
                              unreadOnly: builderUnread,
                              attachmentsOnly: builderAttachments,
                            })
                            setFilter(compiled)
                            setUnreadOnly(builderUnread)
                            setAttachmentsOnly(builderAttachments)
                            setFilterOptionsOpen(false)
                          }}
                        >
                          Apply Filter
                        </button>
                      </div>

                      {filter.trim() && (
                        <div className="mail__filter-popover-search-all">
                          <button
                            type="button"
                            className="mail__filter-search-all-btn"
                            onClick={() => {
                              setFilterOptionsOpen(false)
                              navigate(`/search?q=${encodeURIComponent(filter.trim())}`)
                            }}
                          >
                            Search all mailboxes for &ldquo;{filter.trim()}&rdquo; &rarr;
                          </button>
                        </div>
                      )}
                    </div>
                  </>
                )}

                {showSyntaxHelper && !filterOptionsOpen && !filter.trim() && (
                  <div className="mail__syntax-helper" role="tooltip">
                    <div className="mail__syntax-helper-header">
                      <span className="mail__syntax-helper-title">Supported Filters</span>
                      <span className="mail__syntax-helper-subtitle">Click to insert</span>
                    </div>
                    <div className="mail__syntax-helper-list">
                      <button
                        type="button"
                        className="mail__syntax-item"
                        onMouseDown={(e) => {
                          e.preventDefault()
                          insertOrFocusPrefix('from:')
                          setShowSyntaxHelper(false)
                        }}
                      >
                        <span className="mail__syntax-badge">from:</span>
                        <span className="mail__syntax-label">Sender email or name</span>
                        <span className="mail__syntax-example">from:alice</span>
                      </button>
                      <button
                        type="button"
                        className="mail__syntax-item"
                        onMouseDown={(e) => {
                          e.preventDefault()
                          insertOrFocusPrefix('to:')
                          setShowSyntaxHelper(false)
                        }}
                      >
                        <span className="mail__syntax-badge">to:</span>
                        <span className="mail__syntax-label">Recipient address</span>
                        <span className="mail__syntax-example">to:bob</span>
                      </button>
                      <button
                        type="button"
                        className="mail__syntax-item"
                        onMouseDown={(e) => {
                          e.preventDefault()
                          insertOrFocusPrefix('subject:')
                          setShowSyntaxHelper(false)
                        }}
                      >
                        <span className="mail__syntax-badge">subject:</span>
                        <span className="mail__syntax-label">Subject keyword</span>
                        <span className="mail__syntax-example">subject:report</span>
                      </button>
                      <button
                        type="button"
                        className="mail__syntax-item"
                        onMouseDown={(e) => {
                          e.preventDefault()
                          toggleUnreadPill()
                          setShowSyntaxHelper(false)
                        }}
                      >
                        <span className="mail__syntax-badge">is:unread</span>
                        <span className="mail__syntax-label">Unread messages only</span>
                      </button>
                      <button
                        type="button"
                        className="mail__syntax-item"
                        onMouseDown={(e) => {
                          e.preventDefault()
                          toggleAttachmentPill()
                          setShowSyntaxHelper(false)
                        }}
                      >
                        <span className="mail__syntax-badge">has:attachment</span>
                        <span className="mail__syntax-label">Has file attachments</span>
                      </button>
                    </div>
                  </div>
                )}
              </div>

              <div className="search-pills" role="toolbar" aria-label="Quick filters">
                <button
                  type="button"
                  className={`search-pill ${isUnreadActive ? 'search-pill--active' : ''}`}
                  onClick={toggleUnreadPill}
                  aria-pressed={isUnreadActive}
                >
                  is:unread
                </button>
                <button
                  type="button"
                  className={`search-pill ${isAttachmentActive ? 'search-pill--active' : ''}`}
                  onClick={toggleAttachmentPill}
                  aria-pressed={isAttachmentActive}
                >
                  has:attachment
                </button>
                <button
                  type="button"
                  className={`search-pill search-pill--prefix ${isFromActive ? 'search-pill--active' : ''}`}
                  onClick={() => insertOrFocusPrefix('from:')}
                  title="Filter by sender (e.g. from:alice)"
                >
                  from:
                </button>
                <button
                  type="button"
                  className={`search-pill search-pill--prefix ${isToActive ? 'search-pill--active' : ''}`}
                  onClick={() => insertOrFocusPrefix('to:')}
                  title="Filter by recipient (e.g. to:bob)"
                >
                  to:
                </button>
                <button
                  type="button"
                  className={`search-pill search-pill--prefix ${isSubjectActive ? 'search-pill--active' : ''}`}
                  onClick={() => insertOrFocusPrefix('subject:')}
                  title="Filter by subject (e.g. subject:report)"
                >
                  subject:
                </button>
              </div>
            </div>
          ) : null}
        </header>

        {list.isPending ? (
          <LoadingState label="Loading messages" />
        ) : list.isError ? (
          <ErrorState error={list.error} onRetry={() => void list.refetch()} />
        ) : list.data && loaded.length === 0 ? (
          // The empty state IS the onboarding: it answers "how do I get mail
          // in here?" with the real, copyable endpoint (§49).
          <EmptyState title="No messages yet">
            <p>Send mail to this Sandesh instance and it will appear here instantly.</p>
            <ConnectionDetails compact />
          </EmptyState>
        ) : items.length === 0 && filter.trim() ? (
          <div className="mail__filter-empty">
            <p>No messages match &ldquo;{filter}&rdquo; in this folder</p>
            <div className="mail__filter-empty-actions">
              <button
                type="button"
                className="button button--ghost button--sm"
                onClick={() => {
                  setFilter('')
                  setUnreadOnly(false)
                  setAttachmentsOnly(false)
                }}
              >
                Clear filter
              </button>
              <button
                type="button"
                className="button button--primary button--sm"
                onClick={() => navigate(`/search?q=${encodeURIComponent(filter.trim())}`)}
              >
                Search all mailboxes &rarr;
              </button>
            </div>
          </div>
        ) : (
          <MessageList
            messages={items}
            selectedId={selectedIds.size > 0 ? undefined : messageId}
            onSelect={select}
            mailboxId={mailboxId}
            selectedEntryIds={selectedIds}
            onLongPress={handleLongPress}
            // Paging is driven by the *loaded* rows, not the filtered ones.
            // The filter is client-side, so a filter matching nothing on the
            // current page would otherwise stop fetching and report "no
            // matches" for a mailbox that has them further down. Reaching the
            // end of a filtered view therefore still pulls the next page.
            hasMore={list.hasNextPage && !list.isFetchingNextPage}
            onEndReached={fetchMore}
          />
        )}
      </section>

      <section className="mail__detail" aria-label="Message detail">
        {selectedIds.size > 0 ? (
          <MultiMessageViewer
            messages={selectedMessages}
            selectedIds={selectedIds}
            onSelectMessage={(id) => {
              const targetMsg = items.find((m) => m.id === id)
              const relevant =
                (mailboxId ? targetMsg?.entries.find((e) => e.mailbox_id === mailboxId) : null) ??
                targetMsg?.entries[0]
              if (relevant && !relevant.flags.includes(SEEN)) {
                setFlags.mutate({ entryId: relevant.entry_id, add: [SEEN] })
              }
              setSelectedIds(new Set())
              navigate(`/mailbox/${mailboxId}/message/${id}`)
            }}
            onDeselectEntry={handleDeselectEntry}
            onClearSelection={() => setSelectedIds(new Set())}
            onBulkRead={handleBulkRead}
            onBulkUnread={handleBulkUnread}
            onBulkDelete={handleBulkDelete}
            isPending={bulk.isPending}
            mailboxId={mailboxId}
          />
        ) : !messageId ? (
          <EmptyState title="No message selected">
            <p>Choose a message to inspect its content, headers, MIME structure and source.</p>
          </EmptyState>
        ) : message.isPending ? (
          <LoadingState label="Loading message" />
        ) : message.isError ? (
          <ErrorState error={message.error} onRetry={() => void message.refetch()} />
        ) : message.data ? (
          <MessageViewer message={message.data} mailboxId={mailboxId} onClose={close} />
        ) : null}
      </section>
    </div>
  )
}
