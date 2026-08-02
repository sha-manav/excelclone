/**
 * What the importer could not model.
 *
 * The importer already returns structured warnings; the point of showing them
 * is that "your charts are preserved but not editable" is something the user
 * has to learn on open, not on save. Nothing here is dismissable-and-forgotten
 * — the toolbar keeps a count so the drawer can be reopened.
 */

import type { JSX } from 'react'
import type { ImportWarning } from '../engine/bridge'

export interface WarningsDrawerProps {
  warnings: ImportWarning[]
  onClose(): void
}

/** Turn `UnsupportedFeature` into something a person would say. */
function kindLabel(kind: string): string {
  switch (kind) {
    case 'UnsupportedFeature':
      return 'Preserved, not editable'
    case 'UnparseableFormula':
      return 'Formula kept as text'
    case 'UnsupportedSheetType':
      return 'Sheet not modelled'
    default:
      return 'Note'
  }
}

export function WarningsDrawer(props: WarningsDrawerProps): JSX.Element {
  const { warnings, onClose } = props
  return (
    <aside className="drawer" role="dialog" aria-label="Import warnings">
      <header className="drawer__head">
        <h2>Import notes ({warnings.length})</h2>
        <button aria-label="Close import notes" onClick={onClose}>
          ×
        </button>
      </header>
      <p className="drawer__intro">
        Everything below survives a save untouched. Gridline does not model
        these features, so it will not change them either.
      </p>
      <ul className="drawer__list">
        {warnings.map((w, i) => (
          <li key={i}>
            <span className="drawer__kind">{kindLabel(w.kind)}</span>
            <span className="drawer__detail">{w.detail}</span>
          </li>
        ))}
        {warnings.length === 0 && <li className="muted">Nothing to report.</li>}
      </ul>
    </aside>
  )
}
