/**
 * The persistent application shell (§9).
 *
 * Header, navigation and the outlet the routed views render into. It holds no
 * mail logic: it knows the connection state, the theme and where the user is,
 * and nothing about messages.
 */

import { NavLink, Outlet, useLocation, useNavigate } from 'react-router-dom'
import { LoadingState } from '../components/States'
import { Suspense, useEffect, useRef, useState } from 'react'
import {
  Activity,
  ArrowLeftRight,
  BookOpen,
  Inbox,
  Info,
  Keyboard,
  Layers,
  Menu,
  Moon,
  PenSquare,
  Radio,
  ScrollText,
  Search,
  Settings,
  Sun,
  Terminal,
  Webhook,
  X,
} from 'lucide-react'

import { ConnectionStatus } from '../components/ConnectionStatus'
import { FaultIndicator } from '../components/FaultIndicator'
import { CopyButton } from '../components/CopyButton'
import { ShortcutsModal } from '../components/ShortcutsModal'
import { WorkspaceSwitcher } from '../components/WorkspaceSwitcher'
import { MailboxNav } from '../features/mailboxes/MailboxNav'
import { useComposer } from '../features/composer/ComposerContext'
import { useKeyboardShortcutsContext } from '../context/KeyboardShortcutsContext'
import { useKeyboardShortcuts } from '../hooks/useKeyboardShortcuts'
import { useConnectionState, useEventSync } from '../hooks/useEventStream'
import { useFaults, useInfo, useMailboxes } from '../hooks/queries'
import { useTheme } from '../hooks/useTheme'
import logoMarkLight from '../assets/sandesh-mark.png'
import logoMarkDark from '../assets/sandesh-mark-dark.png'
import './AppShell.css'

export function AppShell() {
  const connection = useConnectionState()
  const { data: info } = useInfo()
  const { data: faults } = useFaults()
  const { data: mailboxes } = useMailboxes()
  const { preference, setTheme } = useTheme()
  const { openComposer } = useComposer()
  const { openShortcutsModal, formatKey } = useKeyboardShortcutsContext()
  const navigate = useNavigate()
  const location = useLocation()
  const [navOpen, setNavOpen] = useState(false)
  const searchInputRef = useRef<HTMLInputElement>(null)
  const [headerSearch, setHeaderSearch] = useState('')
  useEventSync()

  // Sync header search input with URL search params when on /search
  useEffect(() => {
    if (location.pathname.startsWith('/search')) {
      const q = new URLSearchParams(location.search).get('q') ?? ''
      setHeaderSearch(q)
    }
  }, [location.pathname, location.search])

  // Following a link must dismiss the drawer, or the destination renders
  // underneath it and the drawer has to be closed by hand to see the result.
  useEffect(() => {
    setNavOpen(false)
  }, [location.pathname])

  // Prevent background scroll when mobile drawer is open
  useEffect(() => {
    if (navOpen) {
      document.body.style.overflow = 'hidden'
    } else {
      document.body.style.overflow = ''
    }
    return () => {
      document.body.style.overflow = ''
    }
  }, [navOpen])

  // Escape closes it, as it does every other overlay in the app.
  useEffect(() => {
    if (!navOpen) return undefined

    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setNavOpen(false)
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [navOpen])

  const findMailboxId = (predicate: (name: string) => boolean) => {
    return mailboxes?.items?.find((m) => predicate(m.name.toLowerCase()))?.id
  }

  // Global navigation & action keyboard shortcuts
  useKeyboardShortcuts([
    {
      key: 'k',
      mod: true,
      description: 'Focus search bar',
      category: 'Global & Navigation',
      allowInInputs: true,
      action: () => {
        searchInputRef.current?.focus()
        searchInputRef.current?.select()
      },
    },
    {
      key: '/',
      description: 'Focus search bar',
      category: 'Global & Navigation',
      action: () => {
        searchInputRef.current?.focus()
        searchInputRef.current?.select()
      },
    },
    {
      key: 'c',
      description: 'Compose new message',
      category: 'Global & Navigation',
      action: () => {
        openComposer()
      },
    },
    {
      chord: ['g', 'i'],
      description: 'Jump to Inbox',
      category: 'Global & Navigation',
      action: () => {
        const inboxId = findMailboxId((n) => n === 'inbox')
        if (inboxId) navigate(`/mailbox/${inboxId}`)
        else navigate('/')
      },
    },
    {
      chord: ['g', 's'],
      description: 'Jump to Sent',
      category: 'Global & Navigation',
      action: () => {
        const sentId = findMailboxId((n) => n === 'sent' || n.includes('sent'))
        if (sentId) navigate(`/mailbox/${sentId}`)
      },
    },
    {
      chord: ['g', 'd'],
      description: 'Jump to Drafts',
      category: 'Global & Navigation',
      action: () => {
        const draftsId = findMailboxId((n) => n === 'drafts' || n === 'draft')
        if (draftsId) navigate(`/mailbox/${draftsId}`)
      },
    },
    {
      chord: ['g', 't'],
      description: 'Jump to Trash',
      category: 'Global & Navigation',
      action: () => {
        const trashId = findMailboxId((n) => n === 'trash' || n === 'deleted' || n === 'bin')
        if (trashId) navigate(`/mailbox/${trashId}`)
      },
    },
    {
      chord: ['g', 'a'],
      description: 'Jump to Archive',
      category: 'Global & Navigation',
      action: () => {
        const archiveId = findMailboxId((n) => n === 'archive')
        if (archiveId) navigate(`/mailbox/${archiveId}`)
      },
    },
    {
      chord: ['g', '/'],
      description: 'Jump to Search view',
      category: 'Global & Navigation',
      action: () => {
        navigate('/search')
      },
    },
  ])

  const handleHeaderSearchSubmit = (e: React.FormEvent) => {
    e.preventDefault()
    if (headerSearch.trim()) {
      navigate(`/search?q=${encodeURIComponent(headerSearch.trim())}`)
    } else {
      navigate('/search')
    }
  }

  const isLight =
    preference === 'light' ||
    (preference === 'system' &&
      typeof window !== 'undefined' &&
      window.matchMedia?.('(prefers-color-scheme: light)').matches)

  return (
    <div className="shell">
      <a className="skip-link" href="#main">
        Skip to content
      </a>

      <header className="shell__header">
        {/* Shown only at phone width, where the sidebar is off-canvas. */}
        <button
          type="button"
          className="shell__nav-toggle"
          onClick={() => setNavOpen((open) => !open)}
          aria-expanded={navOpen}
          aria-controls="main-nav"
          aria-label={navOpen ? 'Close navigation' : 'Open navigation'}
        >
          {navOpen ? (
            <X size={16} aria-hidden="true" />
          ) : (
            <Menu size={16} aria-hidden="true" />
          )}
        </button>

        <div className="shell__brand">
          <img
            src={logoMarkDark}
            alt=""
            className="shell__logo-img shell__logo-img--dark"
            width={35}
            height={26}
            aria-hidden="true"
          />
          <img
            src={logoMarkLight}
            alt=""
            className="shell__logo-img shell__logo-img--light"
            width={35}
            height={26}
            aria-hidden="true"
          />
          <span className="shell__name">Sandesh</span>
        </div>

        <div className="shell__search-container">
          <form onSubmit={handleHeaderSearchSubmit} className="shell__search">
            <Search size={14} className="shell__search-icon" aria-hidden="true" />
            <input
              ref={searchInputRef}
              type="text"
              className="shell__search-input"
              placeholder="Search all mail (e.g. from:, to:, is:unread)..."
              value={headerSearch}
              onChange={(e) => setHeaderSearch(e.target.value)}
              autoComplete="off"
              spellCheck="false"
              aria-label="Search mail"
            />
            <div className="shell__search-shortcuts">
              <kbd className="shell__search-kbd">{formatKey('Mod')}</kbd>
              <kbd className="shell__search-kbd">K</kbd>
            </div>
          </form>
        </div>

        <div className="shell__status">
          {/* Active database / workspace quick switcher */}
          <WorkspaceSwitcher />

          {/* The endpoint a developer needs most, always in reach (§60). */}
          {info?.smtp_address ? (
            <span className="endpoint">
              <span className="endpoint__label">SMTP</span>
              <code className="endpoint__value">{info.smtp_address}</code>
              <CopyButton value={info.smtp_address} label="the SMTP address" />
            </span>
          ) : null}
          <FaultIndicator faults={faults} />
          <ConnectionStatus state={connection} />

          <button
            type="button"
            className="theme-toggle"
            onClick={openShortcutsModal}
            title={`Keyboard shortcuts (${formatKey('?')})`}
            aria-label="Keyboard shortcuts"
          >
            <Keyboard size={14} aria-hidden="true" />
          </button>

          <button
            type="button"
            className="theme-toggle"
            onClick={() => setTheme(isLight ? 'dark' : 'light')}
            title={`Switch to ${isLight ? 'dark' : 'light'} theme`}
            aria-label={`Switch to ${isLight ? 'dark' : 'light'} theme`}
          >
            {isLight ? <Moon size={14} aria-hidden="true" /> : <Sun size={14} aria-hidden="true" />}
          </button>
        </div>
      </header>

      <div className="shell__body">
        {/* Dismisses the drawer by clicking away from it. Inert and invisible
            above the breakpoint, where the sidebar is simply the sidebar. */}
        {navOpen ? (
          <div
            className="shell__nav-overlay"
            role="presentation"
            onClick={() => setNavOpen(false)}
          />
        ) : null}

        <nav
          id="main-nav"
          className={`shell__nav${navOpen ? ' shell__nav--open' : ''}`}
          aria-label="Main"
        >
          <button
            type="button"
            className="shell__compose-btn"
            onClick={() => openComposer()}
          >
            <PenSquare size={14} aria-hidden="true" />
            <span>Compose</span>
          </button>

          <div className="nav-section">
            <NavLink to="/" end className="nav-item">
              <Activity size={15} aria-hidden="true" />
              <span>Overview</span>
            </NavLink>

            <NavLink to="/search" className="nav-item">
              <Search size={15} aria-hidden="true" />
              <span>Search</span>
            </NavLink>
          </div>

          <div className="nav-section">
            <h2 className="nav-heading" id="nav-mail">
              Mail
            </h2>
            <MailboxNav />
          </div>

          <div className="nav-section">
            <h2 className="nav-heading">System</h2>
            <NavLink to="/traces" className="nav-item">
              <Radio size={15} aria-hidden="true" />
              <span>Traces</span>
            </NavLink>
            <NavLink to="/guide" className="nav-item">
              <BookOpen size={15} aria-hidden="true" />
              <span>Setup Guide</span>
            </NavLink>
            <NavLink to="/api" className="nav-item">
              <Terminal size={15} aria-hidden="true" />
              <span>API Console</span>
            </NavLink>
            <NavLink to="/logs" className="nav-item">
              <ScrollText size={15} aria-hidden="true" />
              <span>Logs</span>
            </NavLink>
            <NavLink to="/webhooks" className="nav-item">
              <Webhook size={15} aria-hidden="true" />
              <span>Webhooks</span>
            </NavLink>
            <NavLink to="/transfer" className="nav-item">
              <ArrowLeftRight size={15} aria-hidden="true" />
              <span>Import / Export</span>
            </NavLink>
            <NavLink to="/workspaces" className="nav-item">
              <Layers size={15} aria-hidden="true" />
              <span>Workspaces</span>
            </NavLink>
            <NavLink to="/settings" className="nav-item">
              <Settings size={15} aria-hidden="true" />
              <span>Settings</span>
            </NavLink>
            <NavLink to="/about" className="nav-item">
              <Info size={15} aria-hidden="true" />
              <span>About</span>
            </NavLink>
          </div>
        </nav>

        <main className="shell__main" id="main">
          {/*
           * The boundary for lazily-loaded routes (§12) sits here rather than
           * around the shell, so a first visit to a split route resolves
           * inside the content pane while the header and navigation stay on
           * screen. Put it any higher and the whole app blanks for a moment.
           */}
          <Suspense fallback={<LoadingState label="Loading view" />}>
            <Outlet />
          </Suspense>
        </main>
      </div>

      <ShortcutsModal />
    </div>
  )
}

/** Shown while a lazily-loaded route is still arriving. */
export function RouteFallback() {
  return (
    <div className="shell__fallback" role="status" aria-live="polite">
      <Inbox size={18} aria-hidden="true" />
      <span>Loading</span>
    </div>
  )
}
