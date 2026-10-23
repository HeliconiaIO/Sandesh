/**
 * Loading, empty and error states (§48–§50).
 *
 * Designed rather than incidental: "why is nothing here?" must always be
 * answerable from the screen. Each of these is a real component so a view
 * cannot accidentally render a blank pane while waiting.
 */

import type { ReactNode } from 'react'
import { AlertTriangle, Loader2 } from 'lucide-react'

import { ApiError } from '../api/client'
import './States.css'

export function LoadingState({ label = 'Loading' }: { label?: string }) {
  return (
    <div className="state" role="status" aria-live="polite">
      <Loader2 className="state__spinner" size={18} aria-hidden="true" />
      <p className="state__title">{label}</p>
    </div>
  )
}

export function EmptyState({
  title,
  children,
}: {
  title: string
  children?: ReactNode
}) {
  return (
    <div className="state">
      <p className="state__title">{title}</p>
      {children ? <div className="state__body">{children}</div> : null}
    </div>
  )
}

/**
 * A failed request, explained.
 *
 * Shows what failed and whether retrying is worth trying (§50). Internal detail
 * never reaches here: the backend already strips it at the error boundary, so
 * this renders the sanitised problem details and nothing else.
 */
export function ErrorState({
  error,
  onRetry,
}: {
  error: unknown
  onRetry?: () => void
}) {
  const api = error instanceof ApiError ? error : null
  const title = api?.message ?? 'Something went wrong'
  const detail =
    api?.detail ||
    (error instanceof Error ? error.message : 'The request failed for an unknown reason.')

  return (
    <div className="state" role="alert">
      <AlertTriangle className="state__icon state__icon--error" size={18} aria-hidden="true" />
      <p className="state__title">{title}</p>
      <p className="state__body">{detail}</p>
      {onRetry && (api === null || api.retryable) ? (
        <button type="button" className="button" onClick={onRetry}>
          Try again
        </button>
      ) : null}
    </div>
  )
}
