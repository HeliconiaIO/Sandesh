import { useEffect, useRef, useState } from 'react'
import { Link } from 'react-router-dom'
import { Check, ChevronDown, Database, ExternalLink, Layers, Lock, Users } from 'lucide-react'

import { useActivateWorkspace, useWorkspaces } from '../hooks/queries'
import './WorkspaceSwitcher.css'

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}

export function WorkspaceSwitcher() {
  const { data: workspacesList, isLoading } = useWorkspaces()
  const activateWorkspace = useActivateWorkspace()
  const [open, setOpen] = useState(false)
  const containerRef = useRef<HTMLDivElement>(null)

  const workspaces = workspacesList ?? []
  const activeWorkspace = workspaces.find((w) => w.is_active) ?? workspaces[0]

  // Close dropdown on outside click
  useEffect(() => {
    if (!open) return undefined

    const handleClickOutside = (event: MouseEvent) => {
      if (
        containerRef.current &&
        !containerRef.current.contains(event.target as Node)
      ) {
        setOpen(false)
      }
    }

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        setOpen(false)
      }
    }

    document.addEventListener('mousedown', handleClickOutside)
    document.addEventListener('keydown', handleKeyDown)
    return () => {
      document.removeEventListener('mousedown', handleClickOutside)
      document.removeEventListener('keydown', handleKeyDown)
    }
  }, [open])

  const handleSelectWorkspace = async (id: string) => {
    if (activeWorkspace && activeWorkspace.id === id) {
      setOpen(false)
      return
    }

    try {
      await activateWorkspace.mutateAsync(id)
      setOpen(false)
    } catch (err) {
      console.error('Failed to switch workspace:', err)
    }
  }

  const activeName = activeWorkspace?.name || 'default'

  return (
    <div className="workspace-switcher" ref={containerRef}>
      <button
        type="button"
        className="workspace-switcher__trigger"
        onClick={() => setOpen((prev) => !prev)}
        aria-expanded={open}
        aria-haspopup="true"
        title={`Active workspace: ${activeName} (Click to switch)`}
      >
        <Database size={13} className="workspace-switcher__icon" aria-hidden="true" />
        <span className="workspace-switcher__label">DB:</span>
        <span className="workspace-switcher__name">
          {isLoading && !activeWorkspace ? 'Loading...' : activeName}
        </span>
        <ChevronDown size={12} className="workspace-switcher__chevron" aria-hidden="true" />
      </button>

      {open && (
        <div className="workspace-switcher__dropdown" role="menu">
          <div className="workspace-switcher__header">
            <span className="workspace-switcher__heading">Workspaces ({workspaces.length})</span>
          </div>

          <ul className="workspace-switcher__list" role="none">
            {workspaces.map((ws) => {
              const isActive = ws.is_active
              return (
                <li key={ws.id} role="none">
                  <button
                    type="button"
                    role="menuitem"
                    className={`workspace-switcher__item-btn${
                      isActive ? ' workspace-switcher__item-btn--active' : ''
                    }`}
                    onClick={() => void handleSelectWorkspace(ws.id)}
                    disabled={activateWorkspace.isPending}
                  >
                    <div className="workspace-switcher__item-info">
                      <div style={{ display: 'flex', alignItems: 'center', gap: '5px' }}>
                        <span className="workspace-switcher__item-title">
                          {ws.name}
                        </span>
                        {ws.auth_required ? (
                          <span title="Authentication required" style={{ display: 'inline-flex' }}>
                            <Lock size={10} style={{ color: 'var(--warning)', flexShrink: 0 }} aria-hidden="true" />
                          </span>
                        ) : null}
                      </div>
                      <span className="workspace-switcher__item-meta">
                        {ws.message_count} {ws.message_count === 1 ? 'mail' : 'mails'} • {formatBytes(ws.size_bytes)}
                      </span>
                    </div>

                    {isActive ? (
                      <span className="workspace-switcher__active-badge">
                        <Check size={11} aria-hidden="true" />
                        <span>Active</span>
                      </span>
                    ) : null}
                  </button>
                </li>
              )
            })}
          </ul>

          <div className="workspace-switcher__footer">
            <Link
              to="/workspaces"
              className="workspace-switcher__manage-link"
              onClick={() => setOpen(false)}
            >
              <Layers size={13} aria-hidden="true" />
              <span>Manage all workspaces</span>
              <ExternalLink size={11} aria-hidden="true" className="workspace-switcher__link-arrow" />
            </Link>
            <Link
              to="/settings?tab=accounts"
              className="workspace-switcher__manage-link"
              onClick={() => setOpen(false)}
            >
              <Users size={13} aria-hidden="true" />
              <span>Accounts &amp; Credentials</span>
              <ExternalLink size={11} aria-hidden="true" className="workspace-switcher__link-arrow" />
            </Link>
          </div>
        </div>
      )}
    </div>
  )
}
