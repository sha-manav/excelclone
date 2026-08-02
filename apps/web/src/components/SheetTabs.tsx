/**
 * Sheet tabs: switch, add, rename (double-click), and delete.
 */

import { useState } from 'react'
import type { SheetInfo } from '../engine/bridge'

export interface SheetTabsProps {
  sheets: SheetInfo[]
  active: string
  onSelect(name: string): void
  onAdd(): void
  onRename(from: string, to: string): void
  onDelete(name: string): void
}

export function SheetTabs({
  sheets,
  active,
  onSelect,
  onAdd,
  onRename,
  onDelete,
}: SheetTabsProps) {
  const [renaming, setRenaming] = useState<string | null>(null)
  const [draft, setDraft] = useState('')

  const commitRename = () => {
    if (renaming && draft.trim() && draft !== renaming) {
      onRename(renaming, draft.trim())
    }
    setRenaming(null)
  }

  return (
    <div className="sheet-tabs">
      <button className="sheet-tabs__add" onClick={onAdd} title="Add sheet">
        +
      </button>
      {sheets.map((s) => (
        <div
          key={s.name}
          className={`sheet-tab${s.name === active ? ' sheet-tab--active' : ''}`}
          onClick={() => onSelect(s.name)}
          onDoubleClick={() => {
            setRenaming(s.name)
            setDraft(s.name)
          }}
        >
          {renaming === s.name ? (
            <input
              className="sheet-tab__rename"
              value={draft}
              autoFocus
              spellCheck={false}
              onChange={(e) => setDraft(e.target.value)}
              onBlur={commitRename}
              onKeyDown={(e) => {
                if (e.key === 'Enter') commitRename()
                if (e.key === 'Escape') setRenaming(null)
              }}
            />
          ) : (
            <>
              <span className="sheet-tab__name">{s.name}</span>
              {sheets.length > 1 && (
                <button
                  className="sheet-tab__close"
                  title={`Delete ${s.name}`}
                  onClick={(e) => {
                    e.stopPropagation()
                    onDelete(s.name)
                  }}
                >
                  ×
                </button>
              )}
            </>
          )}
        </div>
      ))}
    </div>
  )
}
