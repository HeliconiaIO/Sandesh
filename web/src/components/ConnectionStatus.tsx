import { type ConnectionState } from '../api/events'

/**
 * The event-stream indicator.
 *
 * Never colour alone (§11): each state carries a word as well as a dot, so the
 * meaning survives a colour-blind reader and a greyscale screenshot.
 */
export function ConnectionStatus({ state }: { state: ConnectionState }) {
  const label =
    state === 'connected' ? 'Live' : state === 'connecting' ? 'Connecting' : 'Offline'

  return (
    <span className={`connection connection--${state}`}>
      <span className="connection__dot" aria-hidden="true" />
      <span className="connection__label">{label}</span>
      {/* Announced once per change rather than polled, so a screen reader is
          told when live updates stop without being nagged. */}
      <span className="sr-only" role="status" aria-live="polite">
        {state === 'connected'
          ? 'Event stream connected. New mail appears automatically.'
          : state === 'connecting'
            ? 'Reconnecting to the event stream.'
            : 'Event stream disconnected. Messages may be out of date.'}
      </span>
    </span>
  )
}
