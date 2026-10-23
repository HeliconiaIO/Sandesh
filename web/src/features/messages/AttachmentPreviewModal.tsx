import { useEffect, useMemo, useState } from 'react'
import {
  Check,
  Copy,
  Download,
  ExternalLink,
  File,
  FileArchive,
  FileCode,
  FileSpreadsheet,
  FileText,
  Image as ImageIcon,
  RotateCcw,
  Video,
  Volume2,
  X,
  ZoomIn,
  ZoomOut,
} from 'lucide-react'

import type { Attachment } from '../../api/client'
import { attachments as attachmentsApi } from '../../api/resources'
import { formatSize } from '../../lib/format'
import './AttachmentPreviewModal.css'

export type AttachmentFileType =
  | 'image'
  | 'pdf'
  | 'audio'
  | 'video'
  | 'json'
  | 'text'
  | 'code'
  | 'spreadsheet'
  | 'archive'
  | 'document'
  | 'other'

export function classifyAttachment(attachment: Attachment): AttachmentFileType {
  const mime = attachment.content_type.toLowerCase()
  const name = (attachment.filename ?? '').toLowerCase()

  if (
    mime.startsWith('image/') ||
    /\.(png|jpe?g|gif|webp|bmp|ico)$/.test(name)
  ) {
    return 'image'
  }
  if (mime === 'application/pdf' || name.endsWith('.pdf')) {
    return 'pdf'
  }
  if (
    mime.startsWith('audio/') ||
    /\.(mp3|wav|ogg|aac|m4a|flac)$/.test(name)
  ) {
    return 'audio'
  }
  if (
    mime.startsWith('video/') ||
    /\.(mp4|webm|mov|mkv|avi)$/.test(name)
  ) {
    return 'video'
  }
  if (
    mime === 'application/json' ||
    name.endsWith('.json')
  ) {
    return 'json'
  }
  if (
    mime.startsWith('text/csv') ||
    mime.includes('spreadsheet') ||
    /\.(csv|tsv|xlsx?)$/.test(name)
  ) {
    return 'spreadsheet'
  }
  if (
    mime.includes('xml') ||
    mime.includes('yaml') ||
    mime.includes('javascript') ||
    mime.includes('typescript') ||
    /\.(xml|ya?ml|js|jsx|ts|tsx|rs|py|go|sh|css|scss|sql|html?)$/.test(name)
  ) {
    return 'code'
  }
  if (
    mime.startsWith('text/') ||
    /\.(txt|md|markdown|log|diff|patch)$/.test(name)
  ) {
    return 'text'
  }
  if (
    mime.includes('zip') ||
    mime.includes('compressed') ||
    mime.includes('tar') ||
    mime.includes('archive') ||
    /\.(zip|tar|gz|7z|rar|bz2)$/.test(name)
  ) {
    return 'archive'
  }
  if (
    mime.includes('word') ||
    mime.includes('document') ||
    /\.(docx?|rtf|odt)$/.test(name)
  ) {
    return 'document'
  }
  return 'other'
}

export function AttachmentIcon({
  fileType,
  size = 16,
  className = '',
}: {
  fileType: AttachmentFileType
  size?: number
  className?: string
}) {
  switch (fileType) {
    case 'image':
      return <ImageIcon size={size} className={className} aria-hidden="true" />
    case 'pdf':
      return <FileText size={size} className={className} aria-hidden="true" />
    case 'audio':
      return <Volume2 size={size} className={className} aria-hidden="true" />
    case 'video':
      return <Video size={size} className={className} aria-hidden="true" />
    case 'json':
    case 'code':
      return <FileCode size={size} className={className} aria-hidden="true" />
    case 'spreadsheet':
      return <FileSpreadsheet size={size} className={className} aria-hidden="true" />
    case 'text':
      return <FileText size={size} className={className} aria-hidden="true" />
    case 'archive':
      return <FileArchive size={size} className={className} aria-hidden="true" />
    case 'document':
      return <FileText size={size} className={className} aria-hidden="true" />
    default:
      return <File size={size} className={className} aria-hidden="true" />
  }
}

export function AttachmentPreviewModal({
  attachment,
  onClose,
}: {
  attachment: Attachment
  onClose: () => void
}) {
  const [scale, setScale] = useState(1)
  const [textContent, setTextContent] = useState<string | null>(null)
  const [textLoading, setTextLoading] = useState(false)
  const [textError, setTextError] = useState<string | null>(null)
  const [copied, setCopied] = useState(false)

  const fileType = useMemo(() => classifyAttachment(attachment), [attachment])
  const previewUrl = useMemo(
    () => attachmentsApi.url(attachment.id, true),
    [attachment.id],
  )
  const downloadUrl = useMemo(
    () => attachmentsApi.url(attachment.id, false),
    [attachment.id],
  )
  const filename = attachment.filename ?? `part-${attachment.part_path}`

  // Close on Escape key
  useEffect(() => {
    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === 'Escape') {
        event.stopPropagation()
        onClose()
      }
    }
    window.addEventListener('keydown', handleKeyDown)
    return () => window.removeEventListener('keydown', handleKeyDown)
  }, [onClose])

  // Fetch text/code/json content if applicable
  useEffect(() => {
    if (
      fileType === 'text' ||
      fileType === 'code' ||
      fileType === 'json' ||
      fileType === 'spreadsheet'
    ) {
      let cancelled = false
      setTextLoading(true)
      setTextError(null)

      fetch(previewUrl)
        .then(async (res) => {
          if (!res.ok) {
            throw new Error(`Failed to load preview (${res.status} ${res.statusText})`)
          }
          return res.text()
        })
        .then((text) => {
          if (!cancelled) {
            setTextContent(text)
            setTextLoading(false)
          }
        })
        .catch((err: unknown) => {
          if (!cancelled) {
            setTextError(err instanceof Error ? err.message : 'Failed to load content')
            setTextLoading(false)
          }
        })

      return () => {
        cancelled = true
      }
    }
  }, [fileType, previewUrl])

  const formattedContent = useMemo(() => {
    if (fileType === 'json' && textContent) {
      try {
        return JSON.stringify(JSON.parse(textContent), null, 2)
      } catch {
        return textContent
      }
    }
    return textContent
  }, [fileType, textContent])

  const handleCopy = () => {
    const textToCopy = formattedContent ?? textContent
    if (textToCopy) {
      void navigator.clipboard.writeText(textToCopy)
      setCopied(true)
      setTimeout(() => setCopied(false), 2000)
    }
  }

  const handleZoomIn = () => setScale((s) => Math.min(4, s + 0.25))
  const handleZoomOut = () => setScale((s) => Math.max(0.25, s - 0.25))
  const handleResetZoom = () => setScale(1)

  const isTextual =
    fileType === 'text' ||
    fileType === 'code' ||
    fileType === 'json' ||
    fileType === 'spreadsheet'

  return (
    <div
      className="attachment-modal__backdrop"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose()
      }}
      role="dialog"
      aria-modal="true"
      aria-label={`Preview: ${filename}`}
    >
      <div className="attachment-modal__container">
        {/* Header */}
        <header className="attachment-modal__header">
          <div className="attachment-modal__header-left">
            <span className={`attachment-modal__type-badge attachment-modal__type-badge--${fileType}`}>
              <AttachmentIcon fileType={fileType} size={15} />
            </span>
            <div className="attachment-modal__title-box">
              <h2 className="attachment-modal__filename" title={filename}>
                {filename}
              </h2>
              <div className="attachment-modal__meta">
                <span>{formatSize(attachment.size)}</span>
                <span className="attachment-modal__bullet">•</span>
                <code>{attachment.content_type}</code>
                {attachment.is_inline ? (
                  <>
                    <span className="attachment-modal__bullet">•</span>
                    <span className="chip">inline</span>
                  </>
                ) : null}
              </div>
            </div>
          </div>

          <div className="attachment-modal__header-actions">
            {fileType === 'image' ? (
              <div className="attachment-modal__zoom-controls">
                <button
                  type="button"
                  className="button button--ghost button--icon"
                  onClick={handleZoomOut}
                  title="Zoom Out"
                  aria-label="Zoom Out"
                >
                  <ZoomOut size={15} />
                </button>
                <span className="attachment-modal__zoom-level">
                  {Math.round(scale * 100)}%
                </span>
                <button
                  type="button"
                  className="button button--ghost button--icon"
                  onClick={handleZoomIn}
                  title="Zoom In"
                  aria-label="Zoom In"
                >
                  <ZoomIn size={15} />
                </button>
                <button
                  type="button"
                  className="button button--ghost button--icon"
                  onClick={handleResetZoom}
                  title="Reset Zoom"
                  aria-label="Reset Zoom"
                >
                  <RotateCcw size={14} />
                </button>
              </div>
            ) : null}

            {isTextual && formattedContent && !textLoading ? (
              <button
                type="button"
                className="button button--ghost"
                onClick={handleCopy}
                title={copied ? 'Copied to clipboard' : 'Copy content'}
                aria-label={copied ? 'Copied' : fileType === 'json' ? 'Copy JSON' : 'Copy'}
              >
                {copied ? <Check size={14} aria-hidden="true" /> : <Copy size={14} aria-hidden="true" />}
                <span>{copied ? 'Copied' : fileType === 'json' ? 'Copy JSON' : 'Copy'}</span>
              </button>
            ) : null}

            <a
              href={previewUrl}
              target="_blank"
              rel="noopener noreferrer"
              className="button button--ghost"
              title="Open in new tab"
            >
              <ExternalLink size={14} aria-hidden="true" />
              Open
            </a>

            <a
              href={downloadUrl}
              download={filename}
              className="button button--primary"
              title="Download file"
            >
              <Download size={14} aria-hidden="true" />
              Download
            </a>

            <button
              type="button"
              className="button button--ghost button--icon attachment-modal__close-btn"
              onClick={onClose}
              title="Close preview (Esc)"
              aria-label="Close preview"
            >
              <X size={18} />
            </button>
          </div>
        </header>

        {/* Body content */}
        <main className="attachment-modal__body">
          {fileType === 'image' && (
            <div className="attachment-modal__image-wrapper">
              <img
                src={previewUrl}
                alt={filename}
                className="attachment-modal__image"
                style={{ transform: `scale(${scale})` }}
                draggable={false}
              />
            </div>
          )}

          {fileType === 'pdf' && (
            <div className="attachment-modal__frame-wrapper">
              <iframe
                src={previewUrl}
                title={`PDF Preview: ${filename}`}
                className="attachment-modal__pdf-frame"
              />
            </div>
          )}

          {fileType === 'audio' && (
            <div className="attachment-modal__media-wrapper">
              <div className="attachment-modal__media-card">
                <Volume2 size={48} className="attachment-modal__media-icon" aria-hidden="true" />
                <div className="attachment-modal__media-title">{filename}</div>
                <audio controls src={previewUrl} className="attachment-modal__audio-player">
                  Your browser does not support audio playback.
                </audio>
              </div>
            </div>
          )}

          {fileType === 'video' && (
            <div className="attachment-modal__media-wrapper">
              <video controls src={previewUrl} className="attachment-modal__video-player">
                Your browser does not support video playback.
              </video>
            </div>
          )}

          {isTextual && (
            <div className="attachment-modal__text-wrapper">
              {textLoading ? (
                <div className="attachment-modal__status">Loading content...</div>
              ) : textError ? (
                <div className="attachment-modal__status attachment-modal__status--error">
                  {textError}
                </div>
              ) : (
                <pre className="attachment-modal__text-content">
                  <code>{formattedContent}</code>
                </pre>
              )}
            </div>
          )}

          {(fileType === 'archive' || fileType === 'document' || fileType === 'other') && (
            <div className="attachment-modal__unsupported">
              <div className="attachment-modal__unsupported-icon">
                <AttachmentIcon fileType={fileType} size={48} />
              </div>
              <h3 className="attachment-modal__unsupported-title">{filename}</h3>
              <p className="attachment-modal__unsupported-desc">
                This file format ({attachment.content_type}) cannot be previewed inline in the
                browser. Download the file to view it with your system's native application.
              </p>
              <a
                href={downloadUrl}
                download={filename}
                className="button button--primary attachment-modal__unsupported-btn"
              >
                <Download size={15} aria-hidden="true" />
                Download {formatSize(attachment.size)}
              </a>
            </div>
          )}
        </main>
      </div>
    </div>
  )
}
