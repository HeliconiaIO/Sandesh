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
  Code2,
  Inbox,
  Info,
  Layers,
  Menu,
  Moon,
  PenSquare,
  Radio,
  ScrollText,
  Search,
  Settings,
  Sun,
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

function formatPort(addr: string | null | undefined): string | null {
  if (!addr) return null
  const lastColon = addr.lastIndexOf(':')
  if (lastColon >= 0) {
    return addr.slice(lastColon + 1)
  }
  return addr
}

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

  // Calculate total messages across all mailboxes for the sidebar badge
  const totalMessages = mailboxes?.items?.reduce((acc, mb) => acc + (mb.total ?? 0), 0) ?? null
  const defaultInbox = mailboxes?.items?.find((m) => m.name.toLowerCase() === 'inbox') ?? mailboxes?.items?.[0]

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

  const smtpPort = formatPort(info?.smtp_address) ?? '1025'
  const imapPort = formatPort(info?.imap_address) ?? '1143'
  const pop3Port = formatPort(info?.pop3_address) ?? '1110'

  return (
    <div className="shell flex flex-col h-screen w-screen overflow-hidden bg-surface text-on-surface">
      <a className="skip-link" href="#main">
        Skip to content
      </a>

      {/* Fixed Precision Top Header (§48px) */}
      <header className="shell__header h-12 flex-none px-space-md bg-surface-container-lowest/95 backdrop-blur-md border-b border-border shadow-[0_1px_8px_rgba(0,0,0,0.35)] flex items-center justify-between z-50">
        <div className="flex items-center gap-space-md min-w-0">
          {/* Mobile drawer toggle */}
          <button
            type="button"
            className="shell__nav-toggle lg:hidden flex items-center justify-center w-7 h-7 rounded hover:bg-surface-container text-on-surface-variant hover:text-on-surface transition-colors"
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

          {/* Brand & Version Badge */}
          <div className="shell__brand flex items-center gap-space-xs shrink-0">
            <img
              src={logoMarkDark}
              alt=""
              className="shell__logo-img shell__logo-img--dark h-6 w-auto object-contain"
              width={24}
              height={24}
              aria-hidden="true"
            />
            <img
              src={logoMarkLight}
              alt=""
              className="shell__logo-img shell__logo-img--light h-6 w-auto object-contain"
              width={24}
              height={24}
              aria-hidden="true"
            />
            <span className="shell__name font-headline-sm text-headline-sm font-semibold tracking-tight text-on-surface">
              Sandesh
            </span>
            <span className="font-label-code-xs text-label-code-xs bg-surface-container-high text-on-surface-variant px-1.5 py-0.5 rounded font-medium">
              v{info?.version ?? '0.2.0'}
            </span>
          </div>

          <div className="h-4 w-px bg-surface-container-highest shrink-0 hidden sm:block" />

          {/* Active Workspace Switcher */}
          <WorkspaceSwitcher />
        </div>

        {/* Global Instant Search Bar */}
        <div className="shell__search-container flex-1 max-w-md mx-space-md hidden md:flex items-center">
          <form onSubmit={handleHeaderSearchSubmit} className="shell__search w-full relative flex items-center">
            <Search size={15} className="shell__search-icon absolute left-2.5 text-outline pointer-events-none" aria-hidden="true" />
            <input
              ref={searchInputRef}
              type="text"
              className="shell__search-input w-full pl-8 pr-14 py-1 bg-surface-container-low rounded font-body-sm text-body-sm text-on-surface placeholder:text-outline focus:outline-none focus:bg-surface-container transition-colors"
              placeholder="Filter sender, subject, or query... (/)"
              value={headerSearch}
              onChange={(e) => setHeaderSearch(e.target.value)}
              autoComplete="off"
              spellCheck="false"
              aria-label="Search mail"
            />
            <div className="shell__search-shortcuts absolute right-1.5 flex items-center gap-0.5 pointer-events-none">
              <kbd className="shell__search-kbd font-label-code-xs text-label-code-xs px-1.5 py-0.5 rounded bg-surface-container-high text-on-surface-variant font-medium">
                {formatKey('Mod')}K
              </kbd>
            </div>
          </form>
        </div>

        {/* Status, Diagnostics, and Endpoints Group */}
        <div className="shell__status flex items-center gap-space-sm shrink-0">
          {/* Protocol Port Chips */}
          <div className="hidden xl:flex items-center gap-1.5">
            {info?.smtp_address ? (
              <span className="endpoint group flex items-center gap-1 px-2 py-0.5 rounded bg-surface-container-low font-label-code-xs text-label-code-xs text-on-surface-variant hover:text-on-surface hover:bg-surface-container transition-colors">
                <span className="text-outline">SMTP</span>
                <span className="text-primary font-medium">{smtpPort}</span>
                <CopyButton value={info.smtp_address} label="SMTP host and port" />
              </span>
            ) : null}

            {info?.imap_address ? (
              <span className="flex items-center gap-1 px-2 py-0.5 rounded bg-surface-container-low font-label-code-xs text-label-code-xs text-on-surface-variant">
                <span className="text-outline">IMAP</span>
                <span className="text-on-surface font-medium">{imapPort}</span>
              </span>
            ) : null}

            {info?.pop3_address ? (
              <span className="flex items-center gap-1 px-2 py-0.5 rounded bg-surface-container-low font-label-code-xs text-label-code-xs text-on-surface-variant">
                <span className="text-outline">POP3</span>
                <span className="text-on-surface font-medium">{pop3Port}</span>
              </span>
            ) : null}
          </div>

          <FaultIndicator faults={faults} />
          <ConnectionStatus state={connection} />

          <div className="h-4 w-px bg-surface-container-highest shrink-0 hidden sm:block" />

          {/* Action and Utility Buttons */}
          <div className="flex items-center gap-1">
            <button
              type="button"
              className="theme-toggle w-7 h-7 flex items-center justify-center rounded hover:bg-surface-container-high text-on-surface-variant hover:text-on-surface font-label-code-sm text-label-code-sm transition-colors"
              onClick={openShortcutsModal}
              title={`Keyboard shortcuts (${formatKey('?')})`}
              aria-label="Keyboard shortcuts"
            >
              ?
            </button>

            <button
              type="button"
              className="theme-toggle w-7 h-7 flex items-center justify-center rounded hover:bg-surface-container-high text-on-surface-variant hover:text-on-surface transition-colors"
              onClick={() => setTheme(isLight ? 'dark' : 'light')}
              title={`Switch to ${isLight ? 'dark' : 'light'} theme`}
              aria-label={`Switch to ${isLight ? 'dark' : 'light'} theme`}
            >
              {isLight ? <Moon size={15} aria-hidden="true" /> : <Sun size={15} aria-hidden="true" />}
            </button>
          </div>
        </div>
      </header>

      {/* Main Body: Sidebar + Workbench Stage */}
      <div className="shell__body flex flex-1 min-h-0 overflow-hidden relative">
        {/* Dismisses the drawer by clicking away from it. */}
        {navOpen ? (
          <div
            className="shell__nav-overlay fixed inset-0 top-12 z-40 bg-black/60 backdrop-blur-sm lg:hidden"
            role="presentation"
            onClick={() => setNavOpen(false)}
          />
        ) : null}

        {/* Sidebar Navigation */}
        <aside
          id="main-nav"
          className={`shell__nav w-64 bg-surface-container-lowest z-40 flex flex-col justify-between overflow-hidden shadow-[1px_0_12px_rgba(0,0,0,0.25)] border-r border-border shrink-0 ${
            navOpen ? 'shell__nav--open' : ''
          }`}
          aria-label="Main"
        >
          {/* Top Quick Compose Button */}
          <div className="p-space-sm pb-0">
            <button
              type="button"
              className="shell__compose-btn w-full flex items-center justify-between px-space-md py-2 bg-primary hover:bg-primary-container text-on-primary rounded font-body-md text-body-md font-medium transition-all shadow-[0_1px_3px_rgba(0,0,0,0.3)] cursor-pointer"
              onClick={() => openComposer()}
            >
              <span className="flex items-center gap-2">
                <PenSquare size={16} aria-hidden="true" />
                <span>Compose</span>
              </span>
              <kbd className="font-label-code-xs text-label-code-xs px-1.5 py-0.5 rounded bg-on-primary/20 text-on-primary">
                C
              </kbd>
            </button>
          </div>

          {/* Navigation Links and Mailbox Tree */}
          <div className="flex-1 overflow-y-auto px-space-xs py-space-sm space-y-4">
            <nav className="space-y-0.5">
              <NavLink
                to="/"
                end
                className={({ isActive }) =>
                  `flex items-center justify-between px-2.5 py-1.5 rounded transition-colors ${
                    isActive
                      ? 'bg-surface-container-high text-primary font-medium'
                      : 'text-on-surface-variant hover:bg-surface-container hover:text-on-surface'
                  }`
                }
              >
                <span className="flex items-center gap-2">
                  <Activity size={16} aria-hidden="true" />
                  <span className="font-body-sm text-body-sm">Overview</span>
                </span>
              </NavLink>

              <NavLink
                to={defaultInbox ? `/mailbox/${defaultInbox.id}` : '/mailbox'}
                className={({ isActive }) =>
                  `flex items-center justify-between px-2.5 py-1.5 rounded transition-colors ${
                    isActive || location.pathname.startsWith('/mailbox')
                      ? 'bg-surface-container-high text-primary font-medium'
                      : 'text-on-surface-variant hover:bg-surface-container hover:text-on-surface'
                  }`
                }
              >
                <span className="flex items-center gap-2">
                  <Inbox size={16} aria-hidden="true" />
                  <span className="font-body-sm text-body-sm">Mailbox</span>
                </span>
                {totalMessages !== null ? (
                  <span className="font-label-code-xs text-label-code-xs px-1.5 py-0.2 rounded bg-surface-container-highest text-on-surface">
                    {totalMessages}
                  </span>
                ) : null}
              </NavLink>

              <NavLink
                to="/search"
                className={({ isActive }) =>
                  `flex items-center justify-between px-2.5 py-1.5 rounded transition-colors ${
                    isActive
                      ? 'bg-surface-container-high text-primary font-medium'
                      : 'text-on-surface-variant hover:bg-surface-container hover:text-on-surface'
                  }`
                }
              >
                <span className="flex items-center gap-2">
                  <Search size={16} aria-hidden="true" />
                  <span className="font-body-sm text-body-sm">Search</span>
                </span>
              </NavLink>
            </nav>

            {/* Mailboxes and Accounts Tree */}
            <div className="space-y-1">
              <MailboxNav />
            </div>

            {/* Diagnostics and System Links */}
            <div className="space-y-1 pt-2 border-t border-border/50">
              <div className="px-2 pb-1">
                <span className="font-label-caps text-label-caps uppercase text-outline tracking-wider font-semibold">
                  Diagnostics &amp; System
                </span>
              </div>

              <NavLink
                to="/traces"
                className={({ isActive }) =>
                  `flex items-center justify-between px-2.5 py-1.5 rounded transition-colors ${
                    isActive
                      ? 'bg-surface-container-high text-primary font-medium'
                      : 'text-on-surface-variant hover:bg-surface-container hover:text-on-surface'
                  }`
                }
              >
                <span className="flex items-center gap-2">
                  <Radio size={16} aria-hidden="true" />
                  <span className="font-body-sm text-body-sm">Traces</span>
                </span>
                <span className="font-label-code-xs text-label-code-xs text-secondary font-medium">
                  RAW
                </span>
              </NavLink>

              <NavLink
                to="/guide"
                className={({ isActive }) =>
                  `flex items-center justify-between px-2.5 py-1.5 rounded transition-colors ${
                    isActive
                      ? 'bg-surface-container-high text-primary font-medium'
                      : 'text-on-surface-variant hover:bg-surface-container hover:text-on-surface'
                  }`
                }
              >
                <span className="flex items-center gap-2">
                  <BookOpen size={16} aria-hidden="true" />
                  <span className="font-body-sm text-body-sm">Setup Guide</span>
                </span>
              </NavLink>

              <NavLink
                to="/api"
                className={({ isActive }) =>
                  `flex items-center justify-between px-2.5 py-1.5 rounded transition-colors ${
                    isActive
                      ? 'bg-surface-container-high text-primary font-medium'
                      : 'text-on-surface-variant hover:bg-surface-container hover:text-on-surface'
                  }`
                }
              >
                <span className="flex items-center gap-2">
                  <Code2 size={16} aria-hidden="true" />
                  <span className="font-body-sm text-body-sm">API Console</span>
                </span>
              </NavLink>

              <NavLink
                to="/logs"
                className={({ isActive }) =>
                  `flex items-center justify-between px-2.5 py-1.5 rounded transition-colors ${
                    isActive
                      ? 'bg-surface-container-high text-primary font-medium'
                      : 'text-on-surface-variant hover:bg-surface-container hover:text-on-surface'
                  }`
                }
              >
                <span className="flex items-center gap-2">
                  <ScrollText size={16} aria-hidden="true" />
                  <span className="font-body-sm text-body-sm">Logs</span>
                </span>
              </NavLink>

              <NavLink
                to="/webhooks"
                className={({ isActive }) =>
                  `flex items-center justify-between px-2.5 py-1.5 rounded transition-colors ${
                    isActive
                      ? 'bg-surface-container-high text-primary font-medium'
                      : 'text-on-surface-variant hover:bg-surface-container hover:text-on-surface'
                  }`
                }
              >
                <span className="flex items-center gap-2">
                  <Webhook size={16} aria-hidden="true" />
                  <span className="font-body-sm text-body-sm">Webhooks</span>
                </span>
              </NavLink>

              <NavLink
                to="/transfer"
                className={({ isActive }) =>
                  `flex items-center justify-between px-2.5 py-1.5 rounded transition-colors ${
                    isActive
                      ? 'bg-surface-container-high text-primary font-medium'
                      : 'text-on-surface-variant hover:bg-surface-container hover:text-on-surface'
                  }`
                }
              >
                <span className="flex items-center gap-2">
                  <ArrowLeftRight size={16} aria-hidden="true" />
                  <span className="font-body-sm text-body-sm">Import / Export</span>
                </span>
              </NavLink>

              <NavLink
                to="/workspaces"
                className={({ isActive }) =>
                  `flex items-center justify-between px-2.5 py-1.5 rounded transition-colors ${
                    isActive
                      ? 'bg-surface-container-high text-primary font-medium'
                      : 'text-on-surface-variant hover:bg-surface-container hover:text-on-surface'
                  }`
                }
              >
                <span className="flex items-center gap-2">
                  <Layers size={16} aria-hidden="true" />
                  <span className="font-body-sm text-body-sm">Workspaces &amp; DB</span>
                </span>
              </NavLink>

              <NavLink
                to="/settings"
                className={({ isActive }) =>
                  `flex items-center justify-between px-2.5 py-1.5 rounded transition-colors ${
                    isActive
                      ? 'bg-surface-container-high text-primary font-medium'
                      : 'text-on-surface-variant hover:bg-surface-container hover:text-on-surface'
                  }`
                }
              >
                <span className="flex items-center gap-2">
                  <Settings size={16} aria-hidden="true" />
                  <span className="font-body-sm text-body-sm">Settings</span>
                </span>
              </NavLink>

              <NavLink
                to="/about"
                className={({ isActive }) =>
                  `flex items-center justify-between px-2.5 py-1.5 rounded transition-colors ${
                    isActive
                      ? 'bg-surface-container-high text-primary font-medium'
                      : 'text-on-surface-variant hover:bg-surface-container hover:text-on-surface'
                  }`
                }
              >
                <span className="flex items-center gap-2">
                  <Info size={16} aria-hidden="true" />
                  <span className="font-body-sm text-body-sm">About</span>
                </span>
              </NavLink>
            </div>
          </div>

          {/* Bottom Live System Telemetry Strip */}
          <div className="p-space-sm bg-surface-container-low font-label-code-xs text-label-code-xs space-y-1 text-on-surface-variant border-t border-border">
            <div className="flex items-center justify-between">
              <span>Status</span>
              <span className="text-secondary font-medium">Ready</span>
            </div>
            <div className="flex items-center justify-between">
              <span>Auth</span>
              <span className="text-on-surface font-medium capitalize">{info?.smtp_auth ?? 'Disabled'}</span>
            </div>
            <div className="flex items-center justify-between">
              <span>Database</span>
              <span className="text-primary font-medium truncate max-w-[120px]" title={info?.database ?? 'sandesh.db'}>
                {info?.database ? info.database.split('/').pop() : 'sandesh.db'}
              </span>
            </div>
          </div>
        </aside>

        {/* Content Outlet */}
        <main className="shell__main flex-1 min-w-0 flex overflow-hidden bg-surface" id="main">
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
    <div className="shell__fallback flex items-center justify-center gap-2 w-full text-on-surface-variant" role="status" aria-live="polite">
      <Inbox size={18} aria-hidden="true" />
      <span>Loading</span>
    </div>
  )
}
