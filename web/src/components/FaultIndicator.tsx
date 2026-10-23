import { FlaskConical } from 'lucide-react'

import type { Faults } from '../api/client'
import './FaultIndicator.css'

/**
 * The injected-fault indicator (ADR-084).
 *
 * Present only while a fault is configured, which is the point: this is not a
 * status light that sits there reading "normal", it is an interruption that
 * appears when the server has been told to break on purpose.
 *
 * The wording follows from who reads it. Someone seeing this is usually partway
 * through wondering why delivery stopped working, so it names the cause in
 * plain terms and says how many rules are in force, rather than showing a badge
 * that means something only to whoever configured it. `DELETE /api/v1/faults`
 * is named because it is the fix, and a developer who arrived here confused
 * should not have to go looking for it.
 *
 * Amber rather than red: the server is doing exactly what it was asked to do,
 * and nothing is broken. Red is reserved for the event stream dropping, which
 * is a real failure.
 */
export function FaultIndicator({ faults }: { faults: Faults | undefined }) {
  if (!faults?.active) {
    return null
  }

  const count = faults.rules.length
  const summary = `${count} fault ${count === 1 ? 'rule' : 'rules'}`

  return (
    <span className="fault" title="Clear with DELETE /api/v1/faults">
      <FlaskConical className="fault__icon" size={12} aria-hidden="true" />
      {/* A word as well as an icon and a colour (§11): the meaning has to
          survive a greyscale screenshot and a colour-blind reader. */}
      <span className="fault__label">Faults on</span>
      <span className="fault__count">{count}</span>

      {/* Announced on change rather than polled, so a screen reader hears that
          mail is being failed deliberately without being nagged about it. */}
      <span className="sr-only" role="status" aria-live="polite">
        {summary} injected. SMTP delivery is being failed on purpose. Clear them
        with DELETE /api/v1/faults.
      </span>
    </span>
  )
}
