import { useEffect, useState } from 'react'
import { EditorContent, useEditor } from '@tiptap/react'
import StarterKit from '@tiptap/starter-kit'
import {
  Bold,
  Code,
  CodeXml,
  Heading1,
  Heading2,
  Italic,
  Link as LinkIcon,
  List,
  ListOrdered,
  Minus,
  Quote,
  Redo,
  Strikethrough,
  Underline as UnderlineIcon,
  Undo,
  Unlink,
} from 'lucide-react'

import { useDialog } from '../../context/DialogContext'
import './RichTextEditor.css'

interface RichTextEditorProps {
  value: string
  onChange: (html: string) => void
  placeholder?: string
}

export function RichTextEditor({
  value,
  onChange,
  placeholder = 'Write message body here...',
}: RichTextEditorProps) {
  const [isSourceMode, setIsSourceMode] = useState(false)
  const { prompt } = useDialog()

  const editor = useEditor({
    extensions: [
      StarterKit.configure({
        heading: {
          levels: [1, 2, 3],
        },
        link: {
          openOnClick: false,
          HTMLAttributes: {
            target: '_blank',
            rel: 'noopener noreferrer',
          },
        },
      }),
    ],
    content: value || '<p></p>',
    onUpdate: ({ editor }) => {
      onChange(editor.getHTML())
    },
  })

  // Synchronize incoming value when changed externally (e.g. reply draft loaded)
  useEffect(() => {
    if (editor && !editor.isFocused && !isSourceMode) {
      const currentHtml = editor.getHTML()
      if (value !== currentHtml && (value || currentHtml !== '<p></p>')) {
        editor.commands.setContent(value || '<p></p>', { emitUpdate: false })
      }
    }
  }, [value, editor, isSourceMode])

  const toggleSourceMode = () => {
    if (!isSourceMode) {
      // Switching to Source view
      setIsSourceMode(true)
    } else {
      // Switching back to Visual view
      if (editor) {
        editor.commands.setContent(value || '<p></p>')
      }
      setIsSourceMode(false)
    }
  }

  const setLink = async () => {
    if (!editor) return
    const previousUrl = editor.getAttributes('link').href as string | undefined
    const url = await prompt({
      title: 'Insert Link',
      message: 'Enter destination web URL:',
      defaultValue: previousUrl || 'https://',
      placeholder: 'https://example.com',
      confirmText: 'Insert Link',
      cancelText: 'Cancel',
    })
    if (url === null) return
    if (url === '' || url === 'https://') {
      editor.chain().focus().extendMarkRange('link').unsetLink().run()
      return
    }
    editor.chain().focus().extendMarkRange('link').setLink({ href: url }).run()
  }

  return (
    <div className="rich-editor">
      <div className="rich-editor__toolbar" role="toolbar" aria-label="Formatting options">
        <div className="rich-editor__group">
          <button
            type="button"
            className="rich-editor__btn"
            onClick={() => editor?.chain().focus().undo().run()}
            disabled={!editor?.can().undo() || isSourceMode}
            title="Undo (Ctrl+Z)"
            aria-label="Undo"
          >
            <Undo size={14} aria-hidden="true" />
          </button>
          <button
            type="button"
            className="rich-editor__btn"
            onClick={() => editor?.chain().focus().redo().run()}
            disabled={!editor?.can().redo() || isSourceMode}
            title="Redo (Ctrl+Y)"
            aria-label="Redo"
          >
            <Redo size={14} aria-hidden="true" />
          </button>
        </div>

        <div className="rich-editor__divider" />

        <div className="rich-editor__group">
          <button
            type="button"
            className={`rich-editor__btn ${
              editor?.isActive('bold') ? 'rich-editor__btn--active' : ''
            }`}
            onClick={() => editor?.chain().focus().toggleBold().run()}
            disabled={isSourceMode}
            title="Bold (Ctrl+B)"
            aria-label="Bold"
          >
            <Bold size={14} aria-hidden="true" />
          </button>
          <button
            type="button"
            className={`rich-editor__btn ${
              editor?.isActive('italic') ? 'rich-editor__btn--active' : ''
            }`}
            onClick={() => editor?.chain().focus().toggleItalic().run()}
            disabled={isSourceMode}
            title="Italic (Ctrl+I)"
            aria-label="Italic"
          >
            <Italic size={14} aria-hidden="true" />
          </button>
          <button
            type="button"
            className={`rich-editor__btn ${
              editor?.isActive('underline') ? 'rich-editor__btn--active' : ''
            }`}
            onClick={() => editor?.chain().focus().toggleUnderline().run()}
            disabled={isSourceMode}
            title="Underline (Ctrl+U)"
            aria-label="Underline"
          >
            <UnderlineIcon size={14} aria-hidden="true" />
          </button>
          <button
            type="button"
            className={`rich-editor__btn ${
              editor?.isActive('strike') ? 'rich-editor__btn--active' : ''
            }`}
            onClick={() => editor?.chain().focus().toggleStrike().run()}
            disabled={isSourceMode}
            title="Strikethrough"
            aria-label="Strikethrough"
          >
            <Strikethrough size={14} aria-hidden="true" />
          </button>
          <button
            type="button"
            className={`rich-editor__btn ${
              editor?.isActive('code') ? 'rich-editor__btn--active' : ''
            }`}
            onClick={() => editor?.chain().focus().toggleCode().run()}
            disabled={isSourceMode}
            title="Inline Code"
            aria-label="Inline Code"
          >
            <Code size={14} aria-hidden="true" />
          </button>
        </div>

        <div className="rich-editor__divider" />

        <div className="rich-editor__group">
          <button
            type="button"
            className={`rich-editor__btn ${
              editor?.isActive('heading', { level: 1 }) ? 'rich-editor__btn--active' : ''
            }`}
            onClick={() => editor?.chain().focus().toggleHeading({ level: 1 }).run()}
            disabled={isSourceMode}
            title="Heading 1"
            aria-label="Heading 1"
          >
            <Heading1 size={14} aria-hidden="true" />
          </button>
          <button
            type="button"
            className={`rich-editor__btn ${
              editor?.isActive('heading', { level: 2 }) ? 'rich-editor__btn--active' : ''
            }`}
            onClick={() => editor?.chain().focus().toggleHeading({ level: 2 }).run()}
            disabled={isSourceMode}
            title="Heading 2"
            aria-label="Heading 2"
          >
            <Heading2 size={14} aria-hidden="true" />
          </button>
          <button
            type="button"
            className={`rich-editor__btn ${
              editor?.isActive('bulletList') ? 'rich-editor__btn--active' : ''
            }`}
            onClick={() => editor?.chain().focus().toggleBulletList().run()}
            disabled={isSourceMode}
            title="Bullet List"
            aria-label="Bullet List"
          >
            <List size={14} aria-hidden="true" />
          </button>
          <button
            type="button"
            className={`rich-editor__btn ${
              editor?.isActive('orderedList') ? 'rich-editor__btn--active' : ''
            }`}
            onClick={() => editor?.chain().focus().toggleOrderedList().run()}
            disabled={isSourceMode}
            title="Numbered List"
            aria-label="Numbered List"
          >
            <ListOrdered size={14} aria-hidden="true" />
          </button>
          <button
            type="button"
            className={`rich-editor__btn ${
              editor?.isActive('blockquote') ? 'rich-editor__btn--active' : ''
            }`}
            onClick={() => editor?.chain().focus().toggleBlockquote().run()}
            disabled={isSourceMode}
            title="Quote"
            aria-label="Quote"
          >
            <Quote size={14} aria-hidden="true" />
          </button>
          <button
            type="button"
            className="rich-editor__btn"
            onClick={() => editor?.chain().focus().setHorizontalRule().run()}
            disabled={isSourceMode}
            title="Horizontal Divider"
            aria-label="Horizontal Divider"
          >
            <Minus size={14} aria-hidden="true" />
          </button>
        </div>

        <div className="rich-editor__divider" />

        <div className="rich-editor__group">
          <button
            type="button"
            className={`rich-editor__btn ${
              editor?.isActive('link') ? 'rich-editor__btn--active' : ''
            }`}
            onClick={setLink}
            disabled={isSourceMode}
            title="Insert Link"
            aria-label="Insert Link"
          >
            <LinkIcon size={14} aria-hidden="true" />
          </button>
          {editor?.isActive('link') ? (
            <button
              type="button"
              className="rich-editor__btn"
              onClick={() => editor?.chain().focus().unsetLink().run()}
              disabled={isSourceMode}
              title="Remove Link"
              aria-label="Remove Link"
            >
              <Unlink size={14} aria-hidden="true" />
            </button>
          ) : null}
        </div>

        <div className="rich-editor__divider" />

        <div className="rich-editor__group" style={{ marginLeft: 'auto' }}>
          <button
            type="button"
            className={`rich-editor__btn ${
              isSourceMode ? 'rich-editor__btn--active' : ''
            }`}
            onClick={toggleSourceMode}
            title={isSourceMode ? 'Switch to Visual Editor' : 'Edit Raw HTML Source (< >)'}
            aria-label={isSourceMode ? 'Visual Editor' : 'HTML Source'}
          >
            <CodeXml size={15} aria-hidden="true" />
          </button>
        </div>
      </div>

      {isSourceMode ? (
        <textarea
          className="rich-editor__source"
          value={value}
          onChange={(e) => onChange(e.target.value)}
          placeholder="<p>Write HTML message body here...</p>"
          rows={10}
          aria-label="Raw HTML source"
        />
      ) : (
        <div
          className="rich-editor__content-wrapper"
          onClick={() => editor?.commands.focus()}
        >
          <EditorContent editor={editor} placeholder={placeholder} />
        </div>
      )}
    </div>
  )
}
