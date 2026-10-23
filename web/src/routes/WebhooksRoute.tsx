/**
 * Webhooks and the outbox (Phase 8 A2, ADR-085, ADR-086).
 *
 * The view's single job: answer "did my webhook fire, and if not, why?". Every
 * decision below follows from that.
 *
 * The outbox is the content and the endpoint list is the caption, which is why
 * the split puts registration in a narrow rail and gives the attempt history
 * the room. A terminally-failed entry is rendered as prominently as a delivered
 * one — the backend deliberately keeps it (ADR-086) rather than deleting it,
 * and hiding it here would undo that.
 */

import { useState } from 'react'
import { Trash2 } from 'lucide-react'

import { EmptyState, ErrorState, LoadingState } from '../components/States'
import {
  useOutbox,
  useRegisterWebhook,
  useRemoveWebhook,
  useWebhooks,
} from '../hooks/queries'
import { useDialog } from '../context/DialogContext'
import { ApiError, type OutboxEntry } from '../api/client'
import './WebhooksRoute.css'

/** The three states an entry can be in, as the server spells them. */
type Status = 'pending' | 'delivered' | 'failed'

/**
 * Plain-language status, from the end user's side of the screen.
 *
 * "Queued" rather than "pending" because the question being answered is about
 * the notification, not about a row: a person waiting on a webhook thinks in
 * terms of whether it has gone yet.
 */
const STATUS_LABEL: Record<Status, string> = {
  pending: 'Queued',
  delivered: 'Delivered',
  failed: 'Gave up',
}

/** The clock time an entry was queued, to the second. */
function queuedAt(iso: string): string {
  const at = new Date(iso)
  return Number.isNaN(at.getTime()) ? iso : at.toLocaleTimeString()
}

/**
 * Summarises the payload for the row.
 *
 * The full body is available on the row's detail; the list needs the one fact
 * that distinguishes two notifications from each other.
 */
function describe(entry: OutboxEntry): string {
  try {
    const body = JSON.parse(entry.payload) as Record<string, unknown>
    const uid = body['uid']
    return typeof uid === 'number' ? `UID ${uid}` : String(body['event'] ?? 'notification')
  } catch {
    // A payload that will not parse is still worth showing as a row; it just
    // has nothing to summarise.
    return 'notification'
  }
}

function OutboxRow({ entry }: { entry: OutboxEntry }) {
  const [open, setOpen] = useState(false)
  const status = (entry.status as Status) ?? 'pending'

  return (
    <li className={`outbox__row outbox__row--${status}`}>
      <button
        type="button"
        className="outbox__summary"
        onClick={() => setOpen((was) => !was)}
        aria-expanded={open}
      >
        <span className={`outbox__status outbox__status--${status}`}>
          {STATUS_LABEL[status]}
        </span>
        <span className="outbox__what">{describe(entry)}</span>
        <span className="outbox__attempts">
          {entry.attempts === 1 ? '1 attempt' : `${entry.attempts} attempts`}
        </span>
        <time className="outbox__when">{queuedAt(entry.created_at)}</time>
      </button>

      {/* The reason a failed entry is kept rather than deleted: it explains
          itself. Shown without the row being expanded, because "why did this
          fail" is the whole question. */}
      {entry.last_error ? (
        <p className="outbox__error">{entry.last_error}</p>
      ) : null}

      {open ? <pre className="outbox__payload">{entry.payload}</pre> : null}
    </li>
  )
}

export function WebhooksRoute() {
  const [url, setUrl] = useState('')

  const endpoints = useWebhooks()
  const outbox = useOutbox()
  const register = useRegisterWebhook()
  const remove = useRemoveWebhook()
  const { confirm } = useDialog()

  const rows = outbox.data ?? []
  const registered = endpoints.data ?? []

  const submit = (event: React.FormEvent) => {
    event.preventDefault()
    if (!url.trim()) return
    register.mutate(
      { url: url.trim(), enabled: true },
      { onSuccess: () => setUrl('') },
    )
  }

  const registerError =
    register.error instanceof ApiError ? register.error.detail || register.error.message : null

  return (
    <div className="webhooks">
      <header className="webhooks__bar">
        <h1 className="webhooks__title">Webhooks</h1>
        <p className="webhooks__subtitle">
          Sandesh POSTs to each endpoint when mail is delivered, and retries until it
          is accepted.
        </p>
      </header>

      <div className="webhooks__split">
        <section className="webhooks__rail" aria-labelledby="endpoints-heading">
          <h2 className="webhooks__heading" id="endpoints-heading">
            Endpoints
          </h2>

          <form className="webhooks__form" onSubmit={submit}>
            <label className="webhooks__label" htmlFor="webhook-url">
              Notification URL
            </label>
            <input
              id="webhook-url"
              className="webhooks__input"
              type="url"
              placeholder="http://localhost:3000/hooks/mail"
              value={url}
              onChange={(event) => setUrl(event.target.value)}
            />
            <button
              type="submit"
              className="button button--primary"
              disabled={register.isPending || !url.trim()}
            >
              {register.isPending ? 'Adding' : 'Add endpoint'}
            </button>
            {/* The URL is validated server-side at registration, so a typo is a
                400 here rather than a backlog of failures later. */}
            {registerError ? (
              <p className="webhooks__formError" role="alert">
                {registerError}
              </p>
            ) : null}
          </form>

          {endpoints.isLoading ? <LoadingState label="Loading endpoints" /> : null}
          {endpoints.error ? (
            <ErrorState error={endpoints.error} onRetry={() => void endpoints.refetch()} />
          ) : null}

          {!endpoints.isLoading && !endpoints.error && registered.length === 0 ? (
            <EmptyState title="No endpoints yet">
              Add a URL above and deliver a message to see it fire.
            </EmptyState>
          ) : null}

          <ul className="webhooks__list">
            {registered.map((endpoint) => (
              <li className="webhooks__endpoint" key={endpoint.id}>
                <span className="webhooks__url" title={endpoint.url}>
                  {endpoint.url}
                </span>
                <button
                  type="button"
                  className="webhooks__remove"
                  onClick={async () => {
                    const ok = await confirm({
                      title: 'Remove Webhook Endpoint?',
                      message: `Are you sure you want to remove ${endpoint.url}?`,
                      confirmText: 'Remove',
                      cancelText: 'Cancel',
                      variant: 'danger',
                    })
                    if (ok) {
                      remove.mutate(endpoint.id)
                    }
                  }}
                  aria-label={`Remove ${endpoint.url}`}
                >
                  <Trash2 size={14} aria-hidden="true" />
                </button>
              </li>
            ))}
          </ul>
        </section>

        <section className="webhooks__outbox" aria-labelledby="outbox-heading">
          <h2 className="webhooks__heading" id="outbox-heading">
            Recent notifications
          </h2>

          {outbox.isLoading ? <LoadingState label="Loading the outbox" /> : null}
          {outbox.error ? (
            <ErrorState error={outbox.error} onRetry={() => void outbox.refetch()} />
          ) : null}

          {!outbox.isLoading && !outbox.error && rows.length === 0 ? (
            <EmptyState title="Nothing sent yet">
              {registered.length === 0
                ? 'Add an endpoint, then deliver a message over SMTP.'
                : 'Deliver a message over SMTP and it will appear here.'}
            </EmptyState>
          ) : null}

          <ul className="outbox">
            {rows.map((entry) => (
              <OutboxRow entry={entry} key={entry.id} />
            ))}
          </ul>
        </section>
      </div>
    </div>
  )
}
