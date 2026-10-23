import {
  createContext,
  lazy,
  Suspense,
  useContext,
  useState,
  type ReactNode,
} from 'react'
import type { ComposerInitialValues } from './MessageComposer'

/*
 * The composer is loaded on demand (§12).
 *
 * It is the single heaviest thing in the app: TipTap and ProseMirror are about
 * a megabyte of source between them, and they were in the initial bundle for
 * everyone — including the majority of sessions that only ever read mail. The
 * provider renders the composer unconditionally, so a static import meant
 * "always downloaded" even though the dialog is closed until someone presses
 * Compose.
 *
 * Splitting here rather than at the route is what actually moves the weight:
 * the composer is mounted by the provider that wraps every route, so no amount
 * of route splitting would have taken it out of the entry chunk.
 */
const MessageComposer = lazy(() =>
  import('./MessageComposer').then((m) => ({ default: m.MessageComposer })),
)

interface ComposerContextType {
  isOpen: boolean
  openComposer: (initialValues?: ComposerInitialValues) => void
  closeComposer: () => void
}

const ComposerContext = createContext<ComposerContextType | null>(null)

export function ComposerProvider({ children }: { children: ReactNode }) {
  const [isOpen, setIsOpen] = useState(false)
  const [initialValues, setInitialValues] = useState<ComposerInitialValues | undefined>(undefined)

  const openComposer = (values?: ComposerInitialValues) => {
    setInitialValues(values)
    setIsOpen(true)
  }

  const closeComposer = () => {
    setIsOpen(false)
    setInitialValues(undefined)
  }

  return (
    <ComposerContext.Provider value={{ isOpen, openComposer, closeComposer }}>
      {children}
      {/*
       * Mounted only once opened, so the chunk is requested on the first
       * Compose rather than on page load. There is no fallback UI because the
       * dialog has no placeholder to show: until it has loaded there is
       * nothing on screen, which is exactly the closed state.
       */}
      {isOpen ? (
        <Suspense fallback={null}>
          <MessageComposer
            isOpen={isOpen}
            initialValues={initialValues}
            onClose={closeComposer}
          />
        </Suspense>
      ) : null}
    </ComposerContext.Provider>
  )
}

export function useComposer() {
  const context = useContext(ComposerContext)
  if (!context) {
    throw new Error('useComposer must be used within a ComposerProvider')
  }
  return context
}
