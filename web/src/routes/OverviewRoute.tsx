/**
 * The landing view (§10).
 *
 * Answers, in order: where do I send mail, is Sandesh healthy, what is here,
 * and what just arrived. Every number comes from the API — there are no
 * computed "insights" and no metric without a backing endpoint (§10, §59).
 */

import { Link } from 'react-router-dom'
import {
  ArrowRight,
  Database,
  Folders,
  Mail,
  MailOpen,
  Paperclip,
  Radio,
  type LucideIcon,
} from 'lucide-react'

import { Avatar } from '../components/Avatar'
import { CopyButton } from '../components/CopyButton'
import { ErrorState, LoadingState } from '../components/States'
import { ConnectionDetails } from '../features/settings/ConnectionDetails'
import { useConnectionState } from '../hooks/useEventStream'
import { useInfo, useMailboxes, useMessages, useReady } from '../hooks/queries'
import { formatAddressShort, formatAge } from '../lib/format'
import './OverviewRoute.css'

const SEEN = '\\Seen'

/** Splits `host:port` at the LAST colon, so an IPv6 literal keeps its own. */
function splitAddress(address: string): { host: string; port: string } {
  const index = address.lastIndexOf(':')
  if (index < 0) return { host: address, port: '' }
  return { host: address.slice(0, index), port: address.slice(index + 1) }
}

function Stat({
  icon: Icon,
  label,
  value,
  tone,
}: {
  icon: LucideIcon
  label: string
  value: string | number
  tone?: 'ok' | 'warn' | 'error' | undefined
}) {
  return (
    <div className="stat overview-stat">
      <dt>
        <Icon size={14} aria-hidden="true" className="overview-stat__icon" />
        {label}
      </dt>
      <dd className={tone ? `stat--${tone}` : undefined}>{value}</dd>
    </div>
  )
}

export function OverviewRoute() {
  const mailboxes = useMailboxes()
  const recent = useMessages(undefined)
  const ready = useReady()
  const connection = useConnectionState()
  const { data: info } = useInfo()

  const boxes = mailboxes.data?.items ?? []
  const totalMessages = boxes.reduce((sum, mailbox) => sum + mailbox.total, 0)
  const totalUnseen = boxes.reduce((sum, mailbox) => sum + mailbox.unseen, 0)
  const inbox = boxes.find((mailbox) => mailbox.name.toLowerCase() === 'inbox') ?? boxes[0]

  const smtp = info?.smtp_address ? splitAddress(info.smtp_address) : null
  const snippet = smtp ? `SMTP_HOST=${smtp.host}\nSMTP_PORT=${smtp.port}` : null

  const pending = mailboxes.isPending

  return (
    <div className="overview">
      <div className="overview__inner">
        <section className="overview-hero" aria-labelledby="overview-hero-title">
          <div className="overview-hero__intro">
            <span className={`overview-hero__eyebrow overview-hero__eyebrow--${connection}`}>
              <span className="overview-hero__eyebrow-dot" aria-hidden="true" />
              {connection === 'connected'
                ? 'Capturing mail'
                : connection === 'connecting'
                  ? 'Connecting'
                  : 'Event stream offline'}
            </span>
            <h1 id="overview-hero-title" className="overview-hero__title">
              Send mail here
            </h1>
            <p className="overview-hero__lede">
              Point your application's SMTP configuration at this address. Anything it
              delivers appears below the moment it arrives — no account, no TLS, no setup.
            </p>
            {snippet ? (
              <div className="overview-snippet">
                <pre className="overview-snippet__code">{snippet}</pre>
                <CopyButton value={snippet} label="the SMTP environment variables" />
              </div>
            ) : null}
          </div>
          <div className="overview-hero__endpoints">
            <ConnectionDetails />
          </div>
        </section>

        <section aria-labelledby="overview-status-title">
          <h2 id="overview-status-title" className="sr-only">
            Status
          </h2>
          <dl className="stats overview-stats">
            <Stat icon={Mail} label="Messages" value={pending ? '—' : totalMessages} />
            <Stat icon={MailOpen} label="Unread" value={pending ? '—' : totalUnseen} />
            <Stat icon={Folders} label="Mailboxes" value={pending ? '—' : boxes.length} />
            <Stat
              icon={Database}
              label="Storage"
              value={ready.isPending ? '—' : ready.data?.database ? 'Healthy' : 'Unreachable'}
              tone={ready.isPending ? undefined : ready.data?.database ? 'ok' : 'error'}
            />
            <Stat
              icon={Radio}
              label="Event stream"
              value={
                connection === 'connected'
                  ? 'Live'
                  : connection === 'connecting'
                    ? 'Connecting'
                    : 'Offline'
              }
              tone={connection === 'connected' ? 'ok' : 'warn'}
            />
          </dl>
        </section>

        <section className="panel overview-recent" aria-labelledby="overview-recent-title">
          <header className="overview-recent__header">
            <h2 id="overview-recent-title" className="panel__title">
              Recent
            </h2>
            {inbox ? (
              <Link to={`/mailbox/${inbox.id}`} className="overview-recent__all">
                Open {inbox.name}
                <ArrowRight size={13} aria-hidden="true" />
              </Link>
            ) : null}
          </header>
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
                const sender = formatAddressShort(message.from)
                const unread = message.entries.some((entry) => !entry.flags.includes(SEEN))
                return (
                  <li key={message.id}>
                    <Link
                      to={href}
                      className={`recent__item${unread ? ' recent__item--unread' : ''}`}
                    >
                      <Avatar name={sender} size="sm" />
                      <span className="recent__text">
                        <span className="recent__subject">
                          {message.subject || '(no subject)'}
                        </span>
                        <span className="recent__from">{sender}</span>
                      </span>
                      {message.has_attachments ? (
                        <span className="recent__attach">
                          <Paperclip size={13} aria-hidden="true" />
                          <span className="sr-only">Has attachments</span>
                        </span>
                      ) : null}
                      <time className="recent__age" dateTime={message.received_at}>
                        {formatAge(message.received_at)}
                      </time>
                    </Link>
                  </li>
                )
              })}
            </ul>
          ) : (
            <div className="overview-recent__empty">
              <Mail size={20} aria-hidden="true" />
              <p className="panel__lede">
                Nothing has arrived yet. Send a message to the SMTP address above.
              </p>
            </div>
          )}
        </section>
      </div>
    </div>
  )
}
