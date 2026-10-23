import {
  createContext,
  useCallback,
  useContext,
  useRef,
  useState,
  type ReactNode,
} from 'react'
import { DialogModal, type DialogVariant } from '../components/DialogModal'

export interface ConfirmOptions {
  title?: string
  message: ReactNode
  confirmText?: string
  cancelText?: string
  variant?: DialogVariant
}

export interface AlertOptions {
  title?: string
  message: ReactNode
  okText?: string
  variant?: DialogVariant
}

export interface PromptOptions {
  title?: string
  message?: ReactNode
  defaultValue?: string
  placeholder?: string
  confirmText?: string
  cancelText?: string
}

interface DialogState {
  isOpen: boolean
  type: 'confirm' | 'alert' | 'prompt'
  title?: string
  message: ReactNode
  confirmText?: string
  cancelText?: string
  variant?: DialogVariant
  defaultValue?: string
  placeholder?: string
}

export interface DialogContextType {
  confirm: (options: ConfirmOptions | string) => Promise<boolean>
  alert: (options: AlertOptions | string) => Promise<void>
  prompt: (options: PromptOptions | string, defaultValue?: string) => Promise<string | null>
}

const DialogContext = createContext<DialogContextType | null>(null)

export function DialogProvider({ children }: { children: ReactNode }) {
  const [dialogState, setDialogState] = useState<DialogState>({
    isOpen: false,
    type: 'confirm',
    message: '',
  })

  // Store promise resolver
  // A dialog resolves to boolean (confirm), void (alert), or string | null (prompt);
  // which one depends on the dialog type currently open. The ref erases that, so
  // the handlers below resolve with the value matching the open dialog's type.
  type DialogResolver =
    | { type: 'confirm'; resolve: (value: boolean) => void }
    | { type: 'alert'; resolve: () => void }
    | { type: 'prompt'; resolve: (value: string | null) => void }
  const resolverRef = useRef<DialogResolver | null>(null)

  const confirm = useCallback((options: ConfirmOptions | string): Promise<boolean> => {
    return new Promise<boolean>((resolve) => {
      resolverRef.current = { type: 'confirm', resolve }

      if (typeof options === 'string') {
        setDialogState({
          isOpen: true,
          type: 'confirm',
          title: 'Confirm Action',
          message: options,
          confirmText: 'Confirm',
          cancelText: 'Cancel',
          variant: 'default',
        })
      } else {
        setDialogState({
          isOpen: true,
          type: 'confirm',
          title: options.title ?? 'Confirm Action',
          message: options.message,
          confirmText: options.confirmText ?? 'Confirm',
          cancelText: options.cancelText ?? 'Cancel',
          variant: options.variant ?? 'default',
        })
      }
    })
  }, [])

  const alert = useCallback((options: AlertOptions | string): Promise<void> => {
    return new Promise<void>((resolve) => {
      resolverRef.current = { type: 'alert', resolve: () => resolve() }

      if (typeof options === 'string') {
        setDialogState({
          isOpen: true,
          type: 'alert',
          title: 'Notice',
          message: options,
          confirmText: 'OK',
          variant: 'info',
        })
      } else {
        setDialogState({
          isOpen: true,
          type: 'alert',
          title: options.title ?? 'Notice',
          message: options.message,
          confirmText: options.okText ?? 'OK',
          variant: options.variant ?? 'info',
        })
      }
    })
  }, [])

  const prompt = useCallback(
    (options: PromptOptions | string, defaultVal?: string): Promise<string | null> => {
      return new Promise<string | null>((resolve) => {
        resolverRef.current = { type: 'prompt', resolve }

        if (typeof options === 'string') {
          setDialogState({
            isOpen: true,
            type: 'prompt',
            title: options,
            message: '',
            defaultValue: defaultVal ?? '',
            confirmText: 'OK',
            cancelText: 'Cancel',
          })
        } else {
          setDialogState({
            isOpen: true,
            type: 'prompt',
            title: options.title ?? 'Input Required',
            message: options.message ?? '',
            defaultValue: options.defaultValue ?? '',
            placeholder: options.placeholder ?? '',
            confirmText: options.confirmText ?? 'OK',
            cancelText: options.cancelText ?? 'Cancel',
          })
        }
      })
    },
    [],
  )

  const handleConfirm = useCallback((inputValue?: string) => {
    setDialogState((prev) => ({ ...prev, isOpen: false }))
    const pending = resolverRef.current
    if (pending) {
      resolverRef.current = null
      if (pending.type === 'prompt') {
        pending.resolve(inputValue ?? '')
      } else if (pending.type === 'confirm') {
        pending.resolve(true)
      } else {
        pending.resolve()
      }
    }
  }, [])

  const handleCancel = useCallback(() => {
    setDialogState((prev) => ({ ...prev, isOpen: false }))
    const pending = resolverRef.current
    if (pending) {
      resolverRef.current = null
      if (pending.type === 'prompt') {
        pending.resolve(null)
      } else if (pending.type === 'confirm') {
        pending.resolve(false)
      } else {
        pending.resolve()
      }
    }
  }, [])

  return (
    <DialogContext.Provider value={{ confirm, alert, prompt }}>
      {children}
      <DialogModal
        isOpen={dialogState.isOpen}
        type={dialogState.type}
        title={dialogState.title}
        message={dialogState.message}
        confirmText={dialogState.confirmText}
        cancelText={dialogState.cancelText}
        variant={dialogState.variant}
        defaultValue={dialogState.defaultValue}
        placeholder={dialogState.placeholder}
        onConfirm={handleConfirm}
        onCancel={handleCancel}
      />
    </DialogContext.Provider>
  )
}

export function useDialog(): DialogContextType {
  const context = useContext(DialogContext)
  if (!context) {
    throw new Error('useDialog must be used within a DialogProvider')
  }
  return context
}
