/**
 * Multi-key sort over the selected range.
 *
 * Excel allows 64 keys; three covers every sort a person actually reasons
 * about, and the engine takes as many as it is given, so raising the limit is
 * a one-line change here rather than an engine change.
 */

import { useState } from 'react'
import type { JSX } from 'react'
import { colLetters, rangeA1 } from '../engine/actions'
import type { Range, SortKey } from '../engine/actions'

export interface SortDialogProps {
  range: Range
  onApply(keys: SortKey[], hasHeader: boolean): void
  onCancel(): void
}

const MAX_KEYS = 3

export function SortDialog(props: SortDialogProps): JSX.Element {
  const { range, onApply, onCancel } = props
  const columns: number[] = []
  for (let c = range.start.col; c <= range.end.col; c++) columns.push(c)

  const [hasHeader, setHasHeader] = useState(true)
  const [keys, setKeys] = useState<SortKey[]>([
    { column: range.start.col, ascending: true },
  ])

  const setKey = (i: number, patch: Partial<SortKey>) =>
    setKeys((prev) => prev.map((k, j) => (j === i ? { ...k, ...patch } : k)))

  return (
    <div className="modal-backdrop" role="presentation" onClick={onCancel}>
      <div
        className="modal"
        role="dialog"
        aria-modal="true"
        aria-label="Sort"
        onClick={(e) => e.stopPropagation()}
      >
        <h2>Sort {rangeA1(range)}</h2>
        <label className="checkline">
          <input
            type="checkbox"
            checked={hasHeader}
            onChange={(e) => setHasHeader(e.target.checked)}
          />
          My data has a header row
        </label>

        {keys.map((k, i) => (
          <div className="sort-key" key={i}>
            <span className="sort-key__label">{i === 0 ? 'Sort by' : 'Then by'}</span>
            <select
              aria-label={i === 0 ? 'Sort by column' : `Then by column ${i + 1}`}
              value={k.column}
              onChange={(e) => setKey(i, { column: Number(e.target.value) })}
            >
              {columns.map((c) => (
                <option key={c} value={c}>
                  Column {colLetters(c)}
                </option>
              ))}
            </select>
            <select
              aria-label={`Direction ${i + 1}`}
              value={k.ascending ? 'asc' : 'desc'}
              onChange={(e) => setKey(i, { ascending: e.target.value === 'asc' })}
            >
              <option value="asc">Ascending</option>
              <option value="desc">Descending</option>
            </select>
            {keys.length > 1 && (
              <button
                aria-label={`Remove sort key ${i + 1}`}
                onClick={() => setKeys((prev) => prev.filter((_, j) => j !== i))}
              >
                ×
              </button>
            )}
          </div>
        ))}

        {keys.length < MAX_KEYS && columns.length > keys.length && (
          <button
            className="linkish"
            onClick={() =>
              setKeys((prev) => [
                ...prev,
                {
                  column:
                    columns.find((c) => !prev.some((k) => k.column === c)) ??
                    range.start.col,
                  ascending: true,
                },
              ])
            }
          >
            Add another level
          </button>
        )}

        <div className="modal__actions">
          <button onClick={onCancel}>Cancel</button>
          <button className="primary" onClick={() => onApply(keys, hasHeader)}>
            Sort
          </button>
        </div>
      </div>
    </div>
  )
}
