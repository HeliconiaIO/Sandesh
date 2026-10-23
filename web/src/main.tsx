/**
 * Application entry point.
 *
 * Wires the three things that must exist before any view renders: the query
 * client (server state), the router (navigation state) and the shell.
 */

import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { BrowserRouter } from 'react-router-dom'

import { App } from './app/App'
import { ApiError } from './api/client'
import './styles/base.css'

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // The WebSocket already tells us when data changed, so polling would be
      // redundant traffic against a local server.
      refetchOnWindowFocus: false,
      staleTime: 5_000,
      retry: (failureCount, error) => {
        // A 404 or a malformed request will fail identically however many
        // times it is repeated; only retry what might genuinely be transient.
        if (error instanceof ApiError && !error.retryable) return false
        return failureCount < 2
      },
    },
  },
})

const container = document.getElementById('root')

if (!container) {
  throw new Error('The #root element is missing from index.html')
}

createRoot(container).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <BrowserRouter>
        <App />
      </BrowserRouter>
    </QueryClientProvider>
  </StrictMode>,
)
