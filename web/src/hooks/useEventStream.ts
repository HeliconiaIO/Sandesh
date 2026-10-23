/**
 * Binds the event stream to the query cache.
 *
 * When mail arrives, the affected queries are invalidated rather than the
 * application reloaded (§27). React Query then refetches only what is actually
 * mounted, so an event for a mailbox nobody is looking at costs nothing.
 *
 * Arrival order is deliberately not treated as authoritative (§28): every
 * handler invalidates rather than applying a delta, so two frames arriving out
 * of order converge on the same server state instead of racing.
 */

import { useEffect, useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'

import { type ConnectionState, type EventFrame, eventStream } from '../api/events'
import { keys } from './queries'

/** Connects on mount and reports the live connection state. */
export function useConnectionState(): ConnectionState {
  const [state, setState] = useState<ConnectionState>(eventStream.connectionState)

  useEffect(() => {
    eventStream.connect()
    return eventStream.onStateChange(setState)
  }, [])

  return state
}

/**
 * Keeps cached data current as events arrive.
 *
 * Mounted once, at the application root.
 */
export function useEventSync(): void {
  const client = useQueryClient()

  useEffect(() => {
    const refreshLists = () => {
      // Counts live on the mailbox, and the list itself changed.
      void client.invalidateQueries({ queryKey: keys.mailboxes() })
      void client.invalidateQueries({ queryKey: keys.messages() })
    }

    const unsubscribe = eventStream.subscribe(({ event }: EventFrame) => {
      switch (event.kind) {
        case 'MessageReceived':
        case 'EntryCreated':
        case 'EntryDeleted':
        case 'EntryMoved':
        case 'MessageDeleted':
          refreshLists()
          break

        case 'EntryFlagsChanged': {
          // Unread counts change, and the row's own flags with them. The open
          // message is refreshed too, so its flag controls stay truthful.
          refreshLists()
          const messageId = event.payload['message_id']
          if (typeof messageId === 'string') {
            void client.invalidateQueries({ queryKey: keys.message(messageId) })
          }
          break
        }

        case 'MailboxCreated':
        case 'MailboxDeleted':
        case 'MailboxRenamed':
        case 'MailboxSubscriptionChanged':
          void client.invalidateQueries({ queryKey: keys.mailboxes() })
          break

        case 'AccountCreated':
        case 'AccountDeleted':
          void client.invalidateQueries({ queryKey: keys.accounts() })
          void client.invalidateQueries({ queryKey: keys.mailboxes() })
          break

        case 'DeliveryFailed':
          // Nothing cached represents a failed delivery yet; the diagnostics
          // view reads it on demand.
          break

        case 'WorkspaceCreated':
        case 'WorkspaceDeleted':
          void client.invalidateQueries({ queryKey: keys.workspaces() })
          break

        case 'WorkspaceSwitched':
          // Switching the active workspace changes the entire backing database:
          // invalidate all queries so the application immediately reflects the
          // new active workspace.
          void client.invalidateQueries()
          break

        case 'WorkspacePurged':
          void client.invalidateQueries({ queryKey: keys.workspaces() })
          void client.invalidateQueries({ queryKey: keys.messages() })
          void client.invalidateQueries({ queryKey: keys.mailboxes() })
          void client.invalidateQueries({ queryKey: keys.accounts() })
          void client.invalidateQueries({ queryKey: keys.connections() })
          void client.invalidateQueries({ queryKey: keys.webhooks() })
          void client.invalidateQueries({ queryKey: keys.outbox() })
          break
      }
    })

    // A lagged connection missed events, so every cached list is suspect.
    const unsubscribeLag = eventStream.onLag(() => {
      void client.invalidateQueries()
    })

    return () => {
      unsubscribe()
      unsubscribeLag()
    }
  }, [client])
}
