import { describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { DialogProvider, useDialog } from './DialogContext'

function TestComponent({
  onResult,
}: {
  onResult?: (val: any) => void
}) {
  const { confirm, alert, prompt } = useDialog()

  return (
    <div>
      <button
        type="button"
        onClick={async () => {
          const res = await confirm({
            title: 'Discard draft?',
            message: 'You have unsaved changes.',
            confirmText: 'Discard',
            cancelText: 'Keep Editing',
            variant: 'danger',
          })
          onResult?.(res)
        }}
      >
        Trigger Confirm
      </button>

      <button
        type="button"
        onClick={async () => {
          await alert({
            title: 'Important Alert',
            message: 'System error occurred.',
          })
          onResult?.('alert-dismissed')
        }}
      >
        Trigger Alert
      </button>

      <button
        type="button"
        onClick={async () => {
          const res = await prompt({
            title: 'Enter URL',
            message: 'Please provide link URL:',
            defaultValue: 'https://example.com',
          })
          onResult?.(res)
        }}
      >
        Trigger Prompt
      </button>
    </div>
  )
}

describe('DialogContext & DialogModal', () => {
  it('opens confirmation modal and resolves true on confirm', async () => {
    const onResult = vi.fn()
    render(
      <DialogProvider>
        <TestComponent onResult={onResult} />
      </DialogProvider>,
    )

    fireEvent.click(screen.getByText('Trigger Confirm'))

    expect(screen.getByRole('alertdialog')).toBeInTheDocument()
    expect(screen.getByText('Discard draft?')).toBeInTheDocument()
    expect(screen.getByText('You have unsaved changes.')).toBeInTheDocument()

    const confirmBtn = screen.getByRole('button', { name: 'Discard' })
    fireEvent.click(confirmBtn)

    await waitFor(() => {
      expect(screen.queryByRole('alertdialog')).not.toBeInTheDocument()
    })
    expect(onResult).toHaveBeenCalledWith(true)
  })

  it('resolves false on cancel click', async () => {
    const onResult = vi.fn()
    render(
      <DialogProvider>
        <TestComponent onResult={onResult} />
      </DialogProvider>,
    )

    fireEvent.click(screen.getByText('Trigger Confirm'))

    const cancelBtn = screen.getByRole('button', { name: 'Keep Editing' })
    fireEvent.click(cancelBtn)

    await waitFor(() => {
      expect(screen.queryByRole('alertdialog')).not.toBeInTheDocument()
    })
    expect(onResult).toHaveBeenCalledWith(false)
  })

  it('resolves false on Escape key', async () => {
    const onResult = vi.fn()
    render(
      <DialogProvider>
        <TestComponent onResult={onResult} />
      </DialogProvider>,
    )

    fireEvent.click(screen.getByText('Trigger Confirm'))
    expect(screen.getByRole('alertdialog')).toBeInTheDocument()

    fireEvent.keyDown(window, { key: 'Escape' })

    await waitFor(() => {
      expect(screen.queryByRole('alertdialog')).not.toBeInTheDocument()
    })
    expect(onResult).toHaveBeenCalledWith(false)
  })

  it('renders alert dialog and resolves on OK click', async () => {
    const onResult = vi.fn()
    render(
      <DialogProvider>
        <TestComponent onResult={onResult} />
      </DialogProvider>,
    )

    fireEvent.click(screen.getByText('Trigger Alert'))

    expect(screen.getByRole('alertdialog')).toBeInTheDocument()
    expect(screen.getByText('Important Alert')).toBeInTheDocument()
    expect(screen.getByText('System error occurred.')).toBeInTheDocument()

    const okBtn = screen.getByRole('button', { name: 'OK' })
    fireEvent.click(okBtn)

    await waitFor(() => {
      expect(screen.queryByRole('alertdialog')).not.toBeInTheDocument()
    })
    expect(onResult).toHaveBeenCalledWith('alert-dismissed')
  })

  it('handles prompt dialog input submission and cancellation', async () => {
    const onResult = vi.fn()
    render(
      <DialogProvider>
        <TestComponent onResult={onResult} />
      </DialogProvider>,
    )

    fireEvent.click(screen.getByText('Trigger Prompt'))

    expect(screen.getByRole('alertdialog')).toBeInTheDocument()
    const input = screen.getByRole('textbox') as HTMLInputElement
    expect(input.value).toBe('https://example.com')

    fireEvent.change(input, { target: { value: 'https://sandesh.dev' } })
    const submitBtn = screen.getByRole('button', { name: 'OK' })
    fireEvent.click(submitBtn)

    await waitFor(() => {
      expect(screen.queryByRole('alertdialog')).not.toBeInTheDocument()
    })
    expect(onResult).toHaveBeenCalledWith('https://sandesh.dev')
  })
})
