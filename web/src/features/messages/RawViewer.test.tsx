/**
 * The raw source viewer (§65).
 *
 * Raw bytes are authoritative (ADR-003), so the properties worth testing are
 * that the viewer does not alter them and that the byte range it highlights
 * matches what the MIME tree reported — the linkage the product is built on.
 */

import { describe, expect, it } from 'vitest'
import { render, screen } from '@testing-library/react'

import { RawViewer } from './RawViewer'

const MESSAGE = ['Subject: Hello', 'From: a@test', '', 'Body line one', 'Body line two'].join('\n')

describe('RawViewer', () => {
  it('shows every line of the source', () => {
    render(<RawViewer raw={MESSAGE} />)

    expect(screen.getByText('Subject: Hello')).toBeInTheDocument()
    expect(screen.getByText('Body line two')).toBeInTheDocument()
  })

  it('numbers the lines', () => {
    render(<RawViewer raw={MESSAGE} />)

    expect(screen.getByText('1')).toBeInTheDocument()
    expect(screen.getByText('5')).toBeInTheDocument()
  })

  it('reports the true byte length, not the character count', () => {
    // A multi-byte character must not make the reported size disagree with the
    // offsets the backend computed over bytes.
    render(<RawViewer raw={'Subject: café'} />)
    expect(screen.getByText(/14 bytes/)).toBeInTheDocument()
  })

  it('does not rewrite CRLF line endings into the displayed text', () => {
    render(<RawViewer raw={'A: 1\r\nB: 2'} />)

    // The carriage return is stripped for display only; the line content
    // itself is preserved exactly.
    expect(screen.getByText('A: 1')).toBeInTheDocument()
    expect(screen.getByText('B: 2')).toBeInTheDocument()
  })

  it('highlights the lines covered by a selected part', () => {
    const { container } = render(
      // Bytes 0-14 cover the first line only.
      <RawViewer raw={MESSAGE} highlight={{ offset: 0, length: 14 }} />,
    )

    const highlighted = container.querySelectorAll('[data-highlighted="true"]')
    expect(highlighted).toHaveLength(1)
    expect(highlighted[0]).toHaveTextContent('Subject: Hello')
  })

  it('highlights a range spanning several lines', () => {
    const { container } = render(
      <RawViewer raw={MESSAGE} highlight={{ offset: 30, length: 20 }} />,
    )

    expect(container.querySelectorAll('[data-highlighted="true"]').length).toBeGreaterThan(1)
  })

  it('includes the line a range starts exactly at', () => {
    // Boundary precision is the whole point of the MIME-to-raw linkage: a part
    // whose body begins at the first byte of a line must highlight that line,
    // not the one before or after it. `Subject: Hello\n` is bytes 0-14, so a
    // part starting at 15 belongs to line 2 alone.
    const { container } = render(
      <RawViewer raw={MESSAGE} highlight={{ offset: 15, length: 12 }} />,
    )

    const highlighted = container.querySelectorAll('[data-highlighted="true"]')
    expect(highlighted).toHaveLength(1)
    expect(highlighted[0]).toHaveTextContent('From: a@test')
  })

  it('includes a zero-length part at its exact offset', () => {
    // An empty body part still has a location, and the viewer must be able to
    // point at it rather than silently highlighting nothing.
    const { container } = render(
      <RawViewer raw={MESSAGE} highlight={{ offset: 15, length: 0 }} />,
    )

    const highlighted = container.querySelectorAll('[data-highlighted="true"]')
    expect(highlighted.length).toBeGreaterThan(0)
    expect(highlighted[0]).toHaveTextContent('From: a@test')
  })

  it('highlights nothing when no part is selected', () => {
    const { container } = render(<RawViewer raw={MESSAGE} />)
    expect(container.querySelectorAll('[data-highlighted="true"]')).toHaveLength(0)
  })

  it('announces which byte range is shown', () => {
    render(<RawViewer raw={MESSAGE} highlight={{ offset: 15, length: 13 }} />)
    expect(screen.getByText('Showing bytes 15–28')).toBeInTheDocument()
  })
})
