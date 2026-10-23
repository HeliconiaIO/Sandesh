/**
 * The MIME inspector (§65).
 *
 * The tree is reconstructed from a flat list, so the test that matters is that
 * nesting follows the backend's `children` rather than any client-side guess.
 */

import { describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen } from '@testing-library/react'

import { MimeDetail, MimeTree } from './MimeTree'
import { mimeTree } from '../../test/fixtures'

describe('MimeTree', () => {
  it('renders every part in the tree', () => {
    render(<MimeTree tree={mimeTree()} selectedPath={null} onSelect={() => {}} />)

    expect(screen.getByText('multipart/alternative')).toBeInTheDocument()
    expect(screen.getByText('text/plain')).toBeInTheDocument()
    expect(screen.getByText('text/html')).toBeInTheDocument()
  })

  it('nests children under their parent', () => {
    render(<MimeTree tree={mimeTree()} selectedPath={null} onSelect={() => {}} />)

    const items = screen.getAllByRole('treeitem')
    // The root sits at level 1 and its two children at level 2.
    expect(items[0]).toHaveAttribute('aria-level', '1')
    expect(items[1]).toHaveAttribute('aria-level', '2')
    expect(items[2]).toHaveAttribute('aria-level', '2')
  })

  it('reports the selected part by its path', () => {
    const onSelect = vi.fn()
    render(<MimeTree tree={mimeTree()} selectedPath={null} onSelect={onSelect} />)

    fireEvent.click(screen.getByText('text/html'))
    expect(onSelect).toHaveBeenCalledWith('2')
  })

  it('marks the selected node for assistive technology', () => {
    render(<MimeTree tree={mimeTree()} selectedPath="1" onSelect={() => {}} />)
    expect(screen.getByRole('treeitem', { selected: true })).toHaveTextContent('text/plain')
  })

  it('explains a message with no MIME structure', () => {
    render(
      <MimeTree
        tree={{ ...mimeTree(), parts: [] }}
        selectedPath={null}
        onSelect={() => {}}
      />,
    )
    expect(screen.getByText(/no MIME structure/i)).toBeInTheDocument()
  })

  it('shows the byte range that links a part to the raw source', () => {
    // The signature feature depends on this range being surfaced accurately.
    const part = mimeTree().parts[1]!
    render(<MimeDetail part={part} />)

    expect(screen.getByText('Byte range')).toBeInTheDocument()
    expect(screen.getByText('120–184')).toBeInTheDocument()
  })
})
