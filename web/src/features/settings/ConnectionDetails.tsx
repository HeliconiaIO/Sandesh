/**
 * The connection details (§60, UI_UX_DIRECTION §5).
 *
 * The single most valuable element in the product: the answer to the only
 * question a new user actually has, which is "how do I point my app at this?".
 * Every value comes from `GET /api/v1/info` — never a hardcoded default, so
 * what is shown is what the running server is actually bound to.
 */

import { CopyButton } from '../../components/CopyButton'
import { useInfo } from '../../hooks/queries'
import './ConnectionDetails.css'

export function ConnectionDetails({ compact = false }: { compact?: boolean }) {
  const { data: info, isPending, isError } = useInfo()

  if (isPending) {
    return <p className="connection-details__note">Loading connection details</p>
  }

  if (isError || !info) {
    return <p className="connection-details__note">Connection details unavailable.</p>
  }

  const rows: Array<{ label: string; value: string | null }> = [
    { label: 'SMTP', value: info.smtp_address ?? null },
    { label: 'IMAP', value: info.imap_address ?? null },
    { label: 'POP3', value: info.pop3_address ?? null },
    { label: 'HTTP', value: info.http_address ?? null },
  ]

  return (
    <div className={`connection-details${compact ? ' connection-details--compact' : ''}`}>
      {rows.map(({ label, value }) =>
        value ? (
          <div key={label} className="connection-details__row">
            <span className="connection-details__label">{label}</span>
            <code className="connection-details__value">{value}</code>
            <CopyButton value={value} label={`the ${label} address`} />
          </div>
        ) : (
          <div key={label} className="connection-details__row">
            <span className="connection-details__label">{label}</span>
            <span className="connection-details__off">not running</span>
          </div>
        ),
      )}
    </div>
  )
}
