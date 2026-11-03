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

  const colorClass =
    state === 'connected'
      ? 'text-secondary'
      : state === 'connecting'
        ? 'text-tertiary'
        : 'text-error'

  const bgDotClass =
    state === 'connected'
      ? 'bg-secondary'
      : state === 'connecting'
        ? 'bg-tertiary'
        : 'bg-error'

  return (
    <span
      className={`connection connection--${state} flex items-center gap-1.5 px-2 py-1 rounded bg-surface-container-low`}
    >
      <span className="relative flex h-2 w-2 shrink-0">
        {state === 'connected' ? (
          <span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-secondary opacity-75" />
        ) : null}
        <span className={`relative inline-flex rounded-full h-2 w-2 ${bgDotClass}`} />
      </span>
      <span className={`connection__label font-label-caps text-label-caps uppercase tracking-wider font-semibold ${colorClass}`}>
        {label}
      </span>
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
