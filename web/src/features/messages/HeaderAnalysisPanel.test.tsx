/**
 * The header analysis panel (Phase 7 A3).
 *
 * The properties worth testing are the ones the acceptance criterion names:
 * that DKIM fields are displayed, and that nothing in the panel claims
 * verification. The hop timeline's ordering and its delay formatting matter
 * too, because both encode a fact about the message rather than decorating it.
 */

import { describe, expect, it } from 'vitest'
import { render, screen } from '@testing-library/react'

import type { HeaderAnalysis } from '../../api/client'
import { HeaderAnalysisPanel, formatDelay } from './HeaderAnalysisPanel'

const DISCLAIMER =
  'Headers are parsed and displayed as written. Sandesh performs no DKIM, SPF or DMARC ' +
  'verification and makes no claim that any signature or policy is valid.'

function analysis(overrides: Partial<HeaderAnalysis> = {}): HeaderAnalysis {
  return {
    hops: [],
    dkim: [],
    auth_results: [],
    spf: [],
    spam: [],
    disclaimer: DISCLAIMER,
    ...overrides,
  }
}

function hop(index: number, overrides: Record<string, unknown> = {}) {
  return {
    index,
    from: `host${index}.example`,
    by: `host${index + 1}.example`,
    with: 'ESMTP',
    for_recipient: null,
    id: null,
    timestamp: null,
    delay_micros: null,
    raw: 'raw received line',
    ...overrides,
  } as HeaderAnalysis['hops'][number]
}

describe('HeaderAnalysisPanel', () => {
  it('says so plainly when there is nothing to analyse', () => {
    // An API-injected message has no chain and no auth headers. That is a
    // normal outcome, not an error, and the empty state says which.
    render(<HeaderAnalysisPanel analysis={analysis()} />)

    // The text is broken by a <code> element, so match the container.
    expect(screen.getByText(/injected through the API/)).toBeInTheDocument()
  })

  it('lists hops oldest first, numbered', () => {
    render(
      <HeaderAnalysisPanel
        analysis={analysis({ hops: [hop(0), hop(1), hop(2)] })}
      />,
    )

    const items = screen.getAllByRole('listitem')
    expect(items).toHaveLength(3)
    expect(items[0]?.textContent).toContain('host0.example')
    expect(items[2]?.textContent).toContain('host2.example')
    expect(screen.getByText('3 hops')).toBeInTheDocument()
  })

  it('shows the gap between hops', () => {
    render(
      <HeaderAnalysisPanel
        analysis={analysis({ hops: [hop(0), hop(1, { delay_micros: 2_500_000 })] })}
      />,
    )

    expect(screen.getByText('+2.5s')).toBeInTheDocument()
  })

  it('marks a backwards gap rather than hiding it', () => {
    // Disagreeing relay clocks are a finding; clamping to zero would conceal
    // exactly what a developer opened this panel to see.
    render(
      <HeaderAnalysisPanel
        analysis={analysis({ hops: [hop(0), hop(1, { delay_micros: -3_000_000 })] })}
      />,
    )

    const delay = screen.getByText('−3.0s')
    expect(delay).toHaveClass('analysis__delay--backwards')
    expect(delay.getAttribute('title')).toMatch(/clocks disagree/)
  })

  it('shows a malformed hop as its raw text instead of dropping it', () => {
    render(
      <HeaderAnalysisPanel
        analysis={analysis({
          hops: [hop(0, { from: null, by: null, with: null, raw: 'garbage received field' })],
        })}
      />,
    )

    expect(screen.getByText('garbage received field')).toBeInTheDocument()
  })

  it('displays DKIM signature fields', () => {
    render(
      <HeaderAnalysisPanel
        analysis={analysis({
          dkim: [
            {
              version: '1',
              algorithm: 'rsa-sha256',
              domain: 'example.com',
              selector: 'sel1',
              canonicalization: 'relaxed/relaxed',
              signed_headers: ['from', 'subject'],
              body_hash: 'abc123',
              timestamp: null,
              expiration: null,
              tags: [],
            },
          ],
        })}
      />,
    )

    expect(screen.getByText('example.com')).toBeInTheDocument()
    expect(screen.getByText('sel1')).toBeInTheDocument()
    expect(screen.getByText('rsa-sha256')).toBeInTheDocument()
    expect(screen.getByText('from, subject')).toBeInTheDocument()
  })

  it('attributes an authentication result to the server that wrote it', () => {
    render(
      <HeaderAnalysisPanel
        analysis={analysis({
          auth_results: [
            {
              authserv_id: 'mx.example.com',
              method: 'dkim',
              result: 'pass',
              properties: [{ name: 'header.d', value: 'example.com' }],
            },
          ],
        })}
      />,
    )

    // The speaker stays visible: "mx.example.com reported pass", never a bare
    // "pass" that would read as Sandesh's own finding.
    const claim = screen.getByText('mx.example.com').closest('.analysis__claim')
    expect(claim?.textContent).toContain('reported')
    expect(claim?.textContent).toContain('pass')
  })

  it('never claims a signature or policy was verified', () => {
    const { container } = render(
      <HeaderAnalysisPanel
        analysis={analysis({
          auth_results: [
            { authserv_id: 'mx.example.com', method: 'dkim', result: 'pass', properties: [] },
          ],
          spf: [{ result: 'pass', detail: 'client-ip=192.0.2.1' }],
        })}
      />,
    )

    // The disclaimer is the one place these words may appear, and it uses them
    // to deny the claim. Strip it, and nothing else may assert validity.
    const text = (container.textContent ?? '').replace(DISCLAIMER, '')
    // Whole words: "Authentication" is a section heading, not a claim.
    for (const claim of ['verified', 'trusted', 'is valid', 'authenticated']) {
      expect(text.toLowerCase()).not.toContain(claim)
    }
    expect(container.textContent).toContain(DISCLAIMER)
  })

  it('does not colour a result as success or failure', () => {
    // A green tick would be read as "my production mail will pass", which this
    // server has no basis to claim.
    const { container } = render(
      <HeaderAnalysisPanel
        analysis={analysis({
          auth_results: [
            { authserv_id: 'mx.example.com', method: 'spf', result: 'fail', properties: [] },
          ],
        })}
      />,
    )

    const result = container.querySelector('.analysis__claim-result')
    expect(result?.className).not.toMatch(/pass|fail|success|error|danger/)
  })

  it('surfaces spam headers without applying a threshold', () => {
    render(
      <HeaderAnalysisPanel
        analysis={analysis({ spam: [{ name: 'X-Spam-Status', value: 'No, score=-1.9' }] })}
      />,
    )

    expect(screen.getByText('X-Spam-Status')).toBeInTheDocument()
    expect(screen.getByText('No, score=-1.9')).toBeInTheDocument()
    expect(screen.getByText(/no threshold is applied/)).toBeInTheDocument()
  })
})

describe('formatDelay', () => {
  it('scales the unit to the magnitude', () => {
    expect(formatDelay(400)).toBe('+400µs')
    expect(formatDelay(15_000)).toBe('+15ms')
    expect(formatDelay(2_500_000)).toBe('+2.5s')
    expect(formatDelay(45_000_000)).toBe('+45s')
    expect(formatDelay(150_000_000)).toBe('+2m 30s')
  })

  it('signs a backwards gap', () => {
    expect(formatDelay(-5_000_000)).toBe('−5.0s')
  })
})
