/**
 * The traces view (Phase 7 A2).
 *
 * The assertions that matter are the states a developer meets when something
 * is wrong, not the happy path: "tracing is off" and "nothing has connected
 * yet" are different answers, and conflating them sends someone hunting for a
 * capture bug that does not exist. An earlier draft did exactly that.
 */

import { describe, expect, it, vi } from 'vitest'
import { fireEvent, screen, waitFor } from '@testing-library/react'

import { ApiError } from '../api/client'
import { renderWithProviders } from '../test/render'
import { TracesRoute } from './TracesRoute'

const SMTP_TRACE = {
  id: '019a0000-0000-7000-8000-00000000aa01',
  protocol: 'smtp',
  peer: '127.0.0.1:51000',
  encrypted: false,
  opened_at: '2026-01-01T10:00:00Z',
  closed_at: '2026-01-01T10:00:01Z',
  disposition: 'closed',
  duration_us: 1_000_000,
  transcript: [
    'S: 220 sandesh ESMTP ready',
    'C: EHLO localhost',
    'S: 250 OK',
    '*  upgraded to TLS',
  ],
  truncated: false,
}

const IMAP_TRACE = {
  ...SMTP_TRACE,
  id: '019a0000-0000-7000-8000-00000000bb02',
  protocol: 'imap',
  peer: '127.0.0.1:51001',
  transcript: ['S: * OK sandesh ready', 'C: a1 LOGOUT'],
}

const list = vi.fn()
const get = vi.fn()

vi.mock('../api/resources', () => ({
  connections: {
    list: (...args: unknown[]) => list(...args),
    get: (...args: unknown[]) => get(...args),
  },
  accounts: { list: vi.fn().mockResolvedValue({ items: [] }) },
  mailboxes: { list: vi.fn().mockResolvedValue({ items: [] }) },
  messages: {},
  entries: {},
  health: {},
  search: {},
}))

describe('TracesRoute', () => {
  it('tells the developer when tracing is off, rather than showing an empty list', async () => {
    // The distinction this whole view depends on: a 503 means "not recording",
    // and saying "no connections yet" instead would send someone looking for a
    // capture bug and waiting for a trace that will never arrive.
    list.mockRejectedValue(
      new ApiError(
        503,
        'unavailable',
        'Service unavailable',
        'connection tracing is disabled; set SANDESH_TRACING_ENABLED=true to record transcripts',
      ),
    )

    renderWithProviders(<TracesRoute />)

    expect(await screen.findByText('Tracing is off')).toBeInTheDocument()
    // The server's own guidance is shown, not a hardcoded paraphrase.
    expect(screen.getByText(/SANDESH_TRACING_ENABLED=true/)).toBeInTheDocument()
    expect(screen.queryByText('No connections yet')).not.toBeInTheDocument()
  })

  it('invites action when tracing is on and nothing has connected', async () => {
    list.mockResolvedValue({ items: [] })

    renderWithProviders(<TracesRoute />)

    expect(await screen.findByText('No connections yet')).toBeInTheDocument()
    expect(screen.queryByText('Tracing is off')).not.toBeInTheDocument()
  })

  it('renders a transcript with its direction, and marks the client lines', async () => {
    list.mockResolvedValue({ items: [SMTP_TRACE] })

    renderWithProviders(<TracesRoute />)

    fireEvent.click(await screen.findByText('127.0.0.1:51000'))

    // Markers are stripped: the direction is carried by the gutter, so leaving
    // `C: ` in the text would say it twice.
    const line = await screen.findByText('EHLO localhost')
    expect(line).toBeInTheDocument()
    expect(screen.queryByText('C: EHLO localhost')).not.toBeInTheDocument()

    // Client and server lines are distinguishable, which is the view's point.
    expect(line.closest('.dialogue__line')).toHaveClass('dialogue__line--client')
    expect(screen.getByText('250 OK').closest('.dialogue__line')).toHaveClass(
      'dialogue__line--server',
    )
    // A server note is not protocol traffic and is set apart from the exchange.
    expect(screen.getByText('upgraded to TLS').closest('.dialogue__line')).toHaveClass(
      'dialogue__line--note',
    )
  })

  it('filters by protocol through the API rather than in the browser', async () => {
    // Filtering client-side would silently drop rows beyond the page limit and
    // show a subset the user could not explain.
    list.mockResolvedValue({ items: [SMTP_TRACE, IMAP_TRACE] })

    renderWithProviders(<TracesRoute />)

    await screen.findByText('127.0.0.1:51000')
    fireEvent.click(screen.getByRole('button', { name: 'IMAP' }))

    await waitFor(() => {
      expect(list).toHaveBeenCalledWith({ protocol: 'imap' })
    })
  })

  it('reports a truncated transcript so a cut-off trace is not read as a quiet one', async () => {
    list.mockResolvedValue({ items: [{ ...SMTP_TRACE, truncated: true }] })

    renderWithProviders(<TracesRoute />)
    fireEvent.click(await screen.findByText('127.0.0.1:51000'))

    expect(await screen.findByText(/reached its cap/)).toBeInTheDocument()
  })
})

describe('an open connection (Phase 7 Pass B)', () => {
  /** A still-open socket: no close time, no disposition, no duration. */
  const OPEN_TRACE = {
    ...SMTP_TRACE,
    id: '019a0000-0000-7000-8000-0000000000ff',
    closed_at: null,
    disposition: null,
    duration_us: null,
    transcript: ['S: 220 sandesh ESMTP ready', 'C: EHLO localhost'],
  }

  /**
   * The transcript of an open socket now grows while the client speaks, so the
   * view has to say which rows are still moving. Without it a half-finished
   * conversation reads as a broken one.
   */
  it('marks a still-open connection as live', async () => {
    list.mockResolvedValue({ items: [OPEN_TRACE], next_cursor: null, total_estimate: 1 })
    renderWithProviders(<TracesRoute />)

    await waitFor(() => expect(screen.getByText('live')).toBeInTheDocument())
  })

  it('does not mark a finished connection as live', async () => {
    list.mockResolvedValue({ items: [SMTP_TRACE], next_cursor: null, total_estimate: 1 })
    renderWithProviders(<TracesRoute />)

    await waitFor(() => expect(screen.getByLabelText('Recent connections')).toBeInTheDocument())
    expect(screen.queryByText('live')).not.toBeInTheDocument()
  })

  it('explains in the detail pane that the transcript is still growing', async () => {
    list.mockResolvedValue({ items: [OPEN_TRACE], next_cursor: null, total_estimate: 1 })
    renderWithProviders(<TracesRoute />)

    await waitFor(() => expect(screen.getByText('live')).toBeInTheDocument())
    fireEvent.click(screen.getByText('live'))

    await waitFor(() =>
      expect(screen.getByText(/still open/i)).toBeInTheDocument(),
    )
  })

  /** An open connection has no duration or disposition to report yet, and
      inventing one would be worse than saying so. */
  it('reports open rather than a fabricated duration', async () => {
    list.mockResolvedValue({ items: [OPEN_TRACE], next_cursor: null, total_estimate: 1 })
    renderWithProviders(<TracesRoute />)

    await waitFor(() => expect(screen.getByText('live')).toBeInTheDocument())
    fireEvent.click(screen.getByText('live'))

    await waitFor(() => expect(screen.getAllByText('open').length).toBeGreaterThan(0))
  })
})

describe('states (Phase 7 Pass B)', () => {
  /**
   * Tracing being off is a *configuration*, not a failure. Offering "try
   * again" would invite retrying something that cannot succeed until the
   * server restarts — so that case must not render as an error.
   */
  it('treats tracing-off as an empty state, with no retry offered', async () => {
    list.mockRejectedValue(
      new ApiError(
        503,
        'unavailable',
        'Service unavailable',
        'connection tracing is disabled; set SANDESH_TRACING_ENABLED=true to record transcripts',
      ),
    )
    renderWithProviders(<TracesRoute />)

    await waitFor(() => expect(screen.getByText('Tracing is off')).toBeInTheDocument())
    expect(screen.queryByRole('button', { name: /try again/i })).not.toBeInTheDocument()
  })

  /** A genuine failure is different: it may well succeed on a retry. */
  it('offers a retry on a real failure, and refetches on it', async () => {
    list.mockRejectedValueOnce(
      new ApiError(500, 'internal', 'Server error', 'the database was unreachable'),
    )
    list.mockResolvedValue({ items: [SMTP_TRACE], next_cursor: null, total_estimate: 1 })

    renderWithProviders(<TracesRoute />)

    const retry = await screen.findByRole('button', { name: /try again/i })
    fireEvent.click(retry)

    await waitFor(() =>
      expect(screen.getByLabelText('Recent connections')).toBeInTheDocument(),
    )
  })

  it('announces a real failure to assistive technology', async () => {
    list.mockRejectedValue(
      new ApiError(500, 'internal', 'Server error', 'the database was unreachable'),
    )
    renderWithProviders(<TracesRoute />)

    await waitFor(() => expect(screen.getByRole('alert')).toBeInTheDocument())
  })

  it('tells the developer that nothing has connected yet', async () => {
    list.mockResolvedValue({ items: [], next_cursor: null, total_estimate: 0 })
    renderWithProviders(<TracesRoute />)

    await waitFor(() => expect(screen.getByText('No connections yet')).toBeInTheDocument())
  })
})
