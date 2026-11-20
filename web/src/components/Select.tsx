import {
  useCallback,
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactNode,
} from 'react'
import { Check, ChevronDown } from 'lucide-react'
import './Select.css'

export interface SelectOption {
  value: string
  label: string
  disabled?: boolean
  icon?: ReactNode
}

export interface SelectGroup {
  label: string
  options: SelectOption[]
}

export type SelectItem = SelectOption | SelectGroup

function isGroup(item: SelectItem): item is SelectGroup {
  return 'options' in item && Array.isArray(item.options)
}

export interface SelectProps {
  id?: string
  name?: string
  value: string
  onChange: (value: string) => void
  options: SelectItem[]
  placeholder?: string
  disabled?: boolean
  className?: string
  size?: 'sm' | 'md'
  fullWidth?: boolean
  'aria-label'?: string
  'aria-labelledby'?: string
}

export function Select({
  id: propId,
  name,
  value,
  onChange,
  options,
  placeholder = 'Select option...',
  disabled = false,
  className = '',
  size = 'md',
  fullWidth = false,
  'aria-label': ariaLabel,
  'aria-labelledby': ariaLabelledby,
}: SelectProps) {
  const generatedId = useId()
  const id = propId ?? generatedId
  const [isOpen, setIsOpen] = useState(false)
  const [highlightedIndex, setHighlightedIndex] = useState(-1)
  const containerRef = useRef<HTMLDivElement>(null)
  const triggerRef = useRef<HTMLButtonElement>(null)
  const dropdownRef = useRef<HTMLDivElement>(null)

  // Flatten options for easy indexing
  const flatOptions = useMemo<SelectOption[]>(() => {
    const list: SelectOption[] = []
    for (const item of options) {
      if (isGroup(item)) {
        list.push(...item.options)
      } else {
        list.push(item)
      }
    }
    return list
  }, [options])

  const selectedOption = useMemo(
    () => flatOptions.find((opt) => opt.value === value),
    [flatOptions, value],
  )

  // Close on outside click
  useEffect(() => {
    if (!isOpen) return undefined

    const handleClickOutside = (event: MouseEvent) => {
      if (containerRef.current && !containerRef.current.contains(event.target as Node)) {
        setIsOpen(false)
      }
    }

    document.addEventListener('mousedown', handleClickOutside)
    return () => {
      document.removeEventListener('mousedown', handleClickOutside)
    }
  }, [isOpen])

  // Reset highlight index when opening
  useEffect(() => {
    if (isOpen) {
      const idx = flatOptions.findIndex((opt) => opt.value === value)
      setHighlightedIndex(idx >= 0 ? idx : 0)
    }
  }, [isOpen, flatOptions, value])

  const handleSelect = useCallback(
    (val: string) => {
      onChange(val)
      setIsOpen(false)
      triggerRef.current?.focus()
    },
    [onChange],
  )

  const handleKeyDown = (e: KeyboardEvent<HTMLButtonElement>) => {
    if (disabled) return

    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault()
      if (!isOpen) {
        setIsOpen(true)
        return
      }
      const direction = e.key === 'ArrowDown' ? 1 : -1
      let nextIndex = highlightedIndex + direction
      while (nextIndex >= 0 && nextIndex < flatOptions.length && flatOptions[nextIndex]?.disabled) {
        nextIndex += direction
      }
      if (nextIndex >= 0 && nextIndex < flatOptions.length) {
        setHighlightedIndex(nextIndex)
      }
    } else if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault()
      if (!isOpen) {
        setIsOpen(true)
      } else if (highlightedIndex >= 0 && flatOptions[highlightedIndex]) {
        const opt = flatOptions[highlightedIndex]
        if (!opt.disabled) {
          handleSelect(opt.value)
        }
      }
    } else if (e.key === 'Escape' && isOpen) {
      e.preventDefault()
      setIsOpen(false)
      triggerRef.current?.focus()
    } else if (e.key === 'Tab' && isOpen) {
      setIsOpen(false)
    }
  }

  return (
    <div
      className={`select-container ${fullWidth ? 'select-container--full' : ''} ${className}`}
      ref={containerRef}
    >
      <button
        ref={triggerRef}
        type="button"
        id={`${id}-trigger`}
        className={`select-trigger ${size === 'sm' ? 'select-trigger--sm' : ''} ${
          isOpen ? 'select-trigger--open' : ''
        }`}
        disabled={disabled}
        onClick={() => setIsOpen((prev) => !prev)}
        onKeyDown={handleKeyDown}
        aria-haspopup="listbox"
        aria-expanded={isOpen}
        aria-labelledby={ariaLabelledby}
      >
        <span className="select-trigger__label">
          {selectedOption ? selectedOption.label : placeholder}
        </span>
        <ChevronDown size={14} className="select-trigger__chevron" aria-hidden="true" />
      </button>

      {isOpen && (
        <div
          ref={dropdownRef}
          className="select-dropdown"
          role="listbox"
          tabIndex={-1}
          aria-labelledby={ariaLabelledby ?? `${id}-trigger`}
        >
          {options.map((item, idx) => {
            if (isGroup(item)) {
              return (
                <div key={item.label || idx} role="group" aria-label={item.label}>
                  <div className="select-group-header">{item.label}</div>
                  {item.options.map((opt) => {
                    const isSelected = opt.value === value
                    const optIndex = flatOptions.indexOf(opt)
                    return (
                      <button
                        key={opt.value}
                        type="button"
                        role="option"
                        aria-selected={isSelected}
                        disabled={opt.disabled}
                        className={`select-option ${
                          isSelected ? 'select-option--active' : ''
                        } ${optIndex === highlightedIndex ? 'select-option--highlighted' : ''}`}
                        onClick={() => handleSelect(opt.value)}
                        onMouseEnter={() => setHighlightedIndex(optIndex)}
                      >
                        <span className="select-option__label">{opt.label}</span>
                        {isSelected && (
                          <Check size={14} className="select-option__check" aria-hidden="true" />
                        )}
                      </button>
                    )
                  })}
                </div>
              )
            }

            const isSelected = item.value === value
            const optIndex = flatOptions.indexOf(item)
            return (
              <button
                key={item.value}
                type="button"
                role="option"
                aria-selected={isSelected}
                disabled={item.disabled}
                className={`select-option ${
                  isSelected ? 'select-option--active' : ''
                } ${optIndex === highlightedIndex ? 'select-option--highlighted' : ''}`}
                onClick={() => handleSelect(item.value)}
                onMouseEnter={() => setHighlightedIndex(optIndex)}
              >
                <span className="select-option__label">{item.label}</span>
                {isSelected && (
                  <Check size={14} className="select-option__check" aria-hidden="true" />
                )}
              </button>
            )
          })}
        </div>
      )}

      {/* Accessible native select for testing-library, screen readers, and form bindings */}
      <select
        id={id}
        name={name}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        tabIndex={-1}
        aria-label={ariaLabel}
        style={{
          position: 'absolute',
          width: '1px',
          height: '1px',
          padding: 0,
          margin: '-1px',
          overflow: 'hidden',
          clip: 'rect(0, 0, 0, 0)',
          whiteSpace: 'nowrap',
          border: 0,
          opacity: 0,
          pointerEvents: 'none',
        }}
      >
        {options.map((item, idx) => {
          if (isGroup(item)) {
            return (
              <optgroup key={item.label || idx} label={item.label}>
                {item.options.map((opt) => (
                  <option key={opt.value} value={opt.value} disabled={opt.disabled}>
                    {opt.label}
                  </option>
                ))}
              </optgroup>
            )
          }
          return (
            <option key={item.value} value={item.value} disabled={item.disabled}>
              {item.label}
            </option>
          )
        })}
      </select>
    </div>
  )
}
