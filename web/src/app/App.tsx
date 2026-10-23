/**
 * Routing (§45, §58).
 *
 * The URL is the navigation state: a mailbox and an optional selected message.
 * Direct navigation to `/mailbox/x/message/y` works because the Rust server
 * serves the SPA entry point for non-API routes, so a refresh or a pasted link
 * lands where it should.
 */

import { Navigate, Route, Routes, useNavigate } from 'react-router-dom'
import { lazy, useEffect } from 'react'

import { AppShell } from './AppShell'
import { ErrorBoundary } from './ErrorBoundary'
import { OverviewRoute } from '../routes/OverviewRoute'
import { MailboxRoute } from '../routes/MailboxRoute'
import { useTheme } from '../hooks/useTheme'
import { EmptyState } from '../components/States'
import { ComposerProvider } from '../features/composer/ComposerContext'

/*
 * Route-level code splitting (§12).
 *
 * Overview and Mailbox stay in the entry chunk: one of them is what almost
 * every session opens first, so deferring them would only trade bundle size
 * for a blank frame on the most common path.
 *
 * The rest are diagnostics and configuration — reached deliberately, and not
 * at all in most sessions — so they are fetched when their route is entered.
 */
const SearchRoute = lazy(() =>
  import('../routes/SearchRoute').then((m) => ({ default: m.SearchRoute })),
)
const TracesRoute = lazy(() =>
  import('../routes/TracesRoute').then((m) => ({ default: m.TracesRoute })),
)
const GuideRoute = lazy(() =>
  import('../routes/GuideRoute').then((m) => ({ default: m.GuideRoute })),
)
const ApiRoute = lazy(() =>
  import('../routes/ApiRoute').then((m) => ({ default: m.ApiRoute })),
)
const LogsRoute = lazy(() =>
  import('../routes/LogsRoute').then((m) => ({ default: m.LogsRoute })),
)
const WebhooksRoute = lazy(() =>
  import('../routes/WebhooksRoute').then((m) => ({ default: m.WebhooksRoute })),
)
const TransferRoute = lazy(() =>
  import('../routes/TransferRoute').then((m) => ({ default: m.TransferRoute })),
)
const SettingsRoute = lazy(() =>
  import('../routes/SettingsRoute').then((m) => ({ default: m.SettingsRoute })),
)
const WorkspacesRoute = lazy(() =>
  import('../routes/WorkspacesRoute').then((m) => ({ default: m.WorkspacesRoute })),
)
const AboutRoute = lazy(() =>
  import('../routes/AboutRoute').then((m) => ({ default: m.AboutRoute })),
)

import { KeyboardShortcutsProvider } from '../context/KeyboardShortcutsContext'
import { DialogProvider } from '../context/DialogContext'

export function App() {
  // Mounted once so the stored preference is applied and kept in sync.
  useTheme()

  return (
    <ErrorBoundary>
      <DialogProvider>
        <KeyboardShortcutsProvider>
          <ComposerProvider>
            <Routes>
              <Route element={<AppShell />}>
                <Route index element={<OverviewRoute />} />
                <Route path="mailbox/:mailboxId" element={<MailboxRoute />} />
                <Route path="mailbox/:mailboxId/message/:messageId" element={<MailboxRoute />} />
                <Route path="search" element={<SearchRoute />} />
                <Route path="search/message/:messageId" element={<SearchRoute />} />
                <Route path="traces" element={<TracesRoute />} />
                <Route path="guide" element={<GuideRoute />} />
                <Route path="api" element={<ApiRoute />} />
                <Route path="logs" element={<LogsRoute />} />
                <Route path="webhooks" element={<WebhooksRoute />} />
                <Route path="transfer" element={<TransferRoute />} />
                <Route path="workspaces" element={<WorkspacesRoute />} />
                <Route path="settings" element={<SettingsRoute />} />
                <Route path="about" element={<AboutRoute />} />
                <Route path="*" element={<NotFound />} />
              </Route>
            </Routes>
          </ComposerProvider>
        </KeyboardShortcutsProvider>
      </DialogProvider>
    </ErrorBoundary>
  )
}

function NotFound() {
  const navigate = useNavigate()

  // A stale bookmark should not strand the user on a dead end.
  useEffect(() => {
    const timer = setTimeout(() => navigate('/', { replace: true }), 2500)
    return () => clearTimeout(timer)
  }, [navigate])

  return (
    <EmptyState title="That page does not exist">
      <p>Returning you to the overview.</p>
    </EmptyState>
  )
}

/** Kept for symmetry with the router's redirect target. */
export const HomeRedirect = () => <Navigate to="/" replace />
