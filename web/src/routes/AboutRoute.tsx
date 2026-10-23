/**
 * About and diagnostics (§63, §95).
 *
 * Shows what a developer needs to reason about their instance and nothing
 * more. The database path is shown because a local mail server's data
 * directory is something the developer owns and frequently needs; no
 * credentials or internal paths beyond it are exposed.
 */

import { ConnectionDetails } from '../features/settings/ConnectionDetails'
import { CopyButton } from '../components/CopyButton'
import { useConnectionState } from '../hooks/useEventStream'
import { useInfo, useReady } from '../hooks/queries'
import './SettingsRoute.css'

export function AboutRoute() {
  const { data: info } = useInfo()
  const ready = useReady()
  const connection = useConnectionState()

  return (
    <div className="overview">
      <div className="overview__inner">
        <section className="panel">
          <h1 className="panel__title">Sandesh {info?.version ? `v${info.version}` : ''}</h1>
          <p className="panel__lede">Local mail infrastructure for developers.</p>
          <ConnectionDetails />
        </section>

        <section className="panel">
          <h2 className="panel__title">Diagnostics</h2>
          <dl className="stats">
            <div className="stat">
              <dt>Database</dt>
              <dd className={ready.data?.database ? 'stat--ok' : 'stat--error'}>
                {ready.isPending ? '—' : ready.data?.database ? 'Healthy' : 'Unreachable'}
              </dd>
            </div>
            <div className="stat">
              <dt>Event stream</dt>
              <dd className={connection === 'connected' ? 'stat--ok' : 'stat--warn'}>
                {connection === 'connected' ? 'Connected' : connection}
              </dd>
            </div>
            <div className="stat">
              <dt>WebSocket</dt>
              <dd style={{ fontSize: 'var(--text-mono)' }}>{info?.websocket_path ?? '—'}</dd>
            </div>
          </dl>
        </section>

        <section className="panel">
          <h2 className="panel__title">Security</h2>
          <p className="panel__lede">
            Sandesh accepts mail without authentication by default. That is deliberate for
            local development, and it is why every listener binds to loopback.
          </p>

          <dl className="stats">
            <div className="stat">
              <dt>SMTP authentication</dt>
              {/*
                Stated plainly rather than dressed up. "Disabled" is the honest
                default and a developer must never be unsure which it is; the
                colour reinforces the word rather than replacing it.
              */}
              <dd className={info?.smtp_auth === 'disabled' ? 'stat--warn' : 'stat--ok'}>
                {info?.smtp_auth ?? '—'}
              </dd>
            </div>
            <div className="stat">
              <dt>SMTP TLS</dt>
              <dd className={info?.smtp_tls === 'disabled' ? 'stat--warn' : 'stat--ok'}>
                {info?.smtp_tls ?? '—'}
              </dd>
            </div>
          </dl>

          {info?.tls_fingerprint ? (
            <div className="connection-details">
              <div className="connection-details__row">
                <span className="connection-details__label">SHA-256</span>
                <code className="connection-details__value">{info.tls_fingerprint}</code>
                <CopyButton value={info.tls_fingerprint} label="the certificate fingerprint" />
              </div>
            </div>
          ) : null}

          {info?.database ? (
            <div className="connection-details">
              <div className="connection-details__row">
                <span className="connection-details__label">Data</span>
                <code className="connection-details__value">{info.database}</code>
                <CopyButton value={info.database} label="the database path" />
              </div>
            </div>
          ) : null}
        </section>

        <section className="panel">
          <h2 className="panel__title">API</h2>
          <p className="panel__lede">
            Sandesh publishes its own OpenAPI contract. The types this interface is built
            from are generated from it, so the documentation cannot drift from the API.
          </p>
          <div className="connection-details">
            <div className="connection-details__row">
              <span className="connection-details__label">Spec</span>
              <code className="connection-details__value">/api/openapi.json</code>
              <a className="button" href="/api/openapi.json" target="_blank" rel="noreferrer">
                Open
              </a>
            </div>
          </div>
        </section>
      </div>
    </div>
  )
}
