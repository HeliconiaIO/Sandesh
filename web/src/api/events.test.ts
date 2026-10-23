/**
 * The event stream client (§65).
 *
 * Reconnection is the hard part to get right and the easy part to get wrong
 * silently, so it is tested against a fake socket rather than a live server:
 * a tight reconnect loop against a dead backend is a bug that only shows up
 * as CPU load, and no integration test would catch it.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { EventStream, type ResyncNotice } from './events'

/** A controllable stand-in for the browser's WebSocket. */
class FakeSocket {
  static instances: FakeSocket[] = []

  onopen: (() => void) | null = null
  onmessage: ((event: { data: unknown }) => void) | null = null
  onclose: (() => void) | null = null
  onerror: (() => void) | null = null
  readyState = 0
  closed = false
  /** Frames the client sent, so `resume` can be asserted. */
  sent: string[] = []

  constructor(readonly url: string) {
    FakeSocket.instances.push(this)
  }

  send(data: string) {
    this.sent.push(data)
  }

  close() {
    this.closed = true
    this.readyState = 3
  }

  open() {
    this.readyState = 1
    this.onopen?.()
  }

  fail() {
    this.readyState = 3
    this.onclose?.()
  }

  deliver(payload: unknown) {
    this.onmessage?.({ data: JSON.stringify(payload) })
  }
}

const OPEN = 1

beforeEach(() => {
  FakeSocket.instances = []
  vi.stubGlobal('WebSocket', Object.assign(FakeSocket, { OPEN }))
  vi.useFakeTimers()
})

afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

function latest(): FakeSocket {
  const socket = FakeSocket.instances.at(-1)
  if (!socket) throw new Error('no socket was opened')
  return socket
}

describe('EventStream', () => {
  it('reports connecting, then connected', () => {
    const stream = new EventStream('ws://test/events')
    const states: string[] = []
    stream.onStateChange((state) => states.push(state))

    stream.connect()
    latest().open()

    expect(states).toEqual(['disconnected', 'connecting', 'connected'])
  })

  it('delivers decoded events to subscribers', () => {
    const stream = new EventStream('ws://test/events')
    const received: unknown[] = []
    stream.subscribe((frame) => received.push(frame))

    stream.connect()
    latest().open()
    latest().deliver({
      type: 'event',
      seq: 4,
      bus_seq: 91,
      event: { kind: 'MessageReceived', id: 'e1', occurred_at: '', payload: { uid: 3 } },
    })

    expect(received).toHaveLength(1)
    // Both sequence numbers are surfaced: they answer different questions.
    expect(received[0]).toMatchObject({ seq: 4, busSeq: 91 })
  })

  it('delivers workspace events to subscribers', () => {
    const stream = new EventStream('ws://test/events')
    const received: unknown[] = []
    stream.subscribe((frame) => received.push(frame))

    stream.connect()
    latest().open()
    latest().deliver({
      type: 'event',
      seq: 5,
      bus_seq: 92,
      event: {
        kind: 'WorkspaceSwitched',
        id: 'e2',
        occurred_at: '',
        payload: { previous: 'default', current: 'staging' },
      },
    })

    expect(received).toHaveLength(1)
    expect(received[0]).toMatchObject({
      seq: 5,
      busSeq: 92,
      event: { kind: 'WorkspaceSwitched', payload: { previous: 'default', current: 'staging' } },
    })
  })

  it('ignores frame types it does not know', () => {
    // An older UI must survive a newer server introducing a frame type.
    const stream = new EventStream('ws://test/events')
    const received: unknown[] = []
    stream.subscribe((frame) => received.push(frame))

    stream.connect()
    latest().open()
    latest().deliver({ type: 'pong' })
    latest().deliver({ type: 'something_from_the_future' })

    expect(received).toHaveLength(0)
  })

  it('survives a malformed frame without dropping the connection', () => {
    const stream = new EventStream('ws://test/events')
    stream.connect()
    latest().open()

    expect(() => latest().onmessage?.({ data: 'not json{' })).not.toThrow()
    expect(stream.connectionState).toBe('connected')
  })

  it('reports lag so callers know to refetch', () => {
    const stream = new EventStream('ws://test/events')
    const notices: ResyncNotice[] = []
    stream.onLag((notice) => notices.push(notice))

    stream.connect()
    latest().open()
    latest().deliver({ type: 'lagged', dropped: 12 })

    expect(notices).toEqual([{ reason: 'lagged', dropped: 12 }])
  })

  /** An `event` frame at a given bus_seq, so resume tests read as a sequence. */
  const eventAt = (busSeq: number) => ({
    type: 'event',
    seq: busSeq,
    bus_seq: busSeq,
    event: { kind: 'MessageReceived', id: `e${busSeq}`, occurred_at: '', payload: {} },
  })

  /** The parsed frames a socket sent. */
  const sentFrames = (socket: { sent: string[] }) =>
    socket.sent.map((raw) => JSON.parse(raw) as Record<string, unknown>)

  it('does not resume on a first connection', () => {
    const stream = new EventStream('ws://test/events')
    stream.connect()
    latest().open()

    // Nothing has been seen, so there is nothing to resume from: asking with
    // after_seq 0 would replay the whole window into a UI about to fetch
    // current state anyway.
    expect(sentFrames(latest())).toEqual([])
  })

  it('resumes from the last bus_seq it processed after a reconnect', () => {
    const stream = new EventStream('ws://test/events')
    stream.connect()
    latest().open()
    latest().deliver(eventAt(41))
    latest().deliver(eventAt(42))

    latest().fail()
    vi.runOnlyPendingTimers()
    latest().open()

    expect(sentFrames(latest())).toEqual([{ type: 'resume', after_seq: 42 }])
  })

  it('resumes from the highest bus_seq, not the most recent frame', () => {
    const stream = new EventStream('ws://test/events')
    stream.connect()
    latest().open()
    latest().deliver(eventAt(50))
    // Browser arrival order is not a promise (§28); an older frame arriving
    // late must not move the resume point backwards.
    latest().deliver(eventAt(48))

    latest().fail()
    vi.runOnlyPendingTimers()
    latest().open()

    expect(sentFrames(latest())).toEqual([{ type: 'resume', after_seq: 50 }])
  })

  it('treats a resume gap as a reason to refetch', () => {
    const stream = new EventStream('ws://test/events')
    const notices: ResyncNotice[] = []
    stream.onLag((notice) => notices.push(notice))

    stream.connect()
    latest().open()
    latest().deliver({ type: 'resume_gap', after_seq: 7, oldest_seq: 30 })

    // The missed events can never arrive, which is the same situation as
    // `lagged` and takes the same recovery.
    expect(notices).toEqual([{ reason: 'resume_gap', dropped: 0 }])
  })

  it('retries the same position after a gap rather than skipping past it', () => {
    const stream = new EventStream('ws://test/events')
    stream.connect()
    latest().open()
    latest().deliver(eventAt(9))

    latest().fail()
    vi.runOnlyPendingTimers()
    latest().open()
    latest().deliver({ type: 'resume_gap', after_seq: 9, oldest_seq: 40 })

    latest().fail()
    vi.runOnlyPendingTimers()
    latest().open()

    // A gap carries no events, so the mark must not advance: the next attempt
    // asks from the same point and succeeds if the window has caught up.
    expect(sentFrames(latest())).toEqual([{ type: 'resume', after_seq: 9 }])
  })

  it('does not move the resume point backwards when replayed events overlap', () => {
    const stream = new EventStream('ws://test/events')
    stream.connect()
    latest().open()
    latest().deliver(eventAt(20))

    latest().fail()
    vi.runOnlyPendingTimers()
    latest().open()
    // A replay may legitimately overlap what was already seen.
    latest().deliver({ type: 'resumed', after_seq: 20, count: 2 })
    latest().deliver({ ...eventAt(19), replayed: true })
    latest().deliver({ ...eventAt(21), replayed: true })

    latest().fail()
    vi.runOnlyPendingTimers()
    latest().open()

    expect(sentFrames(latest())).toEqual([{ type: 'resume', after_seq: 21 }])
  })

  it('reconnects after an unexpected close', () => {
    const stream = new EventStream('ws://test/events')
    stream.connect()
    latest().open()
    expect(FakeSocket.instances).toHaveLength(1)

    latest().fail()
    // Nothing reconnects instantly; the backoff must elapse first.
    expect(FakeSocket.instances).toHaveLength(1)

    vi.advanceTimersByTime(2000)
    expect(FakeSocket.instances.length).toBeGreaterThan(1)
  })

  it('backs off rather than retrying in a tight loop', () => {
    // The property that matters when the backend is simply gone: a fixed
    // number of failures must not produce an unbounded number of attempts.
    const stream = new EventStream('ws://test/events')
    stream.connect()

    for (let attempt = 0; attempt < 6; attempt += 1) {
      latest().fail()
      vi.advanceTimersByTime(200)
    }

    // With 500ms initial backoff and doubling, 1.2s of virtual time cannot
    // accommodate six reconnects; a tight loop would have made dozens.
    expect(FakeSocket.instances.length).toBeLessThan(6)
  })

  it('does not reconnect after a deliberate disconnect', () => {
    const stream = new EventStream('ws://test/events')
    stream.connect()
    latest().open()

    stream.disconnect()
    const count = FakeSocket.instances.length

    vi.advanceTimersByTime(60_000)

    expect(FakeSocket.instances).toHaveLength(count)
    expect(stream.connectionState).toBe('disconnected')
  })

  it('does not reconnect when the socket closes after a disconnect', () => {
    // The race the `closed` flag exists for: a close event already in flight
    // when `disconnect()` is called must not resurrect the connection. Testing
    // only the happy path would pass even with the guard removed, because
    // `disconnect()` detaches its own handlers before closing.
    const stream = new EventStream('ws://test/events')
    stream.connect()
    const socket = latest()
    socket.open()

    const onclose = socket.onclose
    stream.disconnect()

    // The transport reports the close independently of our teardown.
    onclose?.()
    vi.advanceTimersByTime(60_000)

    expect(FakeSocket.instances).toHaveLength(1)
    expect(stream.connectionState).toBe('disconnected')
  })

  it('stops notifying a listener that unsubscribed', () => {
    const stream = new EventStream('ws://test/events')
    const received: unknown[] = []
    const unsubscribe = stream.subscribe((frame) => received.push(frame))

    stream.connect()
    latest().open()
    unsubscribe()
    latest().deliver({
      type: 'event',
      seq: 1,
      bus_seq: 1,
      event: { kind: 'EntryCreated', id: 'x', occurred_at: '', payload: {} },
    })

    expect(received).toHaveLength(0)
  })
})
