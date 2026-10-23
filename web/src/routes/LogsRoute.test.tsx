/**
 * The log stream view (Phase 7 A6).
 *
 * The assertions that matter are the ones about *where lines come from* and
 * *what the filters do to the request*, because the server does the filtering
 * — a view that filtered locally would look identical on screen while
 * streaming everything over the wire.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { act, fireEvent, screen, waitFor } from '@testing-library/react'

import { renderWithProviders } from '../test/render'
import { LogsRoute } from './LogsRoute'

/** A stand-in for the browser's `EventSource`, which jsdom does not provide. */
class FakeEventSource {
  static instances: FakeEventSource[] = []

  url: string
  closed = false
  onopen: (() => void) | null = null
  onmessage: ((event: MessageEvent<string>) => void) | null = null
  onerror: (() => void) | null = null
  private listeners = new Map<string, Array<(event: Event) => void>>()

  constructor(url: string) {
    this.url = url
    FakeEventSource.instances.push(this)
  }

  addEventListener(type: string, handler: (event: Event) => void) {
    const existing = this.listeners.get(type) ?? []
    existing.push(handler)
    this.listeners.set(type, existing)
  }

  close() {
    this.closed = true
  }

  /**
   * Delivers a `log` event, as the server sends it.
   *
   * Wrapped in `act` because it drives a React state update from outside the
   * event loop React knows about; without it every assertion races the render.
   */
  emit(payload: unknown) {
    const event = new MessageEvent('log', { data: JSON.stringify(payload) })
    act(() => {
      for (const handler of this.listeners.get('log') ?? []) handler(event)
    })
  }

  open() {
    this.onopen?.()
  }

  static latest(): FakeEventSource {
    const last = FakeEventSource.instances.at(-1)
    if (!last) throw new Error('no EventSource was opened')
    return last
  }
}

function line(overrides: Partial<Record<string, unknown>> = {}) {
  return {
    seq: 1,
    at: '2026-01-01T10:00:00.000Z',
    level: 'info',
    target: 'sandesh_smtp::session',
    message: 'message accepted',
    fields: 'placements=1',
    ...overrides,
  }
}

/** An empty page, the shape every list endpoint returns. */
const EMPTY_PAGE = '{"items":[],"next_cursor":null,"total_estimate":0}'

/** Responds to the backlog fetch; everything else gets an empty page. */
function mockFetch(backlog: unknown[] = []) {
  return vi.fn((input: unknown) => {
    const url = String(input)
    const body = url.startsWith('/api/v1/logs')
      ? JSON.stringify({ items: backlog, next_cursor: null, total_estimate: backlog.length })
      : EMPTY_PAGE

    return Promise.resolve(
      new Response(body, { status: 200, headers: { 'content-type': 'application/json' } }),
    )
  })
}

beforeEach(() => {
  FakeEventSource.instances = []
  vi.stubGlobal('EventSource', FakeEventSource)
})

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('the backlog', () => {
  /**
   * A stream alone would leave the pane empty until something next happened,
   * which reads as "logging is broken" rather than "nothing has happened".
   */
  it('is shown immediately, before anything new arrives', async () => {
    vi.stubGlobal('fetch', mockFetch([line({ seq: 1, message: 'from the backlog' })]))

    renderWithProviders(<LogsRoute />)

    await waitFor(() => expect(screen.getByText('from the backlog')).toBeInTheDocument())
  })

  it('reports a failure rather than showing an empty pane', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn((input: unknown) =>
        Promise.resolve(
          String(input).startsWith('/api/v1/logs')
            ? new Response('nope', { status: 500 })
            : new Response(EMPTY_PAGE, {
                status: 200,
                headers: { 'content-type': 'application/json' },
              }),
        ),
      ),
    )

    renderWithProviders(<LogsRoute />)

    await waitFor(() =>
      expect(screen.getByText(/could not be loaded/i)).toBeInTheDocument(),
    )
  })
})

describe('the live stream', () => {
  it('appends a line the server pushes', async () => {
    vi.stubGlobal('fetch', mockFetch())
    renderWithProviders(<LogsRoute />)

    await waitFor(() => expect(FakeEventSource.instances).toHaveLength(1))

    FakeEventSource.latest().emit(line({ seq: 2, message: 'arrived live' }))

    await waitFor(() => expect(screen.getByText('arrived live')).toBeInTheDocument())
  })

  it('renders the level and the structured fields', async () => {
    vi.stubGlobal('fetch', mockFetch())
    renderWithProviders(<LogsRoute />)
    await waitFor(() => expect(FakeEventSource.instances).toHaveLength(1))

    FakeEventSource.latest().emit(line({ seq: 3, level: 'error', message: 'it broke' }))

    await waitFor(() => expect(screen.getByText('it broke')).toBeInTheDocument())
    expect(screen.getByText('error')).toBeInTheDocument()
    expect(screen.getByText(/placements=1/)).toBeInTheDocument()
  })

  /** A malformed frame must not derail the stream. */
  it('survives a frame that will not parse', async () => {
    vi.stubGlobal('fetch', mockFetch())
    renderWithProviders(<LogsRoute />)
    await waitFor(() => expect(FakeEventSource.instances).toHaveLength(1))

    const source = FakeEventSource.latest()
    const broken = new MessageEvent('log', { data: 'not json' })
    expect(() => {
      act(() => {
        for (const handler of (source as never as {
          listeners: Map<string, Array<(event: Event) => void>>
        }).listeners.get('log') ?? []) {
          handler(broken)
        }
      })
    }).not.toThrow()

    source.emit(line({ seq: 4, message: 'still streaming' }))
    await waitFor(() => expect(screen.getByText('still streaming')).toBeInTheDocument())
  })

  it('closes the connection when paused, and reopens on resume', async () => {
    vi.stubGlobal('fetch', mockFetch())
    renderWithProviders(<LogsRoute />)
    await waitFor(() => expect(FakeEventSource.instances).toHaveLength(1))

    const first = FakeEventSource.latest()
    fireEvent.click(screen.getByRole('button', { name: 'Pause' }))

    await waitFor(() => expect(first.closed).toBe(true))

    fireEvent.click(screen.getByRole('button', { name: 'Resume' }))
    await waitFor(() => expect(FakeEventSource.instances).toHaveLength(2))
  })
})

describe('filtering', () => {
  /**
   * The server filters, not the view. Filtering locally would look the same on
   * screen while streaming every line over the wire — which is the bandwidth
   * the filter exists to save.
   */
  it('sends the level to the server, on both the backlog and the stream', async () => {
    const fetchMock = mockFetch()
    vi.stubGlobal('fetch', fetchMock)

    renderWithProviders(<LogsRoute />)
    await waitFor(() => expect(FakeEventSource.instances).toHaveLength(1))

    fireEvent.change(screen.getByLabelText('Level'), { target: { value: 'warn' } })

    await waitFor(() =>
      expect(
        fetchMock.mock.calls.some(([url]) => String(url).includes('level=warn')),
      ).toBe(true),
    )
    await waitFor(() =>
      expect(FakeEventSource.latest().url).toContain('level=warn'),
    )
  })

  it('sends the target filter too', async () => {
    const fetchMock = mockFetch()
    vi.stubGlobal('fetch', fetchMock)

    renderWithProviders(<LogsRoute />)
    await waitFor(() => expect(FakeEventSource.instances).toHaveLength(1))

    fireEvent.change(screen.getByLabelText('Target'), { target: { value: 'sandesh_imap' } })

    await waitFor(() =>
      expect(FakeEventSource.latest().url).toContain('target=sandesh_imap'),
    )
  })

  it('requests no filter when the level is cleared', async () => {
    const fetchMock = mockFetch()
    vi.stubGlobal('fetch', fetchMock)

    renderWithProviders(<LogsRoute />)
    await waitFor(() => expect(FakeEventSource.instances).toHaveLength(1))

    fireEvent.change(screen.getByLabelText('Level'), { target: { value: 'error' } })
    await waitFor(() => expect(FakeEventSource.latest().url).toContain('level=error'))

    fireEvent.change(screen.getByLabelText('Level'), { target: { value: '' } })
    await waitFor(() => expect(FakeEventSource.latest().url).not.toContain('level='))
  })
})

describe('the view', () => {
  it('clears on request without stopping the stream', async () => {
    vi.stubGlobal('fetch', mockFetch([line({ seq: 1, message: 'existing' })]))
    renderWithProviders(<LogsRoute />)

    await waitFor(() => expect(screen.getByText('existing')).toBeInTheDocument())

    fireEvent.click(screen.getByRole('button', { name: 'Clear' }))
    expect(screen.queryByText('existing')).not.toBeInTheDocument()

    FakeEventSource.latest().emit(line({ seq: 2, message: 'after clearing' }))
    await waitFor(() => expect(screen.getByText('after clearing')).toBeInTheDocument())
  })

  it('says so when there is nothing to show', async () => {
    vi.stubGlobal('fetch', mockFetch())
    renderWithProviders(<LogsRoute />)

    await waitFor(() => expect(screen.getByText(/No log lines yet/i)).toBeInTheDocument())
  })
})

describe('states (Phase 7 Pass B)', () => {
  /**
   * A failed backlog used to be a bare sentence with no way forward: the only
   * recovery was a page reload. The shared ErrorState offers a retry, and it
   * has to actually refetch.
   */
  it('offers a retry after a failure, and refetches on it', async () => {
    let attempts = 0
    vi.stubGlobal(
      'fetch',
      vi.fn((input: unknown) => {
        const url = String(input)
        if (url.startsWith('/api/v1/logs')) {
          attempts += 1
          return attempts === 1
            ? Promise.resolve(new Response('nope', { status: 500 }))
            : Promise.resolve(
                new Response(
                  JSON.stringify({
                    items: [line({ seq: 9, message: 'recovered' })],
                    next_cursor: null,
                    total_estimate: 1,
                  }),
                  { status: 200, headers: { 'content-type': 'application/json' } },
                ),
              )
        }
        return Promise.resolve(
          new Response(EMPTY_PAGE, {
            status: 200,
            headers: { 'content-type': 'application/json' },
          }),
        )
      }),
    )

    renderWithProviders(<LogsRoute />)

    const retry = await screen.findByRole('button', { name: /try again/i })
    fireEvent.click(retry)

    await waitFor(() => expect(screen.getByText('recovered')).toBeInTheDocument())
    expect(attempts).toBeGreaterThan(1)
  })

  /**
   * "Nothing logged yet" and "nothing matches this filter" are different
   * answers. Conflating them sends someone looking for a capture bug when
   * they have simply filtered everything out — the same mistake the traces
   * view was corrected for in A2.
   */
  it('distinguishes an empty log from an over-narrow filter', async () => {
    vi.stubGlobal('fetch', mockFetch())
    renderWithProviders(<LogsRoute />)

    await waitFor(() => expect(screen.getByText(/No log lines yet/i)).toBeInTheDocument())

    fireEvent.change(screen.getByLabelText('Level'), { target: { value: 'error' } })

    await waitFor(() =>
      expect(screen.getByText(/No log lines match this filter/i)).toBeInTheDocument(),
    )
    expect(screen.queryByText(/No log lines yet/i)).not.toBeInTheDocument()
  })

  it('announces a failure to assistive technology', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn((input: unknown) =>
        Promise.resolve(
          String(input).startsWith('/api/v1/logs')
            ? new Response('nope', { status: 500 })
            : new Response(EMPTY_PAGE, {
                status: 200,
                headers: { 'content-type': 'application/json' },
              }),
        ),
      ),
    )

    renderWithProviders(<LogsRoute />)

    // role="alert" is what makes a failure reach a screen reader at all; a
    // plain paragraph is silent.
    await waitFor(() => expect(screen.getByRole('alert')).toBeInTheDocument())
  })
})
