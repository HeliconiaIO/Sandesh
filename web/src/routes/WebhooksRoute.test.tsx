/**
 * The webhooks view (Phase 8 A2).
 *
 * The assertions that matter are about the questions a developer brings to
 * this screen: did it fire, and if not, why. A view that rendered every entry
 * identically would look fine and answer neither — so the tests pin the status
 * wording, the failure reason surviving on screen, and the empty state telling
 * someone what to do next rather than just reporting emptiness.
 */

import { describe, expect, it, vi, beforeEach } from 'vitest'
import { fireEvent, screen, waitFor } from '@testing-library/react'

import { renderWithProviders } from '../test/render'
import { WebhooksRoute } from './WebhooksRoute'

const list = vi.fn()
const create = vi.fn()
const remove = vi.fn()
const outbox = vi.fn()

vi.mock('../api/resources', () => ({
  webhooks: {
    list: (...args: unknown[]) => list(...args),
    create: (...args: unknown[]) => create(...args),
    remove: (...args: unknown[]) => remove(...args),
    outbox: (...args: unknown[]) => outbox(...args),
  },
  accounts: { list: vi.fn().mockResolvedValue({ items: [] }) },
  mailboxes: { list: vi.fn().mockResolvedValue({ items: [] }) },
  messages: {},
  entries: {},
  health: {},
  search: {},
  connections: {},
  faults: {},
}))

const ENDPOINT = {
  id: '019a0000-0000-7000-8000-0000000000e1',
  url: 'http://localhost:3000/hooks/mail',
  enabled: true,
  created_at: '2026-01-01T10:00:00Z',
}

function entry(overrides: Record<string, unknown> = {}) {
  return {
    id: '019a0000-0000-7000-8000-0000000000b1',
    endpoint_id: ENDPOINT.id,
    payload: JSON.stringify({ event: 'MessageReceived', uid: 7 }),
    attempts: 1,
    status: 'delivered',
    next_attempt_at: '2026-01-01T10:00:01Z',
    created_at: '2026-01-01T10:00:01Z',
    ...overrides,
  }
}

describe('WebhooksRoute', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    list.mockResolvedValue([])
    outbox.mockResolvedValue([])
  })

  it('tells someone with no endpoints what to do next', async () => {
    // An empty screen is an invitation to act. "No endpoints" alone would
    // report a fact and leave the next step unstated.
    renderWithProviders(<WebhooksRoute />)

    expect(await screen.findByText('No endpoints yet')).toBeInTheDocument()
    expect(
      screen.getByText(/Add a URL above and deliver a message/),
    ).toBeInTheDocument()
  })

  it('explains a failed notification rather than hiding it', async () => {
    // The backend deliberately keeps a terminally-failed entry (ADR-086); if
    // the view dropped it, that decision would be undone here.
    list.mockResolvedValue([ENDPOINT])
    outbox.mockResolvedValue([
      entry({ status: 'failed', attempts: 5, last_error: 'HTTP 500' }),
    ])

    renderWithProviders(<WebhooksRoute />)

    expect(await screen.findByText('Gave up')).toBeInTheDocument()
    expect(screen.getByText('HTTP 500')).toBeInTheDocument()
    expect(screen.getByText('5 attempts')).toBeInTheDocument()
  })

  it('distinguishes queued from delivered', async () => {
    // Three states that must not read alike: the whole question is which one
    // a given notification is in.
    list.mockResolvedValue([ENDPOINT])
    outbox.mockResolvedValue([
      entry({ id: 'a', status: 'delivered' }),
      entry({ id: 'b', status: 'pending', attempts: 0 }),
    ])

    renderWithProviders(<WebhooksRoute />)

    expect(await screen.findByText('Delivered')).toBeInTheDocument()
    expect(screen.getByText('Queued')).toBeInTheDocument()
  })

  it('registers an endpoint through the API', async () => {
    create.mockResolvedValue(ENDPOINT)
    renderWithProviders(<WebhooksRoute />)

    const input = await screen.findByLabelText('Notification URL')
    fireEvent.change(input, { target: { value: 'http://localhost:3000/hooks/mail' } })
    fireEvent.click(screen.getByRole('button', { name: 'Add endpoint' }))

    await waitFor(() => {
      expect(create).toHaveBeenCalledWith({
        url: 'http://localhost:3000/hooks/mail',
        enabled: true,
      })
    })
  })

  it('will not submit an empty URL', async () => {
    renderWithProviders(<WebhooksRoute />)

    const button = await screen.findByRole('button', { name: 'Add endpoint' })
    expect(button).toBeDisabled()
    expect(create).not.toHaveBeenCalled()
  })

  it('removes an endpoint', async () => {
    list.mockResolvedValue([ENDPOINT])
    remove.mockResolvedValue(undefined)

    renderWithProviders(<WebhooksRoute />)

    fireEvent.click(
      await screen.findByRole('button', { name: `Remove ${ENDPOINT.url}` }),
    )

    const confirmBtn = await screen.findByRole('button', { name: 'Remove' })
    fireEvent.click(confirmBtn)

    await waitFor(() => {
      expect(remove).toHaveBeenCalledWith(ENDPOINT.id)
    })
  })

  it('shows the payload on demand rather than by default', async () => {
    // The list answers "did it fire"; the body is detail. Rendering every
    // payload inline would bury the status the view exists to show.
    list.mockResolvedValue([ENDPOINT])
    outbox.mockResolvedValue([entry()])

    renderWithProviders(<WebhooksRoute />)

    const row = await screen.findByRole('button', { expanded: false })
    expect(screen.queryByText(/MessageReceived/)).not.toBeInTheDocument()

    fireEvent.click(row)
    expect(await screen.findByText(/MessageReceived/)).toBeInTheDocument()
  })
})
