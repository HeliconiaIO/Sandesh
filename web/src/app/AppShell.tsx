/**
 * The persistent application shell (§9).
 *
 * Header, navigation and the outlet the routed views render into. It holds no
 * mail logic: it knows the connection state, the theme and where the user is,
 * and nothing about messages.
 */

import { NavLink, Outlet, useLocation, useNavigate } from 'react-router-dom'
import { LoadingState } from '../components/States'
import { Suspense, useEffect, useRef, useState, type ReactNode } from 'react'
import {
  ArrowLeftRight,
  BookOpen,
  Code2,
  Inbox,
  Info,
  Keyboard,
  Layers,
  LayoutDashboard,
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
  type LucideIcon,
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

interface NavEntry {
  to: string
  label: string
  icon: LucideIcon
  end?: boolean
  badge?: ReactNode
}

const DIAGNOSTICS: NavEntry[] = [
  { to: '/traces', label: 'Traces', icon: Radio },
  { to: '/logs', label: 'Logs', icon: ScrollText },
  { to: '/api', label: 'API Console', icon: Code2 },
  { to: '/webhooks', label: 'Webhooks', icon: Webhook },
  { to: '/transfer', label: 'Import / Export', icon: ArrowLeftRight },
]

const SYSTEM: NavEntry[] = [
  { to: '/guide', label: 'Setup Guide', icon: BookOpen },
  { to: '/workspaces', label: 'Workspaces & DB', icon: Layers },
  { to: '/settings', label: 'Settings', icon: Settings },
  { to: '/about', label: 'About', icon: Info },
]

/**
 * One navigation row. The label stays in the DOM when the sidebar collapses to
 * an icon rail — it is clipped rather than removed, so the link keeps its
 * accessible name and `title` supplies the visible tooltip.
 */
function NavItem({ to, label, icon: Icon, end, badge }: NavEntry) {
  return (
    <NavLink to={to} end={end ?? false} className="nav-item" title={label}>
      <Icon size={16} aria-hidden="true" className="nav-item__icon" />
      <span className="nav-item__label">{label}</span>
      {badge}
    </NavLink>
  )
}

/**
 * `g` + key jumps for every top-level page. The mailbox jumps (g i/s/d/t/a)
 * and g / for Search are defined inline below; these keys avoid those.
 */
const PAGE_CHORDS: ReadonlyArray<readonly [key: string, path: string, label: string]> = [
  ['o', '/', 'Overview'],
  ['r', '/traces', 'Traces'],
  ['l', '/logs', 'Logs'],
  ['c', '/api', 'API Console'],
  ['w', '/webhooks', 'Webhooks'],
  ['e', '/transfer', 'Import / Export'],
  ['h', '/guide', 'Setup Guide'],
  ['b', '/workspaces', 'Workspaces & DB'],
  [',', '/settings', 'Settings'],
]

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
    // One `g` chord per page, so nothing depends on finding it in the sidebar.
    // The `?` shortcuts help lists these from their descriptions.
    ...PAGE_CHORDS.map(([key, path, label]) => ({
      chord: ['g', key],
      description: `Jump to ${label}`,
      category: 'Global & Navigation' as const,
      action: () => navigate(path),
    })),
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
    <div className={`shell${navOpen ? ' shell--nav-open' : ''}`}>
      <a className="skip-link" href="#main">
        Skip to content
      </a>

      <header className="shell__header">
        <div className="shell__header-start">
          <button
            type="button"
            className="shell__nav-toggle"
            onClick={() => setNavOpen((open) => !open)}
            aria-expanded={navOpen}
            aria-controls="main-nav"
            aria-label={navOpen ? 'Close navigation' : 'Open navigation'}
          >
            {navOpen ? <X size={17} aria-hidden="true" /> : <Menu size={17} aria-hidden="true" />}
          </button>

          <div className="shell__brand">
            <img
              src={logoMarkDark}
              alt=""
              className="shell__logo-img shell__logo-img--dark"
              width={24}
              height={24}
              aria-hidden="true"
            />
            <img
              src={logoMarkLight}
              alt=""
              className="shell__logo-img shell__logo-img--light"
              width={24}
              height={24}
              aria-hidden="true"
            />
            <span className="shell__name">Sandesh</span>
            <span className="shell__version">v{info?.version ?? '0.3.0'}</span>
          </div>

          <span className="shell__divider" aria-hidden="true" />

          <WorkspaceSwitcher />
        </div>

        <div className="shell__search-container">
          <form onSubmit={handleHeaderSearchSubmit} className="shell__search" role="search">
            <Search size={15} className="shell__search-icon" aria-hidden="true" />
            <input
              ref={searchInputRef}
              type="text"
              className="shell__search-input"
              placeholder="Search mail, senders, or from:alice is:unread"
              value={headerSearch}
              onChange={(e) => setHeaderSearch(e.target.value)}
              autoComplete="off"
              spellCheck="false"
              aria-label="Search mail"
            />
            <kbd className="kbd shell__search-kbd" aria-hidden="true">
              {formatKey('Mod')}K
            </kbd>
          </form>
        </div>

        <div className="shell__status">
          {info?.smtp_address ? (
            <span className="endpoint" title="Point your application's SMTP client here">
              <span className="endpoint__label">SMTP</span>
              <span className="endpoint__value">{info.smtp_address}</span>
              <CopyButton value={info.smtp_address} label="SMTP host and port" />
            </span>
          ) : null}

          <FaultIndicator faults={faults} />
          <ConnectionStatus state={connection} />

          <span className="shell__divider" aria-hidden="true" />

          <button
            type="button"
            className="theme-toggle"
            onClick={openShortcutsModal}
            title={`Keyboard shortcuts (${formatKey('?')})`}
            aria-label="Keyboard shortcuts"
          >
            <Keyboard size={16} aria-hidden="true" />
          </button>

          <button
            type="button"
            className="theme-toggle"
            onClick={() => setTheme(isLight ? 'dark' : 'light')}
            title={`Switch to ${isLight ? 'dark' : 'light'} theme`}
            aria-label={`Switch to ${isLight ? 'dark' : 'light'} theme`}
          >
            {isLight ? <Moon size={16} aria-hidden="true" /> : <Sun size={16} aria-hidden="true" />}
          </button>
        </div>
      </header>

      <div className="shell__body">
        {/* Dismisses the drawer by clicking away from it. */}
        {navOpen ? (
          <div
            className="shell__nav-overlay"
            role="presentation"
            onClick={() => setNavOpen(false)}
          />
        ) : null}

        <aside
          id="main-nav"
          className={`shell__nav${navOpen ? ' shell__nav--open' : ''}`}
          aria-label="Main"
        >
          <div className="shell__nav-top">
            <button
              type="button"
              className="shell__compose-btn"
              onClick={() => openComposer()}
              title="Compose (C)"
            >
              <PenSquare size={15} aria-hidden="true" />
              <span className="shell__compose-label">Compose</span>
              <kbd className="shell__compose-kbd" aria-hidden="true">
                C
              </kbd>
            </button>
          </div>

          <div className="shell__nav-scroll">
            <nav className="nav-section" aria-label="Primary">
              <NavItem to="/" end label="Overview" icon={LayoutDashboard} />
              <NavItem to="/search" label="Search" icon={Search} />
            </nav>

            <div className="nav-section">
              <div className="nav-heading">Mailboxes</div>
              <MailboxNav />
            </div>

            <nav className="nav-section" aria-label="Diagnostics">
              <div className="nav-heading">Diagnostics</div>
              {DIAGNOSTICS.map((entry) => (
                <NavItem key={entry.to} {...entry} />
              ))}
            </nav>
          </div>

          <nav className="shell__nav-footer nav-section" aria-label="System">
            {SYSTEM.map((entry) => (
              <NavItem key={entry.to} {...entry} />
            ))}
          </nav>
        </aside>

        <main className="shell__main" id="main">
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
