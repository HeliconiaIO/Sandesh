import { describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'

import {
  AttachmentPreviewModal,
  classifyAttachment,
} from './AttachmentPreviewModal'
import { AttachmentList } from './AttachmentList'
import type { Attachment } from '../../api/client'

const sampleImage: Attachment = {
  id: 'att-1',
  part_path: '2',
  filename: 'screenshot.png',
  content_type: 'image/png',
  size: 10240,
  is_inline: false,
}

const samplePdf: Attachment = {
  id: 'att-2',
  part_path: '3',
  filename: 'invoice.pdf',
  content_type: 'application/pdf',
  size: 51200,
  is_inline: false,
}

const sampleText: Attachment = {
  id: 'att-3',
  part_path: '4',
  filename: 'notes.txt',
  content_type: 'text/plain',
  size: 256,
  is_inline: false,
}

const sampleArchive: Attachment = {
  id: 'att-4',
  part_path: '5',
  filename: 'data.zip',
  content_type: 'application/zip',
  size: 1048576,
  is_inline: false,
}

const sampleAudio: Attachment = {
  id: 'att-5',
  part_path: '6',
  filename: 'recording.mp3',
  content_type: 'audio/mpeg',
  size: 204800,
  is_inline: false,
}

const sampleVideo: Attachment = {
  id: 'att-6',
  part_path: '7',
  filename: 'demo.mp4',
  content_type: 'video/mp4',
  size: 2048000,
  is_inline: false,
}

const sampleJson: Attachment = {
  id: 'att-7',
  part_path: '8',
  filename: 'payload.json',
  content_type: 'application/json',
  size: 1024,
  is_inline: false,
}

describe('classifyAttachment', () => {
  it('identifies image types correctly', () => {
    expect(classifyAttachment(sampleImage)).toBe('image')
    expect(classifyAttachment({ ...sampleImage, filename: 'photo.jpg', content_type: 'image/jpeg' })).toBe('image')
    expect(classifyAttachment({ ...sampleImage, filename: 'icon.webp', content_type: 'application/octet-stream' })).toBe('image')
  })

  it('identifies PDF types correctly', () => {
    expect(classifyAttachment(samplePdf)).toBe('pdf')
    expect(classifyAttachment({ ...samplePdf, filename: 'doc.pdf', content_type: 'application/octet-stream' })).toBe('pdf')
  })

  it('identifies audio and video types correctly', () => {
    expect(classifyAttachment(sampleAudio)).toBe('audio')
    expect(classifyAttachment({ ...sampleAudio, filename: 'voice.wav', content_type: 'audio/wav' })).toBe('audio')
    expect(classifyAttachment(sampleVideo)).toBe('video')
    expect(classifyAttachment({ ...sampleVideo, filename: 'clip.webm', content_type: 'video/webm' })).toBe('video')
  })

  it('identifies text, code, json and spreadsheet files correctly', () => {
    expect(classifyAttachment(sampleText)).toBe('text')
    expect(classifyAttachment({ ...sampleText, filename: 'data.csv', content_type: 'text/csv' })).toBe('spreadsheet')
    expect(classifyAttachment(sampleJson)).toBe('json')
    expect(classifyAttachment({ ...sampleText, filename: 'script.ts', content_type: 'text/plain' })).toBe('code')
  })

  it('identifies archives and unsupported files correctly', () => {
    expect(classifyAttachment(sampleArchive)).toBe('archive')
    expect(classifyAttachment({ ...sampleArchive, filename: 'binary.bin', content_type: 'application/octet-stream' })).toBe('other')
  })
})

describe('AttachmentPreviewModal', () => {
  it('renders image preview modal with zoom controls and download link', () => {
    const handleClose = vi.fn()
    render(<AttachmentPreviewModal attachment={sampleImage} onClose={handleClose} />)

    expect(screen.getByRole('dialog', { name: /Preview: screenshot.png/i })).toBeInTheDocument()
    expect(screen.getByAltText('screenshot.png')).toBeInTheDocument()
    expect(screen.getByLabelText('Zoom In')).toBeInTheDocument()
    expect(screen.getByLabelText('Zoom Out')).toBeInTheDocument()

    // Test zoom in
    const zoomInBtn = screen.getByLabelText('Zoom In')
    fireEvent.click(zoomInBtn)
    expect(screen.getByText('125%')).toBeInTheDocument()

    // Test close button
    const closeBtn = screen.getByLabelText('Close preview')
    fireEvent.click(closeBtn)
    expect(handleClose).toHaveBeenCalled()
  })

  it('closes when pressing Escape', () => {
    const handleClose = vi.fn()
    render(<AttachmentPreviewModal attachment={sampleImage} onClose={handleClose} />)

    fireEvent.keyDown(window, { key: 'Escape' })
    expect(handleClose).toHaveBeenCalled()
  })

  it('renders PDF iframe for PDF attachments', () => {
    render(<AttachmentPreviewModal attachment={samplePdf} onClose={() => {}} />)
    const iframe = screen.getByTitle('PDF Preview: invoice.pdf')
    expect(iframe).toBeInTheDocument()
    expect(iframe.tagName.toLowerCase()).toBe('iframe')
  })

  it('fetches and displays text content for text attachments', async () => {
    const fetchSpy = vi.spyOn(window, 'fetch').mockResolvedValue({
      ok: true,
      status: 200,
      text: () => Promise.resolve('Hello world plain text attachment'),
    } as Response)

    render(<AttachmentPreviewModal attachment={sampleText} onClose={() => {}} />)

    await waitFor(() => {
      expect(screen.getByText('Hello world plain text attachment')).toBeInTheDocument()
    })

    fetchSpy.mockRestore()
  })

  it('renders audio player for audio attachments', () => {
    render(<AttachmentPreviewModal attachment={sampleAudio} onClose={() => {}} />)
    expect(screen.getByRole('dialog', { name: /Preview: recording.mp3/i })).toBeInTheDocument()
    const audioElement = document.querySelector('audio')
    expect(audioElement).toBeInTheDocument()
    expect(audioElement?.getAttribute('src')).toContain('/api/v1/attachments/att-5')
  })

  it('renders video player for video attachments', () => {
    render(<AttachmentPreviewModal attachment={sampleVideo} onClose={() => {}} />)
    expect(screen.getByRole('dialog', { name: /Preview: demo.mp4/i })).toBeInTheDocument()
    const videoElement = document.querySelector('video')
    expect(videoElement).toBeInTheDocument()
    expect(videoElement?.getAttribute('src')).toContain('/api/v1/attachments/att-6')
  })

  it('formats JSON and provides a copy button', async () => {
    const rawJson = '{"key":"value","count":42}'
    const fetchSpy = vi.spyOn(window, 'fetch').mockResolvedValue({
      ok: true,
      status: 200,
      text: () => Promise.resolve(rawJson),
    } as Response)

    Object.assign(navigator, {
      clipboard: {
        writeText: vi.fn().mockResolvedValue(undefined),
      },
    })

    render(<AttachmentPreviewModal attachment={sampleJson} onClose={() => {}} />)

    await waitFor(() => {
      expect(screen.getByText(/"key": "value"/)).toBeInTheDocument()
    })

    const copyBtn = screen.getByRole('button', { name: /Copy JSON/i })
    expect(copyBtn).toBeInTheDocument()
    fireEvent.click(copyBtn)

    expect(navigator.clipboard.writeText).toHaveBeenCalledWith(
      JSON.stringify({ key: 'value', count: 42 }, null, 2),
    )

    fetchSpy.mockRestore()
  })
})

describe('AttachmentList', () => {
  it('renders attachments with thumbnails, type badges, and preview buttons', () => {
    const handleInspect = vi.fn()
    render(
      <AttachmentList
        messageId="msg-1"
        attachments={[sampleImage, samplePdf, sampleArchive]}
        onInspect={handleInspect}
      />,
    )

    // Check attachment names
    expect(screen.getByText('screenshot.png')).toBeInTheDocument()
    expect(screen.getByText('invoice.pdf')).toBeInTheDocument()
    expect(screen.getByText('data.zip')).toBeInTheDocument()

    // Click inspect on invoice.pdf
    const inspectButtons = screen.getAllByText('Inspect')
    expect(inspectButtons[1]).toBeDefined()
    fireEvent.click(inspectButtons[1]!)
    expect(handleInspect).toHaveBeenCalledWith('3')

    // Click Preview on screenshot.png
    const previewButtons = screen.getAllByText('Preview')
    expect(previewButtons[0]).toBeDefined()
    fireEvent.click(previewButtons[0]!)

    // Verify modal is displayed
    expect(screen.getByRole('dialog', { name: /Preview: screenshot.png/i })).toBeInTheDocument()
  })
})
