import { describe, expect, it, vi, beforeEach } from 'vitest'
import { fireEvent, screen, waitFor } from '@testing-library/react'

import { renderWithProviders } from '../test/render'
import { WorkspaceSwitcher } from './WorkspaceSwitcher'
import type { Workspace } from '../api/client'

const list = vi.fn()
const activate = vi.fn()

vi.mock('../api/resources', () => ({
  workspaces: {
    list: (...args: unknown[]) => list(...args),
    activate: (...args: unknown[]) => activate(...args),
    create: vi.fn(),
    purge: vi.fn(),
    delete: vi.fn(),
  },
  accounts: { list: vi.fn().mockResolvedValue({ items: [] }) },
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
    description: null,
    created_at: '2026-01-01T00:00:00Z',
    is_active: true,
    auth_required: false,
    message_count: 5,
    size_bytes: 40960,
    database_path: '/data/sandesh.db',
  },
  {
    id: 'checkout',
    name: 'Checkout Flow',
    description: null,
    created_at: '2026-01-02T00:00:00Z',
    is_active: false,
    auth_required: false,
    message_count: 2,
    size_bytes: 20480,
    database_path: '/data/workspaces/checkout/sandesh.db',
  },
]

describe('WorkspaceSwitcher', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    list.mockResolvedValue(MOCK_WORKSPACES)
  })

  it('renders active workspace name in header button', async () => {
    renderWithProviders(<WorkspaceSwitcher />)

    await waitFor(() => {
      expect(screen.getByText('Default Workspace')).toBeInTheDocument()
    })
  })

  it('opens dropdown and switches workspace on click', async () => {
    activate.mockResolvedValue({
      ...MOCK_WORKSPACES[1],
      is_active: true,
    })

    renderWithProviders(<WorkspaceSwitcher />)

    await waitFor(() => {
      expect(screen.getByText('Default Workspace')).toBeInTheDocument()
    })

    // Open dropdown
    const trigger = screen.getByRole('button', { name: /Default Workspace/i })
    fireEvent.click(trigger)

    expect(screen.getByRole('menu')).toBeInTheDocument()
    expect(screen.getByText('Checkout Flow')).toBeInTheDocument()
    expect(screen.getByText(/Manage all workspaces/i)).toBeInTheDocument()

    // Click on Checkout Flow to activate
    fireEvent.click(screen.getByText('Checkout Flow'))

    await waitFor(() => {
      expect(activate).toHaveBeenCalledWith('checkout')
    })
  })
})
