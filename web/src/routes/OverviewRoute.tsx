/**
 * The landing view (§10).
 *
 * Answers, in order: where do I send mail, is Sandesh healthy, what is here,
 * and what just arrived. Every number comes from the API — there are no
 * computed "insights" and no metric without a backing endpoint (§10, §59).
 */

import { Link } from 'react-router-dom'

import { ErrorState, LoadingState } from '../components/States'
import { ConnectionDetails } from '../features/settings/ConnectionDetails'
import { useConnectionState } from '../hooks/useEventStream'
import { useMailboxes, useMessages, useReady } from '../hooks/queries'
import { formatAddressShort, formatAge } from '../lib/format'
import './OverviewRoute.css'

export function OverviewRoute() {
  const mailboxes = useMailboxes()
  const recent = useMessages(undefined)
  const ready = useReady()
  const connection = useConnectionState()

  const boxes = mailboxes.data?.items ?? []
  const totalMessages = boxes.reduce((sum, mailbox) => sum + mailbox.total, 0)
  const totalUnseen = boxes.reduce((sum, mailbox) => sum + mailbox.unseen, 0)

  return (
    <div className="overview">
      <div className="overview__inner">
        <section className="panel panel--connect">
          <h1 className="panel__title">Send mail here</h1>
          <p className="panel__lede">
            Point your application's SMTP configuration at this address. Anything it
            delivers appears below the moment it arrives.
          </p>
          <ConnectionDetails />
        </section>

        <section className="panel">
          <h2 className="panel__title">Status</h2>
          <dl className="stats">
            <div className="stat">
              <dt>Messages</dt>
              <dd>{mailboxes.isPending ? '—' : totalMessages}</dd>
            </div>
            <div className="stat">
              <dt>Unread</dt>
              <dd>{mailboxes.isPending ? '—' : totalUnseen}</dd>
            </div>
            <div className="stat">
              <dt>Mailboxes</dt>
              <dd>{mailboxes.isPending ? '—' : boxes.length}</dd>
            </div>
            <div className="stat">
              <dt>Storage</dt>
              <dd className={ready.data?.database ? 'stat--ok' : 'stat--error'}>
                {ready.isPending ? '—' : ready.data?.database ? 'Healthy' : 'Unreachable'}
              </dd>
            </div>
            <div className="stat">
              <dt>Event stream</dt>
              <dd className={connection === 'connected' ? 'stat--ok' : 'stat--warn'}>
                {connection === 'connected'
                  ? 'Live'
                  : connection === 'connecting'
                    ? 'Connecting'
                    : 'Offline'}
              </dd>
            </div>
          </dl>
        </section>

        <section className="panel">
          <h2 className="panel__title">Recent</h2>
          {recent.isPending ? (
            <LoadingState label="Loading recent mail" />
          ) : recent.isError ? (
            <ErrorState error={recent.error} onRetry={() => void recent.refetch()} />
          ) : recent.items.length > 0 ? (
            <ul className="recent">
              {recent.items.slice(0, 8).map((message) => {
                const placement = message.entries[0]
                const href = placement
                  ? `/mailbox/${placement.mailbox_id}/message/${message.id}`
                  : '/'
                return (
                  <li key={message.id}>
                    <Link to={href} className="recent__item">
                      <span className="recent__subject">
                        {message.subject || '(no subject)'}
                      </span>
                      <span className="recent__from">{formatAddressShort(message.from)}</span>
                      <time className="recent__age" dateTime={message.received_at}>
                        {formatAge(message.received_at)}
                      </time>
                    </Link>
                  </li>
                )
              })}
            </ul>
          ) : (
            <p className="panel__lede">
              Nothing has arrived yet. Send a message to the SMTP address above.
            </p>
          )}
        </section>
      </div>
    </div>
  )
}
