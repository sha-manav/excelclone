/**
 * Find and replace.
 *
 * Finding is a read-only query into the engine and changes nothing; replacing
 * is a single `find_replace` action, so Replace All is one undo step and the
 * captured log records one gesture rather than N cell edits.
 *
 * Both sides call the same engine matcher, so the highlighted cells are
 * exactly the cells Replace All would rewrite. A matcher reimplemented here
 * would eventually disagree, and the disagreement would surface as a
 * replacement the user did not see coming.
 */

import { useEffect, useState } from 'react'
import type { JSX } from 'react'

export interface FindReplacePanelProps {
  /** A1 addresses currently matching, in reading order. */
  matches: string[]
  /** Index into `matches` of the one the selection is sitting on. */
  current: number
  onSearch(find: string, matchCase: boolean, wholeCell: boolean): void
  onStep(delta: number): void
  onReplaceAll(replace: string): void
  onClose(): void
}

export function FindReplacePanel(props: FindReplacePanelProps): JSX.Element {
  const { matches, current, onSearch, onStep, onReplaceAll, onClose } = props
  const [find, setFind] = useState('')
  const [replace, setReplace] = useState('')
  const [matchCase, setMatchCase] = useState(false)
  const [wholeCell, setWholeCell] = useState(false)

  // Re-run the search whenever the term or the options change, so the count
  // and the highlights can never describe an older query than the one shown.
  useEffect(() => {
    onSearch(find, matchCase, wholeCell)
  }, [find, matchCase, wholeCell, onSearch])

  return (
    <div className="find-panel" role="search" aria-label="Find and replace">
      <input
        aria-label="Find"
        autoFocus
        placeholder="Find"
        value={find}
        onChange={(e) => setFind(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter') {
            e.preventDefault()
            onStep(e.shiftKey ? -1 : 1)
          }
          if (e.key === 'Escape') onClose()
        }}
      />
      <input
        aria-label="Replace with"
        placeholder="Replace with"
        value={replace}
        onChange={(e) => setReplace(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Escape') onClose()
        }}
      />
      <span className="find-panel__count" aria-live="polite">
        {find === ''
          ? ''
          : matches.length === 0
            ? 'No matches'
            : `${current + 1} of ${matches.length}`}
      </span>
      <button
        aria-label="Previous match"
        disabled={matches.length === 0}
        onClick={() => onStep(-1)}
      >
        ↑
      </button>
      <button
        aria-label="Next match"
        disabled={matches.length === 0}
        onClick={() => onStep(1)}
      >
        ↓
      </button>
      <label className="checkline">
        <input
          type="checkbox"
          checked={matchCase}
          onChange={(e) => setMatchCase(e.target.checked)}
        />
        Match case
      </label>
      <label className="checkline">
        <input
          type="checkbox"
          checked={wholeCell}
          onChange={(e) => setWholeCell(e.target.checked)}
        />
        Whole cell
      </label>
      <button
        disabled={matches.length === 0}
        title="Replace every match in one undoable step"
        onClick={() => onReplaceAll(replace)}
      >
        Replace all
      </button>
      <button aria-label="Close find and replace" onClick={onClose}>
        ×
      </button>
    </div>
  )
}
