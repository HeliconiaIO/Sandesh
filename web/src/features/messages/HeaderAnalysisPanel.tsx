/**
 * Header analysis: the interpreted view of the same headers the flat table
 * shows (Phase 7 A3).
 *
 * Two things a developer asks of a header block that a flat list answers
 * badly: *how did this message travel*, and *what did the servers along the way
 * say about its authenticity*. The first is a timeline — `Received:` fields are
 * a chain, and their per-hop delay is where a slow relay shows itself. The
 * second is a panel of claims.
 *
 * ## Parsed, never verified
 *
 * This panel renders what upstream servers wrote. Sandesh checks none of it.
 * That is stated once in the panel caption and reinforced by the vocabulary: a
 * result reads "upstream said pass", never "pass", and no result is ever drawn
 * in a success colour. A green tick here would be read as "my production mail
 * will pass", which is a claim a development mail server has no basis to make.
 */

import { ArrowDown, ShieldQuestion } from 'lucide-react'

import type { HeaderAnalysis, ReceivedHop } from '../../api/client'
import './HeaderAnalysisPanel.css'

export function HeaderAnalysisPanel({ analysis }: { analysis: HeaderAnalysis }) {
  const nothingFound =
    analysis.hops.length === 0 &&
    analysis.dkim.length === 0 &&
    analysis.auth_results.length === 0 &&
    analysis.spf.length === 0 &&
    analysis.spam.length === 0

  if (nothingFound) {
    return (
      <p className="analysis__empty">
        This message carries no <code>Received:</code> chain and no authentication headers.
        That is normal for mail injected through the API rather than delivered over SMTP.
      </p>
    )
  }

  return (
    <section className="analysis" aria-label="Header analysis">
      {analysis.hops.length > 0 ? (
        <section className="analysis__section">
          <h3 className="analysis__heading">
            Route
            <span className="analysis__count">
              {analysis.hops.length} {analysis.hops.length === 1 ? 'hop' : 'hops'}
            </span>
          </h3>
          <p className="analysis__note">
            Oldest first. Each hop wrote its own <code>Received:</code> line, so the chain is
            the servers' account of the journey, not a measurement.
          </p>
          <ol className="analysis__timeline">
            {analysis.hops.map((hop) => (
              <HopRow key={hop.index} hop={hop} />
            ))}
          </ol>
        </section>
      ) : null}

      {analysis.dkim.length > 0 ||
      analysis.auth_results.length > 0 ||
      analysis.spf.length > 0 ? (
        <section className="analysis__section">
          <h3 className="analysis__heading">
            Authentication
            <ShieldQuestion size={13} aria-hidden="true" className="analysis__heading-icon" />
          </h3>
          <p className="analysis__note analysis__note--caveat">
            {analysis.disclaimer}
          </p>

          {analysis.auth_results.length > 0 ? (
            <ul className="analysis__claims">
              {analysis.auth_results.map((result, index) => (
                <li key={`${result.method}-${index}`} className="analysis__claim">
                  <span className="analysis__claim-method">{result.method}</span>
                  {/* Deliberately not a badge: the wording keeps the speaker
                      visible, so the result reads as a report, not a verdict. */}
                  <span className="analysis__claim-said">
                    <span className="analysis__claim-host">{result.authserv_id}</span> reported{' '}
                    <span className="analysis__claim-result">{result.result}</span>
                  </span>
                  {result.properties.length > 0 ? (
                    <dl className="analysis__props">
                      {result.properties.map((property, propertyIndex) => (
                        <div key={`${property.name}-${propertyIndex}`} className="analysis__prop">
                          <dt>{property.name}</dt>
                          <dd>{property.value}</dd>
                        </div>
                      ))}
                    </dl>
                  ) : null}
                </li>
              ))}
            </ul>
          ) : null}

          {analysis.spf.map((record, index) => (
            <div key={`spf-${index}`} className="analysis__claim">
              <span className="analysis__claim-method">Received-SPF</span>
              <span className="analysis__claim-said">
                upstream wrote <span className="analysis__claim-result">{record.result}</span>
              </span>
              {record.detail ? <p className="analysis__detail">{record.detail}</p> : null}
            </div>
          ))}

          {analysis.dkim.map((signature, index) => (
            <div key={`dkim-${index}`} className="analysis__signature">
              <span className="analysis__claim-method">
                DKIM signature{analysis.dkim.length > 1 ? ` ${index + 1}` : ''}
              </span>
              <dl className="analysis__props">
                <Field label="domain" value={signature.domain} />
                <Field label="selector" value={signature.selector} />
                <Field label="algorithm" value={signature.algorithm} />
                <Field label="canonicalization" value={signature.canonicalization} />
                <Field label="signed" value={signature.signed_headers.join(', ') || null} />
                <Field label="signed at" value={signature.timestamp} />
                <Field label="expires" value={signature.expiration} />
              </dl>
            </div>
          ))}
        </section>
      ) : null}

      {analysis.spam.length > 0 ? (
        <section className="analysis__section">
          <h3 className="analysis__heading">Spam filter headers</h3>
          <p className="analysis__note">
            Shown as written. Scores mean different things to different filters, so no
            threshold is applied here.
          </p>
          <dl className="analysis__props">
            {analysis.spam.map((header, index) => (
              <div key={`${header.name}-${index}`} className="analysis__prop">
                <dt>{header.name}</dt>
                <dd>{header.value || <span className="analysis__blank">empty</span>}</dd>
              </div>
            ))}
          </dl>
        </section>
      ) : null}
    </section>
  )
}

/** One hop of the route, with the gap since the hop before it. */
function HopRow({ hop }: { hop: ReceivedHop }) {
  const unparsed = hop.from === null && hop.by === null

  return (
    <li className="analysis__hop">
      <span className="analysis__hop-index" aria-hidden="true">
        {hop.index + 1}
      </span>
      <div className="analysis__hop-body">
        {unparsed ? (
          // The hop that defeats the parser is usually the interesting one, so
          // it is shown rather than dropped.
          <p className="analysis__hop-raw">{hop.raw}</p>
        ) : (
          <>
            <p className="analysis__hop-route">
              {hop.from ? <span className="analysis__host">{hop.from}</span> : <span className="analysis__blank">unnamed sender</span>}
              <ArrowDown size={11} aria-hidden="true" className="analysis__hop-arrow" />
              {hop.by ? <span className="analysis__host">{hop.by}</span> : <span className="analysis__blank">unnamed receiver</span>}
            </p>
            <p className="analysis__hop-meta">
              {hop.with ? <span>via {hop.with}</span> : null}
              {hop.for_recipient ? <span>for {hop.for_recipient}</span> : null}
              {hop.id ? <span>id {hop.id}</span> : null}
              {hop.timestamp ? <span>{hop.timestamp}</span> : null}
            </p>
          </>
        )}
      </div>
      {hop.delay_micros !== null && hop.delay_micros !== undefined ? (
        <span
          className={`analysis__delay${hop.delay_micros < 0 ? ' analysis__delay--backwards' : ''}`}
          title={
            hop.delay_micros < 0
              ? 'This hop is stamped earlier than the one before it. Relay clocks disagree.'
              : 'Time between this hop and the previous one, by their own clocks'
          }
        >
          {formatDelay(hop.delay_micros)}
        </span>
      ) : null}
    </li>
  )
}

/** A label/value pair, omitted entirely when the field is absent. */
function Field({ label, value }: { label: string; value: string | null | undefined }) {
  if (!value) {
    return null
  }
  return (
    <div className="analysis__prop">
      <dt>{label}</dt>
      <dd>{value}</dd>
    </div>
  )
}

/**
 * Renders a hop gap at a resolution a human reads.
 *
 * A negative gap is shown as such rather than clamped: disagreeing relay clocks
 * are exactly the kind of fault this view exists to surface.
 */
export function formatDelay(micros: number): string {
  const sign = micros < 0 ? '−' : '+'
  const absolute = Math.abs(micros)

  if (absolute < 1_000) {
    return `${sign}${absolute}µs`
  }
  if (absolute < 1_000_000) {
    return `${sign}${Math.round(absolute / 1_000)}ms`
  }
  const seconds = absolute / 1_000_000
  if (seconds < 60) {
    return `${sign}${seconds.toFixed(seconds < 10 ? 1 : 0)}s`
  }
  const minutes = Math.floor(seconds / 60)
  const remainder = Math.round(seconds % 60)
  return `${sign}${minutes}m ${remainder}s`
}
