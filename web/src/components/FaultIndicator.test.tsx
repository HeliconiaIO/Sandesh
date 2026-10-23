/**
 * The injected-fault indicator (ADR-084).
 *
 * The behaviour under test is the "loudly visible" half of the ADR: a
 * configured fault must be impossible to miss, and an unconfigured one must
 * leave no trace in the header.
 */

import { describe, expect, it } from 'vitest'
import { render, screen } from '@testing-library/react'

import { FaultIndicator } from './FaultIndicator'
import type { Faults } from '../api/client'

const active: Faults = {
  active: true,
  rules: [{ mode: 'reject_at_data', selector: {} }],
}

describe('fault indicator', () => {
  it('renders nothing when no fault is configured', () => {
    const { container } = render(
      <FaultIndicator faults={{ active: false, rules: [] }} />,
    )

    // Not merely hidden: a chip reading "no faults" would be noise on the
    // resting state, which is every session that never injects one.
    expect(container).toBeEmptyDOMElement()
  })

  it('renders nothing before the query resolves', () => {
    const { container } = render(<FaultIndicator faults={undefined} />)
    expect(container).toBeEmptyDOMElement()
  })

  it('names the state in words, not only colour', () => {
    render(<FaultIndicator faults={active} />)

    // §11: the meaning has to survive a greyscale screenshot.
    expect(screen.getByText('Faults on')).toBeInTheDocument()
  })

  it('shows how many rules are in force', () => {
    render(
      <FaultIndicator
        faults={{
          active: true,
          rules: [
            { mode: 'reject_at_rcpt', selector: {} },
            { mode: 'connection_drop', selector: {} },
          ],
        }}
      />,
    )

    expect(screen.getByText('2')).toBeInTheDocument()
  })

  it('tells assistive technology what is happening and how to stop it', () => {
    render(<FaultIndicator faults={active} />)

    const status = screen.getByRole('status')
    expect(status).toHaveTextContent('1 fault rule injected')
    expect(status).toHaveTextContent('failed on purpose')
    // The fix belongs with the warning: someone who arrived here confused
    // should not have to go looking for it.
    expect(status).toHaveTextContent('DELETE /api/v1/faults')
  })

  it('uses the singular for one rule and the plural for more', () => {
    const { rerender } = render(<FaultIndicator faults={active} />)
    expect(screen.getByRole('status')).toHaveTextContent('1 fault rule injected')

    rerender(
      <FaultIndicator
        faults={{
          active: true,
          rules: [
            { mode: 'reject_at_rcpt', selector: {} },
            { mode: 'connection_drop', selector: {} },
          ],
        }}
      />,
    )
    expect(screen.getByRole('status')).toHaveTextContent('2 fault rules injected')
  })
})
