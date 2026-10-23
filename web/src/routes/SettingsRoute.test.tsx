import { describe, expect, it, vi } from 'vitest'
import { fireEvent, screen, waitFor } from '@testing-library/react'

import { renderWithProviders } from '../test/render'
import { SettingsRoute } from './SettingsRoute'

// Mock the API resources used in queries
vi.mock('../api/resources', () => ({
  accounts: {
    list: vi.fn().mockResolvedValue({
      items: [
        {
          id: '019a0174-7c3b-7b29-bde0-f3a2cc1176fe',
          username: 'catch-all',
          is_catch_all: true,
          addresses: ['dev@example.test'],
          created_at: '2026-01-01T00:00:00Z',
        },
        {
          id: '019a003d-7bb0-7a50-a525-0f902d74c26d',
          username: 'qa-user',
          is_catch_all: false,
          addresses: ['qa@example.test'],
          created_at: '2026-01-02T00:00:00Z',
        },
      ],
    }),
    create: vi.fn(),
    patch: vi.fn(),
    delete: vi.fn(),
  },
  mailboxes: {
    list: vi.fn().mockResolvedValue({
      items: [
        {
          id: '019a0000-0000-7000-8000-000000000001',
          account_id: '019a0174-7c3b-7b29-bde0-f3a2cc1176fe',
          name: 'INBOX',
          subscribed: true,
          unread_count: 5,
          total_count: 50,
        },
      ],
    }),
  },
  health: {
    info: vi.fn().mockResolvedValue({
      smtp_address: '127.0.0.1:1025',
      imap_address: '127.0.0.1:1143',
      pop3_address: '127.0.0.1:1110',
      http_address: 'http://127.0.0.1:8080',
      version: '0.1.0',
    }),
    ready: vi.fn().mockResolvedValue({ ready: true }),
  },
  entries: {
    setFlags: vi.fn(),
    remove: vi.fn(),
    move: vi.fn(),
    bulk: vi.fn(),
  },
  messages: {
    list: vi.fn().mockResolvedValue({ items: [] }),
    get: vi.fn(),
    headers: vi.fn(),
    mime: vi.fn(),
    raw: vi.fn(),
    delivery: vi.fn(),
    send: vi.fn(),
    inject: vi.fn(),
  },
  search: {
    messages: vi.fn().mockResolvedValue({ items: [] }),
    explain: vi.fn(),
  },
  workspaces: {
    list: vi.fn().mockResolvedValue([
      {
        id: 'default',
        name: 'Default Workspace',
        description: 'Default dev database',
        database_path: 'sandesh.db',
        is_active: true,
        message_count: 50,
        size_bytes: 1048576,
        created_at: '2026-01-01T00:00:00Z',
        auth_required: false,
      },
    ]),
    create: vi.fn(),
    activate: vi.fn(),
    purge: vi.fn(),
    delete: vi.fn(),
    update: vi.fn(),
  },
}))

describe('SettingsRoute', () => {
  it('renders page header, tabs, and overview metrics', async () => {
    renderWithProviders(<SettingsRoute />, { route: '/settings' })

    expect(screen.getByRole('heading', { level: 1, name: 'Settings' })).toBeInTheDocument()
    expect(screen.queryByRole('tab', { name: /all settings/i })).not.toBeInTheDocument()
    const accountsTab = screen.getByRole('tab', { name: /accounts & routing/i })
    expect(accountsTab).toBeInTheDocument()
    expect(accountsTab).toHaveAttribute('aria-selected', 'true')
    expect(screen.getByRole('tab', { name: /protocols & endpoints/i })).toBeInTheDocument()
    expect(screen.getByRole('tab', { name: /appearance/i })).toBeInTheDocument()
    expect(screen.getByRole('tab', { name: /shortcuts/i })).toBeInTheDocument()

    // Overview metric cards
    await waitFor(() => {
      expect(screen.getByText('Configured Accounts')).toBeInTheDocument()
      expect(screen.getByText('Catch-All Target')).toBeInTheDocument()
      expect(screen.getByText('Bound Addresses')).toBeInTheDocument()
      expect(screen.getByText('Total Mailboxes')).toBeInTheDocument()
    })
  })

  it('renders account cards with username, badges, and addresses', async () => {
    renderWithProviders(<SettingsRoute />, { route: '/settings' })

    await waitFor(() => {
      expect(screen.getAllByText('catch-all').length).toBeGreaterThanOrEqual(1)
      expect(screen.getByText('qa-user')).toBeInTheDocument()
      expect(screen.getByText('dev@example.test')).toBeInTheDocument()
      expect(screen.getByText('qa@example.test')).toBeInTheDocument()
    })
  })

  it('switches between category tabs', async () => {
    renderWithProviders(<SettingsRoute />, { route: '/settings' })

    const protocolsTab = screen.getByRole('tab', { name: /protocols & endpoints/i })
    fireEvent.click(protocolsTab)

    // Should display Protocol Endpoints section
    expect(
      screen.getByRole('heading', { level: 2, name: /protocol endpoints & connections/i }),
    ).toBeInTheDocument()

    // SMTP, IMAP, POP3, and REST API cards should be visible
    expect(screen.getByText('SMTP')).toBeInTheDocument()
    expect(screen.getByText('IMAP')).toBeInTheDocument()
    expect(screen.getByText('POP3')).toBeInTheDocument()
    expect(screen.getByText('REST API')).toBeInTheDocument()

    // Accounts section should be hidden when filtered to protocols
    expect(screen.queryByText('Configured Accounts')).not.toBeInTheDocument()
  })

  it('renders visual theme selection cards and allows switching', async () => {
    renderWithProviders(<SettingsRoute />, { route: '/settings?tab=appearance' })

    expect(screen.getByText('System Theme')).toBeInTheDocument()
    expect(screen.getByText('Dark Theme')).toBeInTheDocument()
    expect(screen.getByText('Light Theme')).toBeInTheDocument()

    const darkCard = screen.getByRole('button', { name: /dark theme/i })
    fireEvent.click(darkCard)
    expect(darkCard).toHaveAttribute('aria-pressed', 'true')
  })

  it('renders categorized keyboard shortcuts', () => {
    renderWithProviders(<SettingsRoute />, { route: '/settings?tab=shortcuts' })

    expect(screen.getByText('Message List Navigation')).toBeInTheDocument()
    expect(screen.getByText('Global Controls & Search')).toBeInTheDocument()
    expect(screen.getByText('Message Inspector')).toBeInTheDocument()
  })

  it('opens account creation modal when clicking Add Account', async () => {
    renderWithProviders(<SettingsRoute />, { route: '/settings' })

    const addAccountBtn = screen.getByRole('button', { name: /add account/i })
    fireEvent.click(addAccountBtn)

    await waitFor(() => {
      expect(screen.getByText('Create New Account')).toBeInTheDocument()
    })
  })
})
