import { useEffect, useMemo, useRef, useState } from 'react'
import { Command, Keyboard, Search, X } from 'lucide-react'
import { useKeyboardShortcutsContext } from '../context/KeyboardShortcutsContext'
import './ShortcutsModal.css'

interface StaticShortcut {
  combo: string[]
  label: string
  category: 'Global & Navigation' | 'Message List' | 'Actions' | 'Inspector' | 'Composer'
}

const ALL_SHORTCUTS: StaticShortcut[] = [
  // Global & Navigation
  { combo: ['?'], label: 'Toggle keyboard shortcuts cheatsheet', category: 'Global & Navigation' },
  { combo: ['Mod', 'K'], label: 'Focus global search bar', category: 'Global & Navigation' },
  { combo: ['/'], label: 'Focus search bar', category: 'Global & Navigation' },
  { combo: ['C'], label: 'Compose new message', category: 'Global & Navigation' },
  { combo: ['Esc'], label: 'Close message / modal / clear selection', category: 'Global & Navigation' },
  { combo: ['G', 'I'], label: 'Jump to Inbox', category: 'Global & Navigation' },
  { combo: ['G', 'S'], label: 'Jump to Sent', category: 'Global & Navigation' },
  { combo: ['G', 'D'], label: 'Jump to Drafts', category: 'Global & Navigation' },
  { combo: ['G', 'T'], label: 'Jump to Trash', category: 'Global & Navigation' },
  { combo: ['G', 'A'], label: 'Jump to Archive', category: 'Global & Navigation' },
  { combo: ['G', '/'], label: 'Jump to Search view', category: 'Global & Navigation' },

  // Message List
  { combo: ['J', 'or', '↓'], label: 'Select next message', category: 'Message List' },
  { combo: ['K', 'or', '↑'], label: 'Select previous message', category: 'Message List' },
  { combo: ['Enter', 'or', 'O'], label: 'Open selected message', category: 'Message List' },
  { combo: ['X'], label: 'Toggle message selection checkbox', category: 'Message List' },
  { combo: ['*', 'A'], label: 'Select all messages in folder', category: 'Message List' },
  { combo: ['*', 'N'], label: 'Deselect all messages', category: 'Message List' },

  // Actions
  { combo: ['M'], label: 'Toggle read / unread status', category: 'Actions' },
  { combo: ['Shift', 'I'], label: 'Mark as read', category: 'Actions' },
  { combo: ['Shift', 'U'], label: 'Mark as unread', category: 'Actions' },
  { combo: ['E', 'or', 'Y'], label: 'Archive selected message(s)', category: 'Actions' },
  { combo: ['#', 'or', 'Delete'], label: 'Move to Trash / Delete', category: 'Actions' },
  { combo: ['R'], label: 'Reply to message', category: 'Actions' },

  // Inspector
  { combo: ['1'], label: 'Switch to Preview tab (HTML/Text)', category: 'Inspector' },
  { combo: ['2'], label: 'Switch to Headers tab', category: 'Inspector' },
  { combo: ['3'], label: 'Switch to MIME tree tab', category: 'Inspector' },
  { combo: ['4'], label: 'Switch to Raw source tab', category: 'Inspector' },
  { combo: ['5'], label: 'Switch to Attachments tab', category: 'Inspector' },
  { combo: ['6'], label: 'Switch to Delivery tab', category: 'Inspector' },
  { combo: ['[', ']'], label: 'Cycle previous / next inspector tab', category: 'Inspector' },

  // Composer
  { combo: ['Mod', 'Enter'], label: 'Send email message', category: 'Composer' },
  { combo: ['Esc'], label: 'Close or cancel composer', category: 'Composer' },
]

export function ShortcutsModal() {
  const { shortcutsModalOpen, closeShortcutsModal, os, formatKey } = useKeyboardShortcutsContext()
  const [filterQuery, setFilterQuery] = useState('')
  const modalRef = useRef<HTMLDivElement>(null)
  const searchInputRef = useRef<HTMLInputElement>(null)

  // Focus filter input on open
  useEffect(() => {
    if (shortcutsModalOpen) {
      setFilterQuery('')
      setTimeout(() => {
        searchInputRef.current?.focus()
      }, 50)
    }
  }, [shortcutsModalOpen])

  const filteredShortcuts = useMemo(() => {
    const q = filterQuery.trim().toLowerCase()
    if (!q) return ALL_SHORTCUTS
    return ALL_SHORTCUTS.filter(
      (s) =>
        s.label.toLowerCase().includes(q) ||
        s.category.toLowerCase().includes(q) ||
        s.combo.some((k) => k.toLowerCase().includes(q)),
    )
  }, [filterQuery])

  const grouped = useMemo(() => {
    const map = new Map<string, StaticShortcut[]>()
    for (const s of filteredShortcuts) {
      const list = map.get(s.category) ?? []
      list.push(s)
      map.set(s.category, list)
    }
    return Array.from(map.entries())
  }, [filteredShortcuts])

  if (!shortcutsModalOpen) return null

  return (
    <div
      className="shortcuts-modal__overlay"
      role="presentation"
      onClick={(e) => {
        if (e.target === e.currentTarget) closeShortcutsModal()
      }}
    >
      <div
        ref={modalRef}
        className="shortcuts-modal__dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="shortcuts-modal-title"
      >
        <header className="shortcuts-modal__header">
          <div className="shortcuts-modal__title-wrap">
            <div className="shortcuts-modal__icon" aria-hidden="true">
              <Keyboard size={18} />
            </div>
            <div>
              <h2 id="shortcuts-modal-title" className="shortcuts-modal__title">
                Keyboard Shortcuts
              </h2>
              <span className="shortcuts-modal__badge">
                {os === 'mac' ? 'macOS (⌘ Cmd, ⌥ Option)' : 'Windows & Linux (Ctrl, Alt)'}
              </span>
            </div>
          </div>

          <button
            type="button"
            className="shortcuts-modal__close"
            onClick={closeShortcutsModal}
            aria-label="Close shortcuts modal"
            title="Close (Esc)"
          >
            <X size={16} aria-hidden="true" />
          </button>
        </header>

        <div className="shortcuts-modal__search-wrap">
          <Search size={14} className="shortcuts-modal__search-icon" aria-hidden="true" />
          <input
            ref={searchInputRef}
            type="text"
            className="shortcuts-modal__search"
            placeholder="Search hotkeys (e.g. archive, reply, compose, inbox)..."
            value={filterQuery}
            onChange={(e) => setFilterQuery(e.target.value)}
            aria-label="Filter shortcuts"
          />
        </div>

        <div className="shortcuts-modal__content">
          {grouped.length === 0 ? (
            <div className="shortcuts-modal__empty">
              <Command size={24} className="shortcuts-modal__empty-icon" aria-hidden="true" />
              <p>No shortcuts match &ldquo;{filterQuery}&rdquo;</p>
            </div>
          ) : (
            <div className="shortcuts-modal__grid">
              {grouped.map(([category, list]) => (
                <section key={category} className="shortcuts-modal__group">
                  <h3 className="shortcuts-modal__group-title">{category}</h3>
                  <div className="shortcuts-modal__list">
                    {list.map((item, idx) => (
                      <div key={idx} className="shortcuts-modal__row">
                        <span className="shortcuts-modal__label">{item.label}</span>
                        <div className="shortcuts-modal__keys">
                          {item.combo.map((k, kIdx) => {
                            if (k === 'or' || k === 'then') {
                              return (
                                <span key={kIdx} className="shortcuts-modal__or">
                                  {k}
                                </span>
                              )
                            }
                            return (
                              <kbd key={kIdx} className="shortcuts-modal__kbd">
                                {formatKey(k)}
                              </kbd>
                            )
                          })}
                        </div>
                      </div>
                    ))}
                  </div>
                </section>
              ))}
            </div>
          )}
        </div>

        <footer className="shortcuts-modal__footer">
          <span>Press <kbd className="shortcuts-modal__kbd">?</kbd> anywhere to toggle this cheatsheet</span>
          <span>Press <kbd className="shortcuts-modal__kbd">Esc</kbd> to close</span>
        </footer>
      </div>
    </div>
  )
}
