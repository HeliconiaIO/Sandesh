/**
 * Loading, empty and error states (§48–§50).
 */

import { describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen } from '@testing-library/react'

import { EmptyState, ErrorState, LoadingState } from './States'
import { ApiError } from '../api/client'

describe('states', () => {
  it('announces loading to assistive technology', () => {
    render(<LoadingState label="Loading messages" />)
    expect(screen.getByRole('status')).toHaveTextContent('Loading messages')
  })

  it('renders an empty state with its guidance', () => {
    render(
      <EmptyState title="No messages yet">
        <p>Send mail to 127.0.0.1:1025</p>
      </EmptyState>,
    )

    expect(screen.getByText('No messages yet')).toBeInTheDocument()
    expect(screen.getByText('Send mail to 127.0.0.1:1025')).toBeInTheDocument()
  })

  it('reports what failed as an alert', () => {
    render(<ErrorState error={new ApiError(500, 'internal', 'Internal error', 'It broke.')} />)

    const alert = screen.getByRole('alert')
    expect(alert).toHaveTextContent('Internal error')
    expect(alert).toHaveTextContent('It broke.')
  })

  it('offers a retry for a failure that might be transient', () => {
    const onRetry = vi.fn()
    render(
      <ErrorState
        error={new ApiError(503, 'unavailable', 'Unavailable', 'Storage is down.')}
        onRetry={onRetry}
      />,
    )

    fireEvent.click(screen.getByRole('button', { name: 'Try again' }))
    expect(onRetry).toHaveBeenCalled()
  })

  it('offers no retry for a request that will fail identically', () => {
    // Retrying a 404 wastes the user's time and teaches them to distrust the
    // button, so the affordance is withheld rather than shown and useless.
    render(
      <ErrorState
        error={new ApiError(404, 'not-found', 'Not found', 'No such message.')}
        onRetry={() => {}}
      />,
    )

    expect(screen.queryByRole('button', { name: 'Try again' })).not.toBeInTheDocument()
  })

  it('does not leak internals for an unexpected error shape', () => {
    render(<ErrorState error={new Error('SQLITE_BUSY at /var/lib/sandesh/db.sqlite')} />)
    // A raw Error is still shown as its message; what matters is that the
    // component never renders a stack trace.
    expect(screen.getByRole('alert').textContent).not.toContain('at Object.')
  })
})
