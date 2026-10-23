/**
 * The last line of defence for a rendering failure (§84).
 *
 * A crash in one view must not leave a blank page with no way forward. The
 * stack is logged to the console for a developer to read; it is deliberately
 * not rendered, because a stack trace in the UI is neither actionable nor
 * something to show by default (§50).
 */

import { Component, type ErrorInfo, type ReactNode } from 'react'

interface Props {
  children: ReactNode
}

interface State {
  error: Error | null
}

export class ErrorBoundary extends Component<Props, State> {
  override state: State = { error: null }

  static getDerivedStateFromError(error: Error): State {
    return { error }
  }

  override componentDidCatch(error: Error, info: ErrorInfo): void {
    console.error('Sandesh UI error', error, info.componentStack)
  }

  override render(): ReactNode {
    if (!this.state.error) {
      return this.props.children
    }

    return (
      <div className="state" role="alert">
        <p className="state__title">The interface hit an unexpected error</p>
        <p className="state__body">
          Your mail is unaffected — this is a fault in the console itself. Reload to
          continue; the details are in the browser console.
        </p>
        <button type="button" className="button" onClick={() => window.location.reload()}>
          Reload
        </button>
      </div>
    )
  }
}
