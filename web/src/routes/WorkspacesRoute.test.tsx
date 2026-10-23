import { describe, expect, it, vi, beforeEach } from 'vitest'
import { fireEvent, screen, waitFor, within } from '@testing-library/react'

import { renderWithProviders } from '../test/render'
import { WorkspacesRoute } from './WorkspacesRoute'
import type { Workspace } from '../api/client'

const list = vi.fn()
const activate = vi.fn()
const create = vi.fn()
const update = vi.fn()
const purge = vi.fn()
const remove = vi.fn()
const listAccounts = vi.fn().mockResolvedValue({ items: [] })

vi.mock('../api/resources', () => ({
  workspaces: {
    list: (...args: unknown[]) => list(...args),
    activate: (...args: unknown[]) => activate(...args),
    create: (...args: unknown[]) => create(...args),
    update: (...args: unknown[]) => update(...args),
    purge: (...args: unknown[]) => purge(...args),
    delete: (...args: unknown[]) => remove(...args),
  },
  accounts: { list: (...args: unknown[]) => listAccounts(...args) },
  mailboxes: { list: vi.fn().mockResolvedValue({ items: [] }) },
  messages: {},
  entries: {},
  health: {},
  search: {},
  connections: {},
  faults: {},
  webhooks: { list: vi.fn().mockResolvedValue([]) },
  transfer: {},
}))

const MOCK_WORKSPACES: Workspace[] = [
  {
    id: 'default',
    name: 'Default Workspace',
    description: 'System default workspace',
    created_at: '2026-01-01T00:00:00Z',
    is_active: true,
    auth_required: false,
    message_count: 42,
    size_bytes: 1048576,
    database_path: '/data/sandesh.db',
  },
  {
    id: 'billing-app',
    name: 'Billing App',
    description: 'Dedicated db for billing invoices and receipts',
    created_at: '2026-01-02T00:00:00Z',
    is_active: false,
    auth_required: true,
    message_count: 10,
    size_bytes: 524288,
    database_path: '/data/workspaces/billing-app/sandesh.db',
  },
]

describe('WorkspacesRoute', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    list.mockResolvedValue(MOCK_WORKSPACES)
  })

  it('renders all workspaces and summary metrics', async () => {
    renderWithProviders(<WorkspacesRoute />)

    await waitFor(() => {
      expect(screen.getByText('Workspaces & Databases')).toBeInTheDocument()
    })

    // Active workspace hero card and list
    expect(screen.getAllByText('Default Workspace').length).toBeGreaterThan(0)
    expect(screen.getAllByText('default').length).toBeGreaterThan(0)
    expect(screen.getByText('Currently Active')).toBeInTheDocument()

    // Second workspace
    expect(screen.getByText('Billing App')).toBeInTheDocument()
    expect(screen.getByText('billing-app')).toBeInTheDocument()

    // Message counts
    expect(screen.getByText('52')).toBeInTheDocument() // 42 + 10 = 52 total
  })

  it('switches the active workspace when clicking Switch to this', async () => {
    activate.mockResolvedValue({
      ...MOCK_WORKSPACES[1],
      is_active: true,
    })

    renderWithProviders(<WorkspacesRoute />)

    await waitFor(() => {
      expect(screen.getByText('Billing App')).toBeInTheDocument()
    })

    const switchBtn = screen.getByRole('button', { name: /Switch to this/i })
    fireEvent.click(switchBtn)

    await waitFor(() => {
      expect(activate).toHaveBeenCalledWith('billing-app')
    })
  })

  it('toggles authentication requirement via handleToggleAuth', async () => {
    update.mockResolvedValue({
      ...MOCK_WORKSPACES[0],
      auth_required: true,
    })

    renderWithProviders(<WorkspacesRoute />)

    await waitFor(() => {
      expect(screen.getByText('Require Auth')).toBeInTheDocument()
    })

    fireEvent.click(screen.getByText('Require Auth'))

    await waitFor(() => {
      expect(update).toHaveBeenCalledWith('default', { auth_required: true })
    })
  })

  it('opens ConnectClientModal when clicking Connect Client', async () => {
    renderWithProviders(<WorkspacesRoute />)

    await waitFor(() => {
      expect(screen.getByText('Connect Client')).toBeInTheDocument()
    })

    fireEvent.click(screen.getByText('Connect Client'))

    await waitFor(() => {
      expect(screen.getByText(/Connect to "Default Workspace"/i)).toBeInTheDocument()
      expect(screen.getByText('Odoo ERP')).toBeInTheDocument()
      expect(screen.getByText('Node.js')).toBeInTheDocument()
      expect(screen.getByText('Python')).toBeInTheDocument()
    })
  })

  it('opens create workspace modal and creates a workspace with password and auth', async () => {
    create.mockResolvedValue({
      id: 'auth-service',
      name: 'Auth Service',
      description: 'Authentication emails',
      created_at: '2026-01-03T00:00:00Z',
      is_active: true,
      auth_required: true,
      message_count: 0,
      size_bytes: 32768,
      database_path: '/data/workspaces/auth-service/sandesh.db',
    })

    renderWithProviders(<WorkspacesRoute />)

    await waitFor(() => {
      expect(screen.getByText('New Workspace')).toBeInTheDocument()
    })

    // Open modal
    fireEvent.click(screen.getByRole('button', { name: /New Workspace/i }))

    expect(screen.getByLabelText(/Workspace Name \*/i)).toBeInTheDocument()

    // Fill form
    fireEvent.change(screen.getByLabelText(/Workspace Name \*/i), {
      target: { value: 'Auth Service' },
    })

    // Slug should auto-populate
    expect(screen.getByLabelText(/Slug ID/i)).toHaveValue('auth-service')

    // Check auth required
    fireEvent.click(screen.getByLabelText(/Require SMTP \/ IMAP \/ POP3 authentication/i))

    // Enter password
    fireEvent.change(screen.getByLabelText(/Account Password/i), {
      target: { value: 'secretpass123' },
    })

    // Submit
    fireEvent.click(screen.getByRole('button', { name: /^Create Workspace$/i }))

    await waitFor(() => {
      expect(create).toHaveBeenCalledWith({
        name: 'Auth Service',
        id: 'auth-service',
        description: undefined,
        clone_from: 'default',
        auth_required: true,
        password: 'secretpass123',
        initial_account: undefined,
        set_active: true,
      })
    })
  })

  it('creates a workspace without cloning, providing initial account and bound email addresses', async () => {
    create.mockResolvedValue({
      id: 'billing-app-2',
      name: 'Billing App 2',
      created_at: '2026-01-03T00:00:00Z',
      is_active: true,
      auth_required: false,
      message_count: 0,
      size_bytes: 32768,
      database_path: '/data/workspaces/billing-app-2/sandesh.db',
    })

    renderWithProviders(<WorkspacesRoute />)

    await waitFor(() => {
      expect(screen.getByText('New Workspace')).toBeInTheDocument()
    })

    fireEvent.click(screen.getByRole('button', { name: /New Workspace/i }))

    fireEvent.change(screen.getByLabelText(/Workspace Name \*/i), {
      target: { value: 'Billing App 2' },
    })

    // Uncheck clone accounts
    const cloneCheckbox = screen.getByRole('checkbox', {
      name: /Clone accounts & delivery routing/i,
    })
    fireEvent.click(cloneCheckbox)

    // Initial account fields should now appear
    expect(
      screen.getByText(/Initial Account & Bound Email Addresses/i),
    ).toBeInTheDocument()

    fireEvent.change(screen.getByLabelText(/Account Username/i), {
      target: { value: 'finance' },
    })

    fireEvent.change(screen.getByLabelText(/Bound Email Addresses/i), {
      target: { value: 'finance@example.com, invoices@example.com' },
    })

    fireEvent.click(screen.getByRole('button', { name: /^Create Workspace$/i }))

    await waitFor(() => {
      expect(create).toHaveBeenCalledWith({
        name: 'Billing App 2',
        id: 'billing-app-2',
        description: undefined,
        clone_from: undefined,
        auth_required: false,
        password: undefined,
        initial_account: {
          username: 'finance',
          addresses: ['finance@example.com', 'invoices@example.com'],
          is_catch_all: true,
        },
        set_active: true,
      })
    })
  })

  it('opens purge modal and executes purge on active workspace', async () => {
    purge.mockResolvedValue({
      workspace_id: 'default',
      deleted_messages: 42,
      deleted_traces: 15,
      reclaimed_bytes: 204800,
      purged_at: '2026-01-01T01:00:00Z',
    })

    renderWithProviders(<WorkspacesRoute />)

    await waitFor(() => {
      expect(screen.getByText('Purge Mail & Traces')).toBeInTheDocument()
    })

    fireEvent.click(screen.getByText('Purge Mail & Traces'))

    await waitFor(() => {
      expect(screen.getByText(/Purge Workspace "Default Workspace"/i)).toBeInTheDocument()
    })

    fireEvent.click(screen.getByRole('button', { name: /Execute Purge/i }))

    await waitFor(() => {
      expect(purge).toHaveBeenCalledWith('default', {
        mode: 'mail_and_traces',
        clear_traces: true,
        clear_webhooks: false,
      })
    })
  })

  it('renders workspace type tags and all card action controls properly', async () => {
    renderWithProviders(<WorkspacesRoute />)

    await waitFor(() => {
      expect(screen.getByText('Billing App')).toBeInTheDocument()
    })

    // Check classification badges in card headers
    expect(screen.getByText('Primary Workspace')).toBeInTheDocument()
    expect(screen.getByText('Custom Workspace')).toBeInTheDocument()

    // Delete button exists for custom workspace (billing-app) but not primary
    const deleteButtons = screen.getAllByRole('button', { name: /Delete/i })
    expect(deleteButtons.length).toBe(1) // only 1 delete button for custom workspace

    // Action buttons in the cards
    const connectButtons = screen.getAllByRole('button', { name: /Connect/i })
    expect(connectButtons.length).toBeGreaterThanOrEqual(2) // Hero + Card buttons

    const purgeButtons = screen.getAllByRole('button', { name: /Purge/i })
    expect(purgeButtons.length).toBeGreaterThanOrEqual(2)

    // Database paths are rendered
    expect(screen.getAllByText('/data/sandesh.db').length).toBeGreaterThanOrEqual(1)
    expect(screen.getByText('/data/workspaces/billing-app/sandesh.db')).toBeInTheDocument()
  })

  it('activates the workspace when clicking Accounts on an inactive workspace card', async () => {
    activate.mockResolvedValue({
      ...MOCK_WORKSPACES[1],
      is_active: true,
    })

    renderWithProviders(<WorkspacesRoute />)

    await waitFor(() => {
      expect(screen.getByText('Billing App')).toBeInTheDocument()
    })

    const accountsButtons = screen.getAllByRole('button', { name: /Accounts/i })
    expect(accountsButtons.length).toBe(2)

    // Click the Accounts button on the second workspace (billing-app)
    fireEvent.click(accountsButtons[1]!)

    await waitFor(() => {
      expect(activate).toHaveBeenCalledWith('billing-app')
    })
  })

  it('opens AccountModal with correct styling classes and accessibility attributes when clicking Edit Password', async () => {
    listAccounts.mockResolvedValue({
      items: [
        {
          id: 'acc-catch-all',
          username: 'catch-all',
          addresses: ['catchall@example.com'],
          is_catch_all: true,
          has_password: true,
          created_at: '2026-01-01T00:00:00Z',
        },
      ],
    })

    renderWithProviders(<WorkspacesRoute />)

    await waitFor(() => {
      expect(screen.getByText('Edit Password')).toBeInTheDocument()
    })

    fireEvent.click(screen.getByText('Edit Password'))

    await waitFor(() => {
      expect(screen.getByText('Edit Account: catch-all')).toBeInTheDocument()
    })

    // Verify modal dialog structure and CSS classes
    const dialog = screen.getByRole('dialog')
    expect(dialog).toHaveClass('account-modal')
    expect(dialog.parentElement).toHaveClass('account-modal__backdrop')

    const modalScope = within(dialog)

    // Verify form accessibility attributes (id, name, and associations)
    const usernameInput = modalScope.getByLabelText(/Username/i)
    expect(usernameInput).toHaveAttribute('id', 'acc-username')
    expect(usernameInput).toHaveAttribute('name', 'username')

    const addressesInput = modalScope.getByLabelText(/Bound Email Address/i)
    expect(addressesInput).toHaveAttribute('id', 'acc-addresses')
    expect(addressesInput).toHaveAttribute('name', 'addresses')

    const passwordInput = modalScope.getByLabelText(/New Password/i)
    expect(passwordInput).toHaveAttribute('id', 'acc-password')
    expect(passwordInput).toHaveAttribute('name', 'password')

    const catchAllCheckbox = modalScope.getByRole('checkbox', { name: /Set as Catch-All Account/i })
    expect(catchAllCheckbox).toHaveAttribute('id', 'acc-catch-all')
    expect(catchAllCheckbox).toHaveAttribute('name', 'is_catch_all')
    expect(catchAllCheckbox).toBeChecked()

    // Verify Save Changes button
    expect(modalScope.getByRole('button', { name: /Save Changes/i })).toBeInTheDocument()
  })
})


