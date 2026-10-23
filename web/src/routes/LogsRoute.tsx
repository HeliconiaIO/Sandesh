/**
 * The log stream (Phase 7 A6).
 *
 * The last Pass A increment. Its job: show what the server is logging, right
 * now, without a terminal — and let a developer narrow it when the interesting
 * line is buried.
 *
 * ## Two sources, deliberately
 *
 * The backlog is fetched once so the pane has content the moment it opens; the
 * live stream appends from then on. A stream alone would show an empty pane
 * until something next happened, which reads as "logging is broken" rather
 * than "nothing has happened yet".
 *
 * ## What is not here
 *
 * No message content, and no credentials. Not because this view filters them
 * out, but because they never reach a log line in the first place
 * (`sandesh-app/src/logging.rs`) — a cross-protocol test delivers a message
 * with distinctive strings and fails if any of them reaches this endpoint.
 * Filtering here would put the guarantee on the last reader rather than at
 * the source.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'

import { EmptyState, ErrorState } from '../components/States'
import { logs as logsApi } from '../api/resources'
import type { LogLine } from '../api/client'
import './LogsRoute.css'

/** Severities, ordered, as the filter presents them. */
const LEVELS = ['trace', 'debug', 'info', 'warn', 'error'] as const

/** How many lines the view keeps before dropping the oldest. */
const MAX_LINES = 1000

/** The clock time of a line, to the second. */
function clockTime(at: string): string {
  const when = new Date(at)
  return Number.isNaN(when.getTime()) ? at : when.toLocaleTimeString()
}

export function LogsRoute() {
  const [lines, setLines] = useState<LogLine[]>([])
  const [level, setLevel] = useState('')
  const [target, setTarget] = useState('')
  const [live, setLive] = useState(true)
  const [connected, setConnected] = useState(false)
  const [loadError, setLoadError] = useState<string | null>(null)
  // Bumped to refetch the backlog after a failure, so a transient error does
  // not require a page reload to clear.
  const [attempt, setAttempt] = useState(0)

  // Whether to keep pinned to the newest line. Turned off when the reader
  // scrolls up: yanking someone back to the bottom while they are reading
  // something is the worst behaviour a log view can have.
  const [follow, setFollow] = useState(true)
  const viewport = useRef<HTMLDivElement>(null)

  /** Appends a line, holding the view to its cap. */
  const append = useCallback((line: LogLine) => {
    setLines((current) => {
      const next = current.length >= MAX_LINES ? current.slice(1) : current.slice()
      next.push(line)
      return next
    })
  }, [])

  // The backlog. Refetched when a filter changes so the pane always shows the
  // filter's history, not just its future.
  useEffect(() => {
    let cancelled = false
    const params: { level?: string; target?: string; limit: number } = { limit: 500 }
    if (level) params.level = level
    if (target.trim()) params.target = target.trim()

    logsApi
      .list(params)
      .then((page) => {
        if (!cancelled) {
          setLines(page.items)
          setLoadError(null)
        }
      })
      .catch((error: unknown) => {
        if (!cancelled) setLoadError(error instanceof Error ? error.message : String(error))
      })

    return () => {
      cancelled = true
    }
  }, [level, target, attempt])

  // The live tail. Reopened when a filter changes, because the server does the
  // filtering — sending everything and discarding it here would waste the
  // bandwidth the filter exists to save.
  useEffect(() => {
    if (!live) {
      setConnected(false)
      return undefined
    }

    const params: { level?: string; target?: string } = {}
    if (level) params.level = level
    if (target.trim()) params.target = target.trim()

    const source = new EventSource(logsApi.streamUrl(params))

    source.onopen = () => setConnected(true)
    source.onmessage = (event: MessageEvent<string>) => {
      try {
        append(JSON.parse(event.data) as LogLine)
      } catch {
        // A frame that will not parse is dropped rather than shown as a
        // broken row: one malformed line must not derail the stream.
      }
    }
    source.addEventListener('log', (event) => {
      try {
        append(JSON.parse((event as MessageEvent<string>).data) as LogLine)
      } catch {
        // As above.
      }
    })
    source.onerror = () => setConnected(false)

    return () => {
      source.close()
      setConnected(false)
    }
  }, [live, level, target, append])

  // Keep the newest line visible while following.
  useEffect(() => {
    if (!follow) return
    const element = viewport.current
    if (element) element.scrollTop = element.scrollHeight
  }, [lines, follow])

  /** Follow only while the reader is at the bottom. */
  function onScroll() {
    const element = viewport.current
    if (!element) return
    const atBottom = element.scrollHeight - element.scrollTop - element.clientHeight < 40
    setFollow(atBottom)
  }

  const counts = useMemo(() => {
    const tally: Record<string, number> = {}
    for (const line of lines) {
      tally[line.level] = (tally[line.level] ?? 0) + 1
    }
    return tally
  }, [lines])

  return (
    <div className="logs">
      <div className="logs__toolbar">
        <label className="logs__control">
          <span className="logs__control-label">Level</span>
          <select
            className="logs__select"
            onChange={(event) => setLevel(event.target.value)}
            value={level}
          >
            <option value="">All</option>
            {LEVELS.map((name) => (
              <option key={name} value={name}>
                {name} and above
              </option>
            ))}
          </select>
        </label>

        <label className="logs__control">
          <span className="logs__control-label">Target</span>
          <input
            className="logs__input"
            onChange={(event) => setTarget(event.target.value)}
            placeholder="e.g. sandesh_smtp"
            type="search"
            value={target}
          />
        </label>

        <button
          aria-pressed={live}
          className={`button${live ? ' button--primary' : ''}`}
          onClick={() => setLive((current) => !current)}
          type="button"
        >
          {live ? 'Pause' : 'Resume'}
        </button>

        <button className="button" onClick={() => setLines([])} type="button">
          Clear
        </button>

        <span className="logs__status">
          <span
            className={`logs__dot${connected ? ' logs__dot--live' : ''}`}
            aria-hidden="true"
          />
          {live ? (connected ? 'Streaming' : 'Connecting…') : 'Paused'}
        </span>

        <span className="logs__count">
          {lines.length} line{lines.length === 1 ? '' : 's'}
          {counts.error ? ` · ${counts.error} error${counts.error === 1 ? '' : 's'}` : ''}
        </span>
      </div>

      {loadError && (
        <div className="logs__state">
          <ErrorState
            error={new Error(`The log could not be loaded: ${loadError}`)}
            onRetry={() => setAttempt((current) => current + 1)}
          />
        </div>
      )}

      <div className="logs__viewport" onScroll={onScroll} ref={viewport}>
        {lines.length === 0 ? (
          <div className="logs__state">
            {loadError ? null : level || target.trim() ? (
              <EmptyState title="No log lines match this filter">
                The server may simply not have logged anything at{' '}
                {level ? <code>{level}</code> : 'that level'}
                {target.trim() ? (
                  <>
                    {' '}
                    from <code>{target.trim()}</code>
                  </>
                ) : null}
                . Widen the filter to see more.
              </EmptyState>
            ) : (
              <EmptyState title="No log lines yet">
                Activity appears here as the server works — deliver a message or connect a client.
              </EmptyState>
            )}
          </div>
        ) : (
          <table className="logs__table">
            <tbody>
              {lines.map((line) => (
                <tr className="logs__row" key={line.seq}>
                  <td className="logs__time">{clockTime(line.at)}</td>
                  <td className="logs__level">
                    <span className={`logs__badge logs__badge--${line.level}`}>
                      {line.level}
                    </span>
                  </td>
                  <td className="logs__target" title={line.target}>
                    {line.target}
                  </td>
                  <td className="logs__message">
                    {line.message}
                    {line.fields && <span className="logs__fields"> {line.fields}</span>}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>

      {!follow && lines.length > 0 && (
        <button
          className="logs__jump button button--primary"
          onClick={() => {
            setFollow(true)
            const element = viewport.current
            if (element) element.scrollTop = element.scrollHeight
          }}
          type="button"
        >
          Jump to newest
        </button>
      )}
    </div>
  )
}
