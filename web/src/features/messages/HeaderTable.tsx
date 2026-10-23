/**
 * The headers view (§20).
 *
 * Every header the backend returns, in received order, duplicates preserved.
 * Deliberately not a curated subset: a tool for debugging mail must show what
 * actually arrived, and the interesting header is usually the unexpected one.
 */

import { useState } from 'react'

import type { Header } from '../../api/client'
import { CopyButton } from '../../components/CopyButton'
import './HeaderTable.css'

export function HeaderTable({ headers }: { headers: Header[] }) {
  const [showRaw, setShowRaw] = useState(false)

  if (headers.length === 0) {
    return <p className="headers__empty">This message carries no headers.</p>
  }

  return (
    <div className="headers">
      <div className="headers__toolbar">
        <span className="headers__count">{headers.length} headers</span>
        <label className="headers__toggle">
          <input
            type="checkbox"
            checked={showRaw}
            onChange={(event) => setShowRaw(event.target.checked)}
          />
          Show raw values
        </label>
      </div>

      <table className="headers__table">
        <caption className="sr-only">
          Message headers in the order they were received
        </caption>
        <thead>
          <tr>
            <th scope="col">Name</th>
            <th scope="col">Value</th>
            <th scope="col">
              <span className="sr-only">Actions</span>
            </th>
          </tr>
        </thead>
        <tbody>
          {headers.map((header, index) => (
            // Duplicate header names are legal and meaningful (a `Received:`
            // chain is all duplicates), so the index is part of the key.
            <tr key={`${header.name}-${index}`}>
              <th scope="row" className="headers__name">
                {header.name}
              </th>
              <td className="headers__value">{showRaw ? header.raw_value : header.value}</td>
              <td className="headers__action">
                <CopyButton
                  value={`${header.name}: ${showRaw ? header.raw_value : header.value}`}
                  label={`the ${header.name} header`}
                />
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}
