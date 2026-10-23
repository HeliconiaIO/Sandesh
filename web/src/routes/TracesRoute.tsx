/**
 * Protocol traces (Phase 7 A2).
 *
 * The retrieval half of ADR-066. Transcripts have been captured for SMTP, IMAP
 * and POP3 since A2's first half with no way to read them back; this is where
 * they are read.
 *
 * The view's single job: answer "what did that client actually say?". Every
 * decision below follows from that — the transcript is the content, and the
 * record's metadata is the caption.
 */

import { useState } from 'react'

import { EmptyState, ErrorState, LoadingState } from '../components/States'
import { useConnection, useConnections } from '../hooks/queries'
import { ApiError, type Connection } from '../api/client'
import './TracesRoute.css'

/** The protocols a trace can belong to, in the order the servers start. */
const PROTOCOLS = ['smtp', 'imap', 'pop3'] as const

/** How a transcript line is classified, from the marker capture wrote. */
type LineKind = 'client' | 'server' | 'note'

/**
 * Splits a transcript line into its direction and its text.
 *
 * The `C:`/`S:`/`*` markers are written at capture time, so direction is
 * recorded data rather than something inferred here. A line that matches none
 * of them is shown verbatim as a note rather than guessed at.
 */
function classify(line: string): { kind: LineKind; text: string } {
  if (line.startsWith('C: ')) return { kind: 'client', text: line.slice(3) }
  if (line.startsWith('S: ')) return { kind: 'server', text: line.slice(3) }
  if (line.startsWith('* ')) return { kind: 'note', text: line.slice(2).trim() }
  return { kind: 'note', text: line }
}

/** Renders microseconds as the shortest honest unit. */
function duration(us: number | null | undefined): string {
  if (us === null || us === undefined) return 'open'
  if (us < 1000) return `${us}µs`
  if (us < 1_000_000) return `${(us / 1000).toFixed(0)}ms`
  return `${(us / 1_000_000).toFixed(2)}s`
}

/** The clock time a connection opened, to the second. */
function openedAt(iso: string): string {
  const at = new Date(iso)
  return Number.isNaN(at.getTime()) ? iso : at.toLocaleTimeString()
}

export function TracesRoute() {
  const [protocol, setProtocol] = useState<string | undefined>(undefined)
  const [selectedId, setSelectedId] = useState<string | null>(null)

  const list = useConnections(protocol)
  const rows = list.data?.items ?? []

  // The list already carries every transcript, so a selected row renders
  // immediately from it. The single fetch is the fallback for a deep link,
  // where the list may not hold that record any more.
  const fromList = rows.find((row) => row.id === selectedId)
  const fetched = useConnection(fromList ? undefined : (selectedId ?? undefined))
  const selected = fromList ?? fetched.data

  // Tracing off is a configuration state, not a failure: the 503 is the server
  // saying "not recording", which needs different words from "nothing has
  // connected yet". Read from `ApiError.status` rather than by matching the
  // message — an earlier draft did the latter, and it silently fell through to
  // the empty state, telling the user to send mail that would never be
  // recorded.
  const failure = list.error instanceof ApiError ? list.error : null
  const tracingDisabled = failure?.status === 503

  return (
    <div className={`traces${selected ? ' traces--has-selection' : ''}`}>
      <header className="traces__bar">
        <h1 className="traces__title">Protocol traces</h1>
        <div className="traces__filters" role="group" aria-label="Filter by protocol">
          <button
            type="button"
            className="traces__filter"
            aria-pressed={protocol === undefined}
            onClick={() => setProtocol(undefined)}
          >
            All
          </button>
          {PROTOCOLS.map((name) => (
            <button
              key={name}
              type="button"
              className="traces__filter"
              aria-pressed={protocol === name}
              onClick={() => setProtocol(name)}
            >
              {name.toUpperCase()}
            </button>
          ))}
        </div>
      </header>

      {tracingDisabled ? (
        // Deliberately an *empty* state, not an error: tracing being off is a
        // configuration, not a failure, and offering "try again" would invite
        // retrying something that cannot succeed until the server restarts.
        <div className="traces__state">
          <EmptyState title="Tracing is off">
            {/* The server's own words: it names the variable to set, and
                stays right if that guidance ever changes. */}
            {failure?.detail || 'Set SANDESH_TRACING_ENABLED=true and restart.'}
          </EmptyState>
        </div>
      ) : failure ? (
        <div className="traces__state">
          <ErrorState error={failure} onRetry={() => void list.refetch()} />
        </div>
      ) : list.isPending ? (
        <div className="traces__state">
          <LoadingState label="Reading recent connections" />
        </div>
      ) : rows.length === 0 ? (
        <div className="traces__state">
          <EmptyState title="No connections yet">
            Send mail or connect a client, and the exchange appears here.
          </EmptyState>
        </div>
      ) : (
        <div className="traces__split">
          <ol className="traces__list" aria-label="Recent connections">
            {rows.map((row) => (
              <TraceRow
                key={row.id}
                connection={row}
                selected={row.id === selectedId}
                onSelect={() => setSelectedId(row.id)}
              />
            ))}
          </ol>

          {selected ? (
            <TraceDetail
              connection={selected}
              onBack={() => setSelectedId(null)}
            />
          ) : (
            <div className="traces__placeholder">
              <p>Select a connection to read its transcript.</p>
            </div>
          )}
        </div>
      )}
    </div>
  )
}

/** One row in the connection list. */
function TraceRow({
  connection,
  selected,
  onSelect,
}: {
  connection: Connection
  selected: boolean
  onSelect: () => void
}) {
  return (
    <li>
      <button
        type="button"
        className={`trace-row${selected ? ' trace-row--selected' : ''}`}
        aria-current={selected ? 'true' : undefined}
        onClick={onSelect}
      >
        <span className={`trace-row__protocol trace-row__protocol--${connection.protocol}`}>
          {connection.protocol}
        </span>
        <span className="trace-row__peer">{connection.peer ?? 'local'}</span>
        <span className="trace-row__meta">
          {connection.closed_at === null ? (
            <span className="trace-row__live" title="Still open — this transcript is still growing">
              live
            </span>
          ) : null}
          {connection.encrypted ? <span className="trace-row__tls" title="Encrypted">TLS</span> : null}
          <span className="trace-row__time">{openedAt(connection.opened_at)}</span>
        </span>
      </button>
    </li>
  )
}

/** The selected connection: its transcript, with the record as a caption. */
function TraceDetail({
  connection,
  onBack,
}: {
  connection: Connection
  onBack?: () => void
}) {
  return (
    <article className="trace" aria-label="Transcript">
      {onBack && (
        <button
          type="button"
          className="trace__back-btn"
          onClick={onBack}
          aria-label="Back to connections"
        >
          ← Back to traces
        </button>
      )}

      {connection.closed_at === null && (
        <p className="trace__live-note">
          <span className="trace__live-dot" aria-hidden="true" />
          This connection is still open — the transcript below grows as the client speaks.
        </p>
      )}

      <dl className="trace__facts">
        <div>
          <dt>Peer</dt>
          <dd>{connection.peer ?? 'local'}</dd>
        </div>
        <div>
          <dt>Opened</dt>
          <dd>{openedAt(connection.opened_at)}</dd>
        </div>
        <div>
          <dt>Lasted</dt>
          <dd>{duration(connection.duration_us)}</dd>
        </div>
        <div>
          <dt>Ended</dt>
          <dd>{connection.disposition?.replace('_', ' ') ?? 'open'}</dd>
        </div>
        <div>
          <dt>Transport</dt>
          <dd>{connection.encrypted ? 'Encrypted' : 'Cleartext'}</dd>
        </div>
      </dl>

      <ol className="dialogue">
        {connection.transcript.map((line, index) => {
          const { kind, text } = classify(line)
          return (
            // Transcript lines are positional and append-only — the same
            // command may legitimately repeat, and a live transcript only ever
            // grows at the end — so the index is a stable identity.
            <li key={index} className={`dialogue__line dialogue__line--${kind}`}>
              <span className="dialogue__marker" aria-hidden="true">
                {kind === 'client' ? '→' : kind === 'server' ? '←' : '·'}
              </span>
              <span className="dialogue__text">{text}</span>
            </li>
          )
        })}
      </ol>

      {connection.truncated ? (
        <p className="trace__truncated">
          Transcript reached its cap and was cut short. The connection continued past this point.
        </p>
      ) : null}
    </article>
  )
}
