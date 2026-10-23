/**
 * The delivery panel's Rules section (Phase 8 A3).
 *
 * The property worth testing is the one the acceptance criterion names: an
 * applied rule is *visible in the delivery record*, and it is identified by the
 * name it had when it fired rather than by an id a reader cannot interpret.
 * The section's absence when nothing fired matters too — a server with no rules
 * configured should not be shown a feature it is not using.
 */

import { describe, expect, it } from 'vitest'
import { render, screen } from '@testing-library/react'

import type { Delivery } from '../../api/client'
import { DeliveryPanel } from './DeliveryPanel'

function delivery(overrides: Partial<Delivery> = {}): Delivery {
  return {
    id: '019a0000-0000-7000-8000-00000000d001',
    message_id: '019a0000-0000-7000-8000-00000000m001',
    mail_from: 'billing@vendor.test',
    rcpt_to: ['dev@example.test'],
    source: 'smtp',
    peer: '127.0.0.1:51000',
    helo: 'test.local',
    received_at: '2026-01-01T10:00:00Z',
    duration_us: 1500,
    recipients: [],
    rules_applied: [],
    ...overrides,
  }
}

describe('DeliveryPanel rules', () => {
  it('names the rule that fired and the keyword it added', () => {
    render(
      <DeliveryPanel
        delivery={delivery({
          rules_applied: [
            {
              rule_id: '019a0000-0000-7000-8000-00000000r001',
              rule_name: 'invoices',
              keyword: '$Invoice',
            },
          ],
        })}
      />,
    )

    expect(screen.getByText('Rules applied')).toBeInTheDocument()
    // The name, not the id: a record that reads "rule 019a… matched" once its
    // rule is gone explains nothing.
    expect(screen.getByText('invoices')).toBeInTheDocument()
    expect(screen.getByText('$Invoice')).toBeInTheDocument()
  })

  it('lists every rule that fired', () => {
    render(
      <DeliveryPanel
        delivery={delivery({
          rules_applied: [
            { rule_id: 'a', rule_name: 'invoices', keyword: '$Invoice' },
            { rule_id: 'b', rule_name: 'from billing', keyword: '$Billing' },
          ],
        })}
      />,
    )

    expect(screen.getByText('invoices')).toBeInTheDocument()
    expect(screen.getByText('from billing')).toBeInTheDocument()
    expect(screen.getByText('2')).toBeInTheDocument()
  })

  it('hides the section entirely when no rule fired', () => {
    // Not an empty table with a "none" row: a server with no rules configured
    // has no reason to be told about the feature.
    render(<DeliveryPanel delivery={delivery()} />)

    expect(screen.queryByText('Rules applied')).not.toBeInTheDocument()
  })

  it('still renders the envelope, which is the panel of record', () => {
    render(<DeliveryPanel delivery={delivery()} />)

    expect(screen.getByText('Envelope')).toBeInTheDocument()
    expect(screen.getByText('billing@vendor.test')).toBeInTheDocument()
  })
})
