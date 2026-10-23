/** Test helpers: render a component with the providers the app supplies. */

import type { ReactElement, ReactNode } from 'react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router-dom'
import { render } from '@testing-library/react'
import { ComposerProvider } from '../features/composer/ComposerContext'

import { DialogProvider } from '../context/DialogContext'

export function makeClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      // Retries turn a deliberate failure into a slow test that still passes.
      queries: { retry: false, gcTime: 0 },
      mutations: { retry: false },
    },
  })
}

export function Providers({
  children,
  client,
  route = '/',
}: {
  children: ReactNode
  client?: QueryClient
  route?: string
}) {
  return (
    <QueryClientProvider client={client ?? makeClient()}>
      <MemoryRouter initialEntries={[route]}>
        <DialogProvider>
          <ComposerProvider>{children}</ComposerProvider>
        </DialogProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

export function renderWithProviders(ui: ReactElement, options?: { route?: string }) {
  return render(ui, {
    wrapper: ({ children }) => (
      <Providers {...(options?.route ? { route: options.route } : {})}>{children}</Providers>
    ),
  })
}
