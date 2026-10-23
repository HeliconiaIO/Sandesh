import { useState } from 'react'
import { Download, Eye } from 'lucide-react'

import type { Attachment } from '../../api/client'
import { attachments as attachmentsApi } from '../../api/resources'
import { formatSize } from '../../lib/format'
import {
  AttachmentIcon,
  AttachmentPreviewModal,
  classifyAttachment,
} from './AttachmentPreviewModal'
import './AttachmentList.css'

export function AttachmentList({
  attachments,
  onInspect,
}: {
  messageId: string
  attachments: Attachment[]
  onInspect: (partPath: string) => void
}) {
  const [previewAttachment, setPreviewAttachment] = useState<Attachment | null>(null)
  const [thumbErrors, setThumbErrors] = useState<Record<string, boolean>>({})

  if (attachments.length === 0) {
    return <p className="attachments__empty">This message has no attachments.</p>
  }

  const handleThumbError = (id: string) => {
    setThumbErrors((prev) => ({ ...prev, [id]: true }))
  }

  return (
    <>
      <ul className="attachments">
        {attachments.map((attachment) => {
          const fileType = classifyAttachment(attachment)
          const isImage = fileType === 'image'
          const filename = attachment.filename ?? `part-${attachment.part_path}`
          const hasThumbError = thumbErrors[attachment.id]

          return (
            <li key={attachment.id} className="attachment">
              {/* Thumbnail or File Type Icon */}
              <div
                className={`attachment__preview-thumb ${
                  isImage ? 'attachment__preview-thumb--image' : ''
                }`}
                onClick={() => setPreviewAttachment(attachment)}
                title="Click to preview"
                role="button"
                tabIndex={0}
                onKeyDown={(e) => {
                  if (e.key === 'Enter' || e.key === ' ') {
                    e.preventDefault()
                    setPreviewAttachment(attachment)
                  }
                }}
              >
                {isImage && !hasThumbError ? (
                  <img
                    src={attachmentsApi.thumbnailUrl(attachment.id)}
                    alt=""
                    className="attachment__thumbnail-img"
                    loading="lazy"
                    onError={() => handleThumbError(attachment.id)}
                  />
                ) : (
                  <span className={`attachment__icon-badge attachment__icon-badge--${fileType}`}>
                    <AttachmentIcon fileType={fileType} size={18} />
                  </span>
                )}
              </div>

              {/* Main Metadata */}
              <div className="attachment__main">
                <span
                  className="attachment__name"
                  title={filename}
                  onClick={() => setPreviewAttachment(attachment)}
                  role="button"
                  tabIndex={0}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' || e.key === ' ') {
                      e.preventDefault()
                      setPreviewAttachment(attachment)
                    }
                  }}
                >
                  {attachment.filename ?? <em>unnamed part {attachment.part_path}</em>}
                </span>
                <span className="attachment__meta">
                  <code>{attachment.content_type}</code>
                  <span>•</span>
                  <span>{formatSize(attachment.size)}</span>
                  {attachment.is_inline ? <span className="chip">inline</span> : null}
                </span>
              </div>

              {/* Actions */}
              <div className="attachment__actions">
                <button
                  type="button"
                  className="button button--secondary attachment__preview-btn"
                  onClick={() => setPreviewAttachment(attachment)}
                  title="Preview attachment"
                >
                  <Eye size={13} aria-hidden="true" />
                  Preview
                </button>
                <button
                  type="button"
                  className="button"
                  onClick={() => onInspect(attachment.part_path)}
                  title="Inspect MIME part structure"
                >
                  Inspect
                </button>
                <a
                  className="button"
                  href={attachmentsApi.url(attachment.id, false)}
                  download={filename}
                  title="Download attachment"
                >
                  <Download size={13} aria-hidden="true" />
                  Download
                </a>
              </div>
            </li>
          )
        })}
      </ul>

      {previewAttachment && (
        <AttachmentPreviewModal
          attachment={previewAttachment}
          onClose={() => setPreviewAttachment(null)}
        />
      )}
    </>
  )
}
