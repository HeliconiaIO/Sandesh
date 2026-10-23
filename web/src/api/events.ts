/**
 * The event stream client (`API_DESIGN.md` §5, `EVENT_MODEL.md`).
 *
 * One class owns the socket for the whole application. Components never
 * construct a `WebSocket` (§25): reconnection, backoff and frame decoding are
 * transport concerns, and scattering them through the view layer means every
 * component grows its own subtly different retry policy.
 *
 * ```text
 * EventBus (Rust) ──▶ /api/v1/events ──▶ EventStream ──▶ subscribers
 * ```
 */

/**
 * The wire kinds the backend actually emits.
 *
 * These are `EventKind::as_str` verbatim — PascalCase, not snake_case, and
 * asserted stable by a test in `sandesh-core`. They are deliberately spelled
 * out rather than derived, because the REST DTOs are snake_case and assuming
 * the event stream matched them silently broke every live update: the switch
 * fell through, no query was invalidated, and new mail only appeared on a
 * manual refresh.
 */
export type EventKind =
  | 'MessageReceived'
  | 'MessageDeleted'
  | 'EntryCreated'
  | 'EntryDeleted'
  | 'EntryFlagsChanged'
  | 'EntryMoved'
  | 'MailboxCreated'
  | 'MailboxDeleted'
  | 'MailboxRenamed'
  | 'MailboxSubscriptionChanged'
  | 'AccountCreated'
  | 'AccountDeleted'
  | 'DeliveryFailed'
  | 'WorkspaceCreated'
  | 'WorkspaceDeleted'
  | 'WorkspaceSwitched'
  | 'WorkspacePurged'

/** One domain event, as it arrives on the wire. */
export interface DomainEvent {
  kind: EventKind
  id: string
  occurred_at: string
  payload: Record<string, unknown>
}

/**
 * A received frame.
 *
 * `seq` counts frames on THIS connection; `bus_seq` is the bus's global order
 * (ADR-021). They answer different questions, so both are surfaced rather than
 * conflated: a gap in `seq` means this connection missed something, while
 * `bus_seq` is what correlates events across connections. Neither is a promise
 * that browser arrival order matches server order (§28).
 */
export interface EventFrame {
  seq: number
  busSeq: number
  event: DomainEvent
}

export type ConnectionState = 'connecting' | 'connected' | 'disconnected'

type EventListener = (frame: EventFrame) => void
type StateListener = (state: ConnectionState) => void
/**
 * Why a client must resynchronise.
 *
 * `lagged` — the server dropped events for this client alone, and `dropped`
 * says how many. `resume_gap` — events missed while disconnected have already
 * left the replay window, so they can never arrive; `dropped` is unknown and
 * reported as 0. Both take the same recovery, which is why they share a
 * listener, but a consumer that wants to distinguish them can.
 */
export interface ResyncNotice {
  reason: 'lagged' | 'resume_gap'
  dropped: number
}

type LagListener = (notice: ResyncNotice) => void

/** Backoff bounds. Bounded so a dead backend is retried calmly, not in a loop. */
const BACKOFF_INITIAL_MS = 500
const BACKOFF_MAX_MS = 15_000
const BACKOFF_FACTOR = 2

/** Full jitter, so many tabs reconnecting do not synchronise into a thundering herd. */
function jitter(delay: number): number {
  return Math.random() * delay
}

export class EventStream {
  private socket: WebSocket | null = null
  private readonly url: string
  private state: ConnectionState = 'disconnected'
  private backoff = BACKOFF_INITIAL_MS
  private timer: ReturnType<typeof setTimeout> | null = null
  /** Set when `disconnect()` was called, so a deliberate close never retries. */
  private closed = false
  /**
   * The highest `bus_seq` this client has processed, or 0 if none yet.
   *
   * `bus_seq` rather than `seq` because the per-connection counter restarts at
   * 1 on every socket, so it cannot name a position across the reconnect that
   * resume exists for (ADR-100).
   *
   * Deliberately NOT reset by `disconnect()`: a tab that is hidden and later
   * shown closes and reopens the stream, and that is precisely the case where
   * resuming beats refetching.
   */
  private lastBusSeq = 0

  private readonly events = new Set<EventListener>()
  private readonly states = new Set<StateListener>()
  private readonly lags = new Set<LagListener>()

  constructor(url?: string) {
    // Derived from the page origin so the same build works behind any host or
    // port, and over TLS without a second configuration knob.
    if (url) {
      this.url = url
    } else if (typeof window === 'undefined') {
      this.url = 'ws://localhost/api/v1/events'
    } else {
      const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
      this.url = `${protocol}//${window.location.host}/api/v1/events`
    }
  }

  /** The current connection state. */
  get connectionState(): ConnectionState {
    return this.state
  }

  connect(): void {
    this.closed = false
    if (this.socket && this.socket.readyState <= WebSocket.OPEN) {
      return
    }
    this.open()
  }

  disconnect(): void {
    this.closed = true
    this.clearTimer()
    this.backoff = BACKOFF_INITIAL_MS

    const socket = this.socket
    this.socket = null
    if (socket) {
      // Drop handlers first: closing fires `onclose`, which would otherwise
      // schedule a reconnect for a stream we were asked to stop.
      socket.onopen = null
      socket.onmessage = null
      socket.onerror = null
      socket.onclose = null
      socket.close()
    }
    this.setState('disconnected')
  }

  /** Subscribes to events. Returns an unsubscribe function. */
  subscribe(listener: EventListener): () => void {
    this.events.add(listener)
    return () => this.events.delete(listener)
  }

  /** Subscribes to connection-state changes. Fires immediately with the current state. */
  onStateChange(listener: StateListener): () => void {
    this.states.add(listener)
    listener(this.state)
    return () => this.states.delete(listener)
  }

  /**
   * Subscribes to resynchronisation notices, which mean this client must
   * refetch: either it lagged, or it missed events that are no longer
   * replayable. See [`ResyncNotice`].
   */
  onLag(listener: LagListener): () => void {
    this.lags.add(listener)
    return () => this.lags.delete(listener)
  }

  private open(): void {
    this.setState('connecting')

    let socket: WebSocket
    try {
      socket = new WebSocket(this.url)
    } catch {
      // A malformed URL or a blocked scheme: treat it as a failed attempt so
      // the backoff still applies rather than throwing into the caller.
      this.scheduleReconnect()
      return
    }

    this.socket = socket

    socket.onopen = () => {
      this.backoff = BACKOFF_INITIAL_MS
      this.setState('connected')

      // Ask for what was missed while disconnected (ADR-100). Only after the
      // first event: `after_seq: 0` on a fresh client would replay the whole
      // retained window into a UI that is about to fetch current state anyway.
      if (this.lastBusSeq > 0) {
        try {
          socket.send(JSON.stringify({ type: 'resume', after_seq: this.lastBusSeq }))
        } catch {
          // The socket can close between `onopen` and this send. The server
          // then never replays, the next `onclose` reconnects, and the retry
          // carries the same `after_seq` — so nothing is lost by ignoring it.
        }
      }
    }

    socket.onmessage = (message) => this.handleFrame(message.data)

    socket.onclose = () => {
      this.socket = null
      this.setState('disconnected')
      this.scheduleReconnect()
    }

    // `onerror` is always followed by `onclose`, which does the reconnecting.
    socket.onerror = () => {}
  }

  private handleFrame(data: unknown): void {
    if (typeof data !== 'string') {
      return
    }

    let frame: Record<string, unknown>
    try {
      frame = JSON.parse(data) as Record<string, unknown>
    } catch {
      // A frame we cannot parse is not worth killing the connection over.
      return
    }

    switch (frame['type']) {
      case 'event': {
        const event = frame['event'] as DomainEvent | undefined
        if (!event?.kind) {
          return
        }
        const decoded: EventFrame = {
          seq: Number(frame['seq'] ?? 0),
          busSeq: Number(frame['bus_seq'] ?? 0),
          event,
        }
        // Advance with `max`, never by assignment: replayed frames and live
        // ones share this path, and a replay that overlaps what was already
        // seen must not move the mark backwards.
        this.lastBusSeq = Math.max(this.lastBusSeq, decoded.busSeq)
        for (const listener of this.events) {
          listener(decoded)
        }
        return
      }
      case 'lagged': {
        // The server dropped events for this client alone. Anything cached is
        // now suspect, so listeners refetch rather than patch.
        const dropped = Number(frame['dropped'] ?? 0)
        for (const listener of this.lags) {
          listener({ reason: 'lagged', dropped })
        }
        return
      }
      case 'resumed': {
        // A header stating what is about to be replayed. The events follow as
        // ordinary `event` frames, so there is nothing to do but note it —
        // `count: 0` means this client was already current.
        return
      }
      case 'resume_gap': {
        // The missed events are no longer retained, so they can never arrive.
        // This is the same situation as `lagged` and takes the same recovery:
        // refetch. Routing it through the lag listeners means consumers need no
        // second code path for a state they already handle.
        //
        // The mark is NOT advanced here. The server sent no events, so the next
        // reconnect asks from the same point; if the window has meanwhile
        // caught up, that attempt succeeds rather than being skipped.
        for (const listener of this.lags) {
          listener({ reason: 'resume_gap', dropped: 0 })
        }
        return
      }
      default:
        // `pong` and any frame a newer server introduces. Ignoring unknown
        // types is what lets an older UI talk to a newer backend.
        return
    }
  }

  private scheduleReconnect(): void {
    if (this.closed || this.timer) {
      return
    }

    const delay = jitter(this.backoff)
    this.backoff = Math.min(this.backoff * BACKOFF_FACTOR, BACKOFF_MAX_MS)

    this.timer = setTimeout(() => {
      this.timer = null
      if (!this.closed) {
        this.open()
      }
    }, delay)
  }

  private clearTimer(): void {
    if (this.timer) {
      clearTimeout(this.timer)
      this.timer = null
    }
  }

  private setState(state: ConnectionState): void {
    if (this.state === state) {
      return
    }
    this.state = state
    for (const listener of this.states) {
      listener(state)
    }
  }
}

/** The application's stream. One socket per tab. */
export const eventStream = new EventStream()
