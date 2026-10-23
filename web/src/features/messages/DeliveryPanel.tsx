/**
 * The delivery view (Phase 7, A1).
 *
 * Answers "how did this message get here, and why did it land where it did?"
 * — the envelope the client actually presented, and what happened to each
 * recipient.
 *
 * The envelope is the point. `To:` and `Cc:` are headers a sender writes and
 * can lie about; the envelope recipients are what the server routed on, which
 * is why a `Bcc` recipient appears here and nowhere in the headers. Showing
 * both would invite the reader to compare them, so this view is deliberately
 * about the envelope alone.
 *
 * Phase 8 A3 adds the Rules section. Its criterion is that an applied rule is
 * "visible in the delivery record", and this is where a human reads that: a
 * keyword appearing on a message with no explanation is the opacity Phase 7
 * was built to remove. The section renders only when a rule fired — a server
 * with no rules configured should not be told about a feature it is not using.
 */

import type { Delivery } from '../../api/client'
import { CopyButton } from '../../components/CopyButton'
import './DeliveryPanel.css'

/** Outcomes carry a stable domain name; these are the human labels. */
const OUTCOME_LABELS: Record<string, string> = {
  delivered: 'Delivered',
  catch_all: 'Catch-all',
  rejected: 'Rejected',
}

/** Renders microseconds at a scale a human reads without counting zeros. */
function formatDuration(microseconds: number): string {
  if (microseconds < 1000) return `${microseconds} µs`
  if (microseconds < 1_000_000) return `${(microseconds / 1000).toFixed(1)} ms`
  return `${(microseconds / 1_000_000).toFixed(2)} s`
}

/** Formats an RFC 3339 timestamp in the viewer's locale, falling back to raw. */
function formatReceivedAt(value: string): string {
  const parsed = new Date(value)
  return Number.isNaN(parsed.getTime()) ? value : parsed.toLocaleString()
}

export function DeliveryPanel({ delivery }: { delivery: Delivery }) {
  return (
    <div className="delivery">
      <section className="delivery__section">
        <h3 className="delivery__heading">Envelope</h3>
        <dl className="delivery__grid">
          <dt>MAIL FROM</dt>
          <dd>
            {delivery.mail_from ? (
              <>
                <code>{delivery.mail_from}</code>
                <CopyButton value={delivery.mail_from} label="Copy sender" />
              </>
            ) : (
              // A null reverse-path is legitimate, not missing data: it is how
              // a bounce identifies itself (RFC 5321 §4.5.5).
              <span className="delivery__muted">
                <code>&lt;&gt;</code> — null reverse-path
              </span>
            )}
          </dd>

          <dt>RCPT TO</dt>
          <dd>
            {delivery.rcpt_to.length === 0 ? (
              <span className="delivery__muted">None recorded</span>
            ) : (
              <ul className="delivery__addresses">
                {delivery.rcpt_to.map((address) => (
                  <li key={address}>
                    <code>{address}</code>
                  </li>
                ))}
              </ul>
            )}
          </dd>

          <dt>Source</dt>
          <dd>
            <code>{delivery.source}</code>
          </dd>

          <dt>HELO</dt>
          <dd>
            {delivery.helo ? (
              <>
                <code>{delivery.helo}</code>{' '}
                {/* SMTP cannot verify this, and a view that presented it as
                    fact would be asserting something the protocol does not
                    know. */}
                <span className="delivery__note">as claimed by the client</span>
              </>
            ) : (
              <span className="delivery__muted">Not given</span>
            )}
          </dd>

          <dt>Peer</dt>
          <dd>
            {delivery.peer ? (
              <code>{delivery.peer}</code>
            ) : (
              <span className="delivery__muted">Unknown</span>
            )}
          </dd>

          <dt>Received</dt>
          <dd>
            {formatReceivedAt(delivery.received_at)}{' '}
            <span className="delivery__note">
              in {formatDuration(delivery.duration_us)}
            </span>
          </dd>
        </dl>
      </section>

      {delivery.rules_applied.length > 0 ? (
        <section className="delivery__section">
          <h3 className="delivery__heading">
            Rules applied
            <span className="delivery__count">{delivery.rules_applied.length}</span>
          </h3>

          <table className="delivery__table">
            <caption className="sr-only">
              Which rules matched this message, and what each one added
            </caption>
            <thead>
              <tr>
                <th scope="col">Rule</th>
                <th scope="col">Added</th>
              </tr>
            </thead>
            <tbody>
              {delivery.rules_applied.map((applied) => (
                <tr key={`${applied.rule_id}-${applied.keyword}`}>
                  {/* The name as it stood when the rule fired, not a live
                      lookup: a rule edited or deleted since must not rewrite
                      what this record says happened. */}
                  <td>{applied.rule_name}</td>
                  <td>
                    <code className="delivery__keyword">{applied.keyword}</code>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </section>
      ) : null}

      <section className="delivery__section">
        <h3 className="delivery__heading">
          Recipients
          <span className="delivery__count">{delivery.recipients.length}</span>
        </h3>

        {delivery.recipients.length === 0 ? (
          <p className="delivery__empty">
            This delivery recorded no per-recipient outcomes.
          </p>
        ) : (
          <table className="delivery__table">
            <caption className="sr-only">
              How each envelope recipient was routed
            </caption>
            <thead>
              <tr>
                <th scope="col">Address</th>
                <th scope="col">Outcome</th>
                <th scope="col">Detail</th>
              </tr>
            </thead>
            <tbody>
              {delivery.recipients.map((recipient) => (
                <tr key={`${recipient.address}-${recipient.outcome}`}>
                  <td>
                    <code>{recipient.address}</code>
                  </td>
                  <td>
                    <span
                      className={`delivery__outcome delivery__outcome--${recipient.outcome}`}
                    >
                      {OUTCOME_LABELS[recipient.outcome] ?? recipient.outcome}
                    </span>
                  </td>
                  <td>
                    {recipient.reason ? (
                      <span className="delivery__reason">{recipient.reason}</span>
                    ) : (
                      <span className="delivery__muted">—</span>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>
    </div>
  )
}
