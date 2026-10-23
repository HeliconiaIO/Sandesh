/**
 * The API reference and try-it console (Phase 7 A5).
 *
 * Closes the criterion "the API console can execute every documented
 * endpoint". Two things follow from that wording and shape everything here:
 *
 * *Every* endpoint — so the operation list is derived from the served
 * document, never from a hand-maintained list. An endpoint added to the Rust
 * API appears here on the next build with no edit to this file, which is the
 * only way the criterion can stay true after today.
 *
 * *Execute* — so requests go to the running server at its own origin, not to
 * a mock and not to a placeholder base URL. The response shown is the response
 * the server gave, status and headers included.
 *
 * ## Why this is hand-built rather than Scalar or Swagger UI
 *
 * The CSP this origin serves (`assets.rs`) is `default-src 'self'` with no CDN
 * and no `unsafe-eval`, and a test asserts `unsafe-eval` never appears.
 * Swagger UI requires it. Loading any of them from a CDN would mean widening a
 * policy that protects an origin holding real mail — a bad trade for a
 * developer convenience, so the console is built from the design system
 * already here.
 */

import { useEffect, useMemo, useState } from 'react'

import { CopyButton } from '../components/CopyButton'
import { EmptyState, ErrorState, LoadingState } from '../components/States'
import {
  buildPath,
  buildQuery,
  describe,
  flatten,
  sampleFor,
  typeLabel,
  type Method,
  type OpenApiDocument,
  type Operation,
} from '../api/openapi'
import './ApiRoute.css'

/** What a request produced, once it has come back. */
interface Outcome {
  status: number
  statusText: string
  durationMs: number
  contentType: string
  body: string
  /** Set when the request never reached the server at all. */
  transportError?: string
}

/** Pretty-prints a JSON body, leaving anything else exactly as received. */
function formatBody(body: string, contentType: string): string {
  if (!contentType.includes('json')) return body
  try {
    return JSON.stringify(JSON.parse(body), null, 2)
  } catch {
    // A malformed body from a server claiming JSON is itself the finding;
    // showing it raw is more useful than showing a parse error.
    return body
  }
}

/** The colour class for a status code. Semantic, never decorative. */
function statusKind(status: number): string {
  if (status >= 500) return 'api-console__status--error'
  if (status >= 400) return 'api-console__status--warn'
  if (status >= 200 && status < 300) return 'api-console__status--ok'
  return 'api-console__status--info'
}

export function ApiRoute() {
  const [document, setDocument] = useState<OpenApiDocument | null>(null)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [selectedId, setSelectedId] = useState<string | null>(null)
  const [filter, setFilter] = useState('')

  // Held in React state for the session and never persisted. The UI shares an
  // origin with the API, so a token in localStorage would be a credential at
  // rest readable by anything running on that origin; re-typing it after a
  // reload is a small price for a local development tool.
  const [token, setToken] = useState('')

  const [pathValues, setPathValues] = useState<Record<string, string>>({})
  const [queryValues, setQueryValues] = useState<Record<string, string>>({})
  const [body, setBody] = useState('')
  const [sending, setSending] = useState(false)
  const [outcome, setOutcome] = useState<Outcome | null>(null)

  // Bumped to re-run the fetch below. A contract that failed to load is
  // usually a server that was still starting; without a retry the only way
  // back was a full page reload.
  const [attempt, setAttempt] = useState(0)

  useEffect(() => {
    let cancelled = false
    setLoadError(null)

    fetch('/api/openapi.json', { headers: { Accept: 'application/json' } })
      .then(async (response) => {
        if (!response.ok) throw new Error(`the server answered ${response.status}`)
        return (await response.json()) as OpenApiDocument
      })
      .then((loaded) => {
        if (!cancelled) setDocument(loaded)
      })
      .catch((error: unknown) => {
        if (!cancelled) setLoadError(error instanceof Error ? error.message : String(error))
      })

    return () => {
      cancelled = true
    }
  }, [attempt])

  const operations = useMemo(() => (document ? flatten(document) : []), [document])

  const visible = useMemo(() => {
    const needle = filter.trim().toLowerCase()
    if (!needle) return operations
    return operations.filter(
      (operation) =>
        operation.path.toLowerCase().includes(needle) ||
        operation.method.includes(needle) ||
        operation.tag.toLowerCase().includes(needle),
    )
  }, [operations, filter])

  const selected = useMemo(
    () => operations.find((operation) => operation.id === selectedId) ?? null,
    [operations, selectedId],
  )

  /** Resets the request builder whenever a different operation is chosen. */
  function choose(operation: Operation) {
    setSelectedId(operation.id)
    setOutcome(null)
    setPathValues({})
    setQueryValues({})

    const schema = operation.requestBody?.content?.['application/json']?.schema
    if (schema && document) {
      setBody(JSON.stringify(sampleFor(schema, document), null, 2))
    } else {
      setBody('')
    }
  }

  const pathParams = selected?.parameters.filter((parameter) => parameter.in === 'path') ?? []
  const queryParams = selected?.parameters.filter((parameter) => parameter.in === 'query') ?? []

  const requestUrl = selected
    ? buildPath(selected.path, pathValues) +
      buildQuery(
        queryValues,
        queryParams.map((parameter) => parameter.name),
      )
    : ''

  /** Whether every required path parameter has been supplied. */
  const ready =
    selected !== null && pathParams.every((parameter) => pathValues[parameter.name]?.trim())

  async function send() {
    if (!selected || !ready) return

    setSending(true)
    setOutcome(null)
    const startedAt = performance.now()

    const headers: Record<string, string> = { Accept: 'application/json' }
    if (token.trim()) headers.Authorization = `Bearer ${token.trim()}`

    const sendsBody = selected.method !== 'get' && selected.method !== 'delete' && body.trim()
    if (sendsBody) headers['Content-Type'] = 'application/json'

    try {
      const init: RequestInit = { method: selected.method.toUpperCase(), headers }
      // Assigned only when there is a body: `exactOptionalPropertyTypes`
      // rejects an explicit `undefined` for an optional property.
      if (sendsBody) init.body = body

      const response = await fetch(requestUrl, init)
      const text = await response.text()
      const contentType = response.headers.get('content-type') ?? ''

      setOutcome({
        status: response.status,
        statusText: response.statusText,
        durationMs: Math.round(performance.now() - startedAt),
        contentType,
        body: formatBody(text, contentType),
      })
    } catch (error: unknown) {
      // A network failure is not an HTTP status, and reporting it as one
      // would invent a response the server never sent.
      setOutcome({
        status: 0,
        statusText: 'no response',
        durationMs: Math.round(performance.now() - startedAt),
        contentType: '',
        body: '',
        transportError: error instanceof Error ? error.message : String(error),
      })
    } finally {
      setSending(false)
    }
  }

  /** The equivalent curl, so a request can leave the browser. */
  const curl = useMemo(() => {
    if (!selected) return ''
    const parts = [`curl -X ${selected.method.toUpperCase()} "${window.location.origin}${requestUrl}"`]
    if (token.trim()) parts.push(`  -H "Authorization: Bearer ${token.trim()}"`)
    if (selected.method !== 'get' && selected.method !== 'delete' && body.trim()) {
      parts.push('  -H "Content-Type: application/json"')
      parts.push(`  -d '${body.replace(/\n\s*/g, '')}'`)
    }
    return parts.join(' \\\n')
  }, [selected, requestUrl, token, body])

  if (loadError) {
    return (
      <div className="api-console api-console--message">
        <ErrorState
          error={new Error(`The contract could not be loaded from /api/openapi.json: ${loadError}`)}
          onRetry={() => setAttempt((current) => current + 1)}
        />
      </div>
    )
  }

  if (!document) {
    return (
      <div className="api-console api-console--message">
        <LoadingState label="Loading the API contract" />
      </div>
    )
  }

  return (
    <div className={`api-console${selected ? ' api-console--has-selection' : ''}`}>
      <aside className="api-console__list">
        <div className="api-console__list-header">
          <input
            aria-label="Filter endpoints"
            className="api-console__filter"
            onChange={(event) => setFilter(event.target.value)}
            placeholder="Filter endpoints…"
            type="search"
            value={filter}
          />
          <span className="api-console__count">
            {visible.length} of {operations.length}
          </span>
        </div>

        {visible.length === 0 && (
          <p className="api-console__no-match" role="status">
            No endpoint matches “{filter}”.
          </p>
        )}

        <ul className="api-console__operations">
          {visible.map((operation) => (
            <li key={operation.id}>
              <button
                aria-current={operation.id === selectedId}
                className={`api-console__operation${
                  operation.id === selectedId ? ' api-console__operation--active' : ''
                }`}
                onClick={() => choose(operation)}
                type="button"
              >
                <span className={`api-console__method api-console__method--${operation.method}`}>
                  {operation.method.toUpperCase()}
                </span>
                <span className="api-console__path">{operation.path}</span>
              </button>
            </li>
          ))}
        </ul>
      </aside>

      <section className="api-console__detail">
        {!selected ? (
          <EmptyState title="API reference & console">
            Every endpoint this server documents, read from <code>/api/openapi.json</code> as it
            is running now. Choose one to see its parameters and to run it against this instance.
          </EmptyState>
        ) : (
          <>
            <button
              type="button"
              className="api-console__back-btn"
              onClick={() => setSelectedId(null)}
              aria-label="Back to endpoint list"
            >
              ← Back to endpoints
            </button>
            <header className="api-console__header">
              <div className="api-console__title">
                <span className={`api-console__method api-console__method--${selected.method}`}>
                  {selected.method.toUpperCase()}
                </span>
                <code className="api-console__title-path">{selected.path}</code>
              </div>
              {selected.description &&
                describe(selected.description).map((line, lineIndex) =>
                  line[0]?.kind === 'heading' ? (
                    <h2 className="api-console__doc-heading" key={lineIndex}>
                      {line[0].text}
                    </h2>
                  ) : (
                    <p className="api-console__description" key={lineIndex}>
                      {line.map((segment, index) =>
                        segment.kind === 'code' ? (
                          <code className="api-console__doc-code" key={index}>
                            {segment.text}
                          </code>
                        ) : (
                          <span key={index}>{segment.text}</span>
                        ),
                      )}
                    </p>
                  ),
                )}
            </header>

            <div className="api-console__grid">
              <div className="panel api-console__request">
                <h2 className="panel__title">Request</h2>

                <label className="api-console__field">
                  <span className="api-console__label">
                    Bearer token
                    <span className="api-console__hint">
                      Only needed when the API requires authentication. Kept in memory for this
                      session and never stored.
                    </span>
                  </span>
                  <input
                    autoComplete="off"
                    className="api-console__input"
                    onChange={(event) => setToken(event.target.value)}
                    placeholder="(none)"
                    type="password"
                    value={token}
                  />
                </label>

                {pathParams.length > 0 && (
                  <fieldset className="api-console__group">
                    <legend className="api-console__legend">Path</legend>
                    {pathParams.map((parameter) => (
                      <label className="api-console__field" key={parameter.name}>
                        <span className="api-console__label">
                          {parameter.name}
                          <span className="api-console__type">
                            {typeLabel(parameter.schema, document)}
                          </span>
                          <span className="api-console__required">required</span>
                        </span>
                        <input
                          className="api-console__input"
                          onChange={(event) =>
                            setPathValues((current) => ({
                              ...current,
                              [parameter.name]: event.target.value,
                            }))
                          }
                          placeholder={parameter.description ?? parameter.name}
                          value={pathValues[parameter.name] ?? ''}
                        />
                      </label>
                    ))}
                  </fieldset>
                )}

                {queryParams.length > 0 && (
                  <fieldset className="api-console__group">
                    <legend className="api-console__legend">Query</legend>
                    {queryParams.map((parameter) => (
                      <label className="api-console__field" key={parameter.name}>
                        <span className="api-console__label">
                          {parameter.name}
                          <span className="api-console__type">
                            {typeLabel(parameter.schema, document)}
                          </span>
                        </span>
                        <input
                          className="api-console__input"
                          onChange={(event) =>
                            setQueryValues((current) => ({
                              ...current,
                              [parameter.name]: event.target.value,
                            }))
                          }
                          placeholder={parameter.description ?? 'optional'}
                          value={queryValues[parameter.name] ?? ''}
                        />
                      </label>
                    ))}
                  </fieldset>
                )}

                {selected.requestBody && (
                  <fieldset className="api-console__group">
                    <legend className="api-console__legend">Body</legend>
                    <textarea
                      aria-label="Request body"
                      className="api-console__body"
                      onChange={(event) => setBody(event.target.value)}
                      rows={10}
                      spellCheck={false}
                      value={body}
                    />
                  </fieldset>
                )}

                <div className="api-console__url">
                  <span className="api-console__label">URL</span>
                  <code className="api-console__url-value">{requestUrl}</code>
                </div>

                <div className="api-console__actions">
                  <button
                    className="button button--primary"
                    disabled={!ready || sending}
                    onClick={() => void send()}
                    type="button"
                  >
                    {sending ? 'Sending…' : 'Send request'}
                  </button>
                  <CopyButton label="Copy curl" value={curl} />
                </div>

                {!ready && pathParams.length > 0 && (
                  <p className="api-console__blocked">
                    Fill every path parameter to send this request.
                  </p>
                )}
              </div>

              <div className="panel api-console__response">
                <h2 className="panel__title">Response</h2>

                {!outcome ? (
                  <p className="api-console__placeholder">
                    Nothing sent yet. The response appears here exactly as the server returns it.
                  </p>
                ) : outcome.transportError ? (
                  <>
                    <p className="api-console__blocked">
                      The request never reached the server: {outcome.transportError}
                    </p>
                  </>
                ) : (
                  <>
                    <div className="api-console__outcome">
                      <span className={`api-console__status ${statusKind(outcome.status)}`}>
                        {outcome.status} {outcome.statusText}
                      </span>
                      <span className="api-console__meta">{outcome.durationMs}ms</span>
                      {outcome.contentType && (
                        <span className="api-console__meta">{outcome.contentType}</span>
                      )}
                    </div>
                    {outcome.body ? (
                      <pre className="api-console__payload">{outcome.body}</pre>
                    ) : (
                      <p className="api-console__placeholder">No response body.</p>
                    )}
                  </>
                )}

                {selected.responses.length > 0 && (
                  <div className="api-console__documented">
                    <h3 className="api-console__subtitle">Documented responses</h3>
                    <ul className="api-console__responses">
                      {selected.responses.map((response) => (
                        <li key={response.status}>
                          <code className="api-console__response-status">{response.status}</code>
                          <span>{response.description}</span>
                        </li>
                      ))}
                    </ul>
                  </div>
                )}
              </div>
            </div>
          </>
        )}
      </section>
    </div>
  )
}

export type { Method }
