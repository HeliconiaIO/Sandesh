/**
 * The search view: filter-based message search with explain plan and bulk actions.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useNavigate, useParams, useSearchParams } from 'react-router-dom'
import { MailCheck, MailX, Search, Trash2, X } from 'lucide-react'

import { EmptyState, ErrorState, LoadingState } from '../components/States'
import { MessageList } from '../features/messages/MessageList'
import { MessageViewer } from '../features/messages/MessageViewer'
import { MultiMessageViewer } from '../features/messages/MultiMessageViewer'
import {
  useBulkEntries,
  useMessage,
  useSearchExplain,
  useSearchMessages,
  useSetFlags,
} from '../hooks/queries'
import './SearchRoute.css'
import './MailboxRoute.css'

const SEEN = '\\Seen'

export function SearchRoute() {
  const { messageId } = useParams<{ messageId: string }>()
  const [searchParams, setSearchParams] = useSearchParams()
  const q = searchParams.get('q') ?? ''
  const navigate = useNavigate()

  const [inputVal, setInputVal] = useState(q)
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set())
  const [isSelectionMode, setIsSelectionMode] = useState(false)
  const [lastClickedIndex, setLastClickedIndex] = useState<number>(-1)
  const searchInputRef = useRef<HTMLInputElement>(null)

  // Keep inputVal in sync with query param
  useEffect(() => {
    setInputVal(q)
    setSelectedIds(new Set())
    setIsSelectionMode(false)
    setLastClickedIndex(-1)
  }, [q])

  const isTokenActive = useCallback(
    (token: string) => {
      const tokens = q.trim() ? q.trim().split(/\s+/) : []
      return tokens.includes(token)
    },
    [q],
  )

  const toggleFilterPill = useCallback(
    (token: string) => {
      const tokens = inputVal.trim() ? inputVal.trim().split(/\s+/) : []
      const index = tokens.indexOf(token)
      let newTokens: string[]
      if (index >= 0) {
        newTokens = tokens.filter((t) => t !== token)
      } else {
        newTokens = [...tokens, token]
      }
      const newQuery = newTokens.join(' ')
      setInputVal(newQuery)
      if (newQuery) {
        setSearchParams({ q: newQuery })
      } else {
        setSearchParams({})
      }
    },
    [inputVal, setSearchParams],
  )

  const appendPrefixPill = useCallback(
    (prefix: string) => {
      let updated = inputVal.trim()
      if (!updated.includes(prefix)) {
        updated = updated ? `${updated} ${prefix}` : prefix
        setInputVal(updated)
      }
      setTimeout(() => {
        if (searchInputRef.current) {
          searchInputRef.current.focus()
          searchInputRef.current.setSelectionRange(updated.length, updated.length)
        }
      }, 0)
    },
    [inputVal],
  )

  const searchResults = useSearchMessages(q)
  const explain = useSearchExplain(q)
  const message = useMessage(messageId)
  const bulk = useBulkEntries()
  const setFlags = useSetFlags()

  const items = searchResults.data?.items ?? []

  const currentEntryIds = useMemo(() => {
    return items
      .map((msg) => msg.entries[0]?.entry_id)
      .filter((id): id is string => Boolean(id))
  }, [items])

  const selectedMessages = useMemo(() => {
    return items.filter((msg) => {
      const entryId = msg.entries[0]?.entry_id
      return Boolean(entryId && selectedIds.has(entryId))
    })
  }, [items, selectedIds])

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

  const handleSearchSubmit = (event: React.FormEvent) => {
    event.preventDefault()
    if (inputVal.trim()) {
      setSearchParams({ q: inputVal.trim() })
    } else {
      setSearchParams({})
    }
  }

  const applyFilterChip = (filterToken: string) => {
    const updated = inputVal ? `${inputVal} ${filterToken}` : filterToken
    setInputVal(updated)
    setSearchParams({ q: updated })
  }

  const activeIndex = useMemo(() => {
    if (!messageId) return -1
    return items.findIndex((m) => m.id === messageId)
  }, [items, messageId])

  const activeEntryId = useMemo(() => {
    if (activeIndex < 0) return undefined
    return items[activeIndex]?.entries[0]?.entry_id
  }, [items, activeIndex])

  const handleToggleSelect = useCallback(
    (id: string) => {
      const targetMsg = items.find((m) => m.id === id)
      const entryId = targetMsg?.entries[0]?.entry_id
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
    [items],
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
      const entryId = clickedMsg?.entries[0]?.entry_id

      // When in selection mode or when multiple items are selected:
      // toggle that item's selection instead of navigating away.
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
        setIsSelectionMode(true)
        setSelectedIds((prev) => {
          const next = new Set(prev)
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
        setIsSelectionMode(true)
        const baseIndex =
          lastClickedIndex >= 0 ? lastClickedIndex : activeIndex >= 0 ? activeIndex : clickedIndex
        const start = Math.min(baseIndex, clickedIndex)
        const end = Math.max(baseIndex, clickedIndex)
        const next = new Set(selectedIds)
        for (let i = start; i <= end; i++) {
          const entId = items[i]?.entries[0]?.entry_id
          if (entId) next.add(entId)
        }
        setSelectedIds(next)
      } else {
        setIsSelectionMode(false)
        setSelectedIds(new Set())
        setLastClickedIndex(clickedIndex)
        if (clickedMsg?.entries[0] && !clickedMsg.entries[0].flags.includes(SEEN)) {
          setFlags.mutate({ entryId: clickedMsg.entries[0].entry_id, add: [SEEN] })
        }
        navigate(`/search/message/${id}${q ? `?q=${encodeURIComponent(q)}` : ''}`)
      }
    },
    [
      items,
      lastClickedIndex,
      selectedIds,
      isSelectionMode,
      activeIndex,
      activeEntryId,
      navigate,
      q,
      setFlags,
      handleToggleSelect,
    ],
  )

  const close = useCallback(() => {
    navigate(`/search${q ? `?q=${encodeURIComponent(q)}` : ''}`)
  }, [navigate, q])

  // Escape closes the message or clears multi-selection
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.defaultPrevented) return
      if (document.querySelector('.composer-dialog, [role="dialog"]')) return
      if (event.key === 'Escape') {
        if (isSelectionMode || selectedIds.size > 0) {
          setIsSelectionMode(false)
          setSelectedIds(new Set())
        } else if (messageId) {
          close()
        }
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [messageId, close, selectedIds.size, isSelectionMode])

  // On mobile, only collapse search results list when viewing an individual message.
  const hasMessageOpen = Boolean(messageId && selectedIds.size === 0 && !isSelectionMode)

  return (
    <div className={`mail search-page ${hasMessageOpen ? 'mail--has-selection' : ''}`}>
      <section className="mail__list search-page__list" aria-label="Search results">
        <header className="mail__list-header search-page__header">
          <div className="mail__list-header-top">
            <h1 className="mail__title">Search</h1>
            <div className="mail__header-meta">
              {searchResults.data ? (
                <span className="mail__count">
                  {items.length} {items.length === 1 ? 'result' : 'results'}
                </span>
              ) : null}
              {items.length > 0 && (
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

          <form onSubmit={handleSearchSubmit} className="mail__filter">
            <Search size={13} className="mail__filter-icon" aria-hidden="true" />
            <input
              ref={searchInputRef}
              type="search"
              className="mail__filter-input"
              placeholder="Search with from:, to:, subject:, is:unread, has:attachment..."
              value={inputVal}
              onChange={(e) => setInputVal(e.target.value)}
              aria-label="Search messages"
            />
            {inputVal && (
              <button
                type="button"
                className="mail__filter-clear"
                onClick={() => {
                  setInputVal('')
                  setSearchParams({})
                }}
                aria-label="Clear search input"
              >
                <X size={12} aria-hidden="true" />
              </button>
            )}
          </form>

          <div className="search-pills" role="toolbar" aria-label="Search quick filters">
            <button
              type="button"
              className={`search-pill ${isTokenActive('is:unread') ? 'search-pill--active' : ''}`}
              onClick={() => toggleFilterPill('is:unread')}
              aria-pressed={isTokenActive('is:unread')}
            >
              is:unread
            </button>
            <button
              type="button"
              className={`search-pill ${isTokenActive('has:attachment') ? 'search-pill--active' : ''}`}
              onClick={() => toggleFilterPill('has:attachment')}
              aria-pressed={isTokenActive('has:attachment')}
            >
              has:attachment
            </button>
            <button
              type="button"
              className="search-pill search-pill--prefix"
              onClick={() => appendPrefixPill('from:')}
              title="Add from: filter"
            >
              from:
            </button>
            <button
              type="button"
              className="search-pill search-pill--prefix"
              onClick={() => appendPrefixPill('to:')}
              title="Add to: filter"
            >
              to:
            </button>
            <button
              type="button"
              className="search-pill search-pill--prefix"
              onClick={() => appendPrefixPill('subject:')}
              title="Add subject: filter"
            >
              subject:
            </button>
          </div>

          {explain.data && explain.data.plan && (
            <div className="search-page__plan" title={`Compiled plan: ${explain.data.plan}`}>
              Plan: {explain.data.plan}
            </div>
          )}

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
                  }}
                  title="Done / Clear selection (Esc)"
                  aria-label="Clear selection"
                >
                  <X size={14} aria-hidden="true" />
                </button>
              </div>
            </div>
          ) : null}
        </header>

        {!q.trim() ? (
          <div className="search-guide">
            <h2 className="search-guide__title">Filter Syntax</h2>
            <p className="search-guide__desc">
              Compose powerful queries using developer filter prefixes or free text:
            </p>
            <div className="search-guide__chips">
              <button
                type="button"
                className="search-chip"
                onClick={() => applyFilterChip('is:unread')}
              >
                is:unread
              </button>
              <button
                type="button"
                className="search-chip"
                onClick={() => applyFilterChip('has:attachment')}
              >
                has:attachment
              </button>
              <button
                type="button"
                className="search-chip"
                onClick={() => applyFilterChip('from:test')}
              >
                from:test
              </button>
              <button
                type="button"
                className="search-chip"
                onClick={() => applyFilterChip('to:dev')}
              >
                to:dev
              </button>
              <button
                type="button"
                className="search-chip"
                onClick={() => applyFilterChip('subject:welcome')}
              >
                subject:welcome
              </button>
              <button
                type="button"
                className="search-chip"
                onClick={() => applyFilterChip('larger:100kb')}
              >
                larger:100kb
              </button>
            </div>
          </div>
        ) : searchResults.isPending ? (
          <LoadingState label="Searching messages" />
        ) : searchResults.isError ? (
          <ErrorState
            error={searchResults.error}
            onRetry={() => void searchResults.refetch()}
          />
        ) : items.length === 0 ? (
          <div className="mail__filter-empty">
            <p>No messages match "{q}"</p>
            <button
              type="button"
              className="button button--ghost"
              onClick={() => {
                setInputVal('')
                setSearchParams({})
              }}
            >
              Clear search
            </button>
          </div>
        ) : (
          <MessageList
            messages={items}
            selectedId={selectedIds.size > 0 ? undefined : messageId}
            onSelect={select}
            selectedEntryIds={selectedIds}
            onLongPress={handleLongPress}
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
              const relevant = targetMsg?.entries[0]
              if (relevant && !relevant.flags.includes(SEEN)) {
                setFlags.mutate({ entryId: relevant.entry_id, add: [SEEN] })
              }
              setSelectedIds(new Set())
              navigate(`/search/message/${id}${q ? `?q=${encodeURIComponent(q)}` : ''}`)
            }}
            onDeselectEntry={handleDeselectEntry}
            onClearSelection={() => setSelectedIds(new Set())}
            onBulkRead={handleBulkRead}
            onBulkUnread={handleBulkUnread}
            onBulkDelete={handleBulkDelete}
            isPending={bulk.isPending}
            mailboxId={undefined}
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
          <MessageViewer message={message.data} mailboxId={undefined} onClose={close} />
        ) : null}
      </section>
    </div>
  )
}
