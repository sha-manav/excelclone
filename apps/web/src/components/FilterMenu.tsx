/**
 * The checkbox filter menu for one column.
 *
 * The engine's `FilterSpec` is an allowed-value list, so this is deliberately
 * a checkbox list and not a predicate builder: the UI can only express what
 * the engine can replay, which is what keeps a recorded filter reproducible.
 */

import { useMemo, useState } from 'react'
import type { JSX } from 'react'
import { colLetters, rangeA1 } from '../engine/actions'
import type { Range } from '../engine/actions'

export interface FilterMenuProps {
  range: Range
  column: number
  /** Every distinct display value in the column, header excluded. */
  values: string[]
  /** Values currently visible; null when no filter is active. */
  allowed: string[] | null
  onApply(allowed: string[]): void
  onClear(): void
  onCancel(): void
}

export function FilterMenu(props: FilterMenuProps): JSX.Element {
  const { range, column, values, allowed, onApply, onClear, onCancel } = props
  const [checked, setChecked] = useState<Set<string>>(
    () => new Set(allowed ?? values),
  )
  const [query, setQuery] = useState('')

  const shown = useMemo(() => {
    const q = query.trim().toLowerCase()
    return q ? values.filter((v) => v.toLowerCase().includes(q)) : values
  }, [values, query])

  const toggle = (v: string) =>
    setChecked((prev) => {
      const next = new Set(prev)
      if (next.has(v)) next.delete(v)
      else next.add(v)
      return next
    })

  return (
    <div className="modal-backdrop" role="presentation" onClick={onCancel}>
      <div
        className="modal"
        role="dialog"
        aria-modal="true"
        aria-label="Filter"
        onClick={(e) => e.stopPropagation()}
      >
        <h2>
          Filter column {colLetters(column)} of {rangeA1(range)}
        </h2>
        <input
          aria-label="Search values"
          placeholder="Search values"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
        <div className="filter-actions">
          <button className="linkish" onClick={() => setChecked(new Set(values))}>
            Select all
          </button>
          <button className="linkish" onClick={() => setChecked(new Set())}>
            Select none
          </button>
        </div>
        <ul className="filter-values">
          {shown.map((v) => (
            <li key={v}>
              <label>
                <input
                  type="checkbox"
                  checked={checked.has(v)}
                  onChange={() => toggle(v)}
                />
                {/* An empty cell still needs something clickable. */}
                {v === '' ? <em>(blank)</em> : v}
              </label>
            </li>
          ))}
          {shown.length === 0 && <li className="muted">No matching values</li>}
        </ul>
        <div className="modal__actions">
          <button onClick={onClear}>Clear filter</button>
          <button onClick={onCancel}>Cancel</button>
          <button
            className="primary"
            disabled={checked.size === 0}
            title={
              checked.size === 0
                ? 'Hiding every row would leave nothing to look at'
                : undefined
            }
            onClick={() => onApply([...checked])}
          >
            Apply
          </button>
        </div>
      </div>
    </div>
  )
}
