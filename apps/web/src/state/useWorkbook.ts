/**
 * Workbook state for the React tree.
 *
 * Holds the engine handle, the current selection, and the edit-in-progress.
 * Spreadsheet data itself is never mirrored here — the grid asks the engine
 * for the cells it is about to paint. `version` bumps on every applied action
 * so views know to re-read.
 */

import { useCallback, useEffect, useRef, useState } from 'react'
import { EngineHandle, type ImportWarning, type SheetInfo } from '../engine/bridge'
import {
  addr as mkAddr,
  range as mkRange,
  singleRange,
  type Action,
  type Addr,
  type Range,
} from '../engine/actions'

export interface Selection {
  /** The cell the keyboard acts from; stays put while the range grows. */
  anchor: Addr
  range: Range
}

export interface EditState {
  addr: Addr
  value: string
  /** True when typing replaced the cell rather than opening it with F2. */
  replacing: boolean
}

export type MoveDirection = 'down' | 'right' | 'up' | 'left' | 'none'

export interface WorkbookApi {
  engine: EngineHandle | null
  ready: boolean
  sheets: SheetInfo[]
  activeSheet: string
  selection: Selection
  editing: EditState | null
  version: number
  error: string | null
  warnings: ImportWarning[]
  canUndo: boolean
  canRedo: boolean

  apply: (action: Action) => boolean
  applyBatch: (actions: Action[]) => boolean
  setActiveSheet: (name: string) => void
  select: (sel: Selection) => void
  selectCell: (a: Addr) => void
  moveSelection: (dir: MoveDirection, extend: boolean) => void
  startEdit: (a: Addr, initial?: string) => void
  updateEdit: (value: string) => void
  commitEdit: (move: MoveDirection) => void
  cancelEdit: () => void
  dismissError: () => void
  setWarnings: (w: ImportWarning[]) => void
}

const MAX_ROW = 1_048_575
const MAX_COL = 16_383

export function useWorkbook(): WorkbookApi {
  const engineRef = useRef<EngineHandle | null>(null)
  const [ready, setReady] = useState(false)
  const [sheets, setSheets] = useState<SheetInfo[]>([])
  const [activeSheet, setActiveSheetState] = useState('Sheet1')
  const [selection, setSelection] = useState<Selection>({
    anchor: mkAddr(0, 0),
    range: singleRange(mkAddr(0, 0)),
  })
  const [editing, setEditing] = useState<EditState | null>(null)
  // Mirrors `editing` so commit can read it without doing work inside a
  // state updater — React may invoke updaters more than once, which would
  // apply the edit (and emit its events) twice.
  const editingRef = useRef<EditState | null>(null)
  editingRef.current = editing
  const [version, setVersion] = useState(0)
  const [error, setError] = useState<string | null>(null)
  const [warnings, setWarnings] = useState<ImportWarning[]>([])
  const [undoRedo, setUndoRedo] = useState({ undo: false, redo: false })

  useEffect(() => {
    let cancelled = false
    EngineHandle.create()
      .then((handle) => {
        if (cancelled) return
        engineRef.current = handle
        setSheets(handle.sheets())
        setReady(true)
      })
      .catch((e) => setError(String(e)))
    return () => {
      cancelled = true
    }
  }, [])

  const refresh = useCallback(() => {
    const engine = engineRef.current
    if (!engine) return
    setSheets(engine.sheets())
    setUndoRedo({ undo: engine.canUndo(), redo: engine.canRedo() })
    setVersion((v) => v + 1)
  }, [])

  const apply = useCallback(
    (action: Action): boolean => {
      const engine = engineRef.current
      if (!engine) return false
      try {
        engine.apply(action)
        refresh()
        return true
      } catch (e) {
        setError(String(e))
        return false
      }
    },
    [refresh],
  )

  const applyBatch = useCallback(
    (actions: Action[]): boolean => {
      const engine = engineRef.current
      if (!engine || actions.length === 0) return false
      try {
        engine.applyBatch(actions)
        refresh()
        return true
      } catch (e) {
        setError(String(e))
        return false
      }
    },
    [refresh],
  )

  // Keep the active sheet valid when sheets are added, renamed, or deleted.
  useEffect(() => {
    if (sheets.length === 0) return
    if (!sheets.some((s) => s.name === activeSheet)) {
      setActiveSheetState(sheets[0].name)
    }
  }, [sheets, activeSheet])

  const setActiveSheet = useCallback((name: string) => {
    setActiveSheetState(name)
    setEditing(null)
    setSelection({ anchor: mkAddr(0, 0), range: singleRange(mkAddr(0, 0)) })
  }, [])

  const select = useCallback((sel: Selection) => {
    setSelection(sel)
  }, [])

  const selectCell = useCallback((a: Addr) => {
    setSelection({ anchor: a, range: singleRange(a) })
  }, [])

  const moveSelection = useCallback(
    (dir: MoveDirection, extend: boolean) => {
      setSelection((prev) => {
        const from = extend ? prev.range.end : prev.anchor
        const delta: Record<MoveDirection, [number, number]> = {
          down: [1, 0],
          up: [-1, 0],
          left: [0, -1],
          right: [0, 1],
          none: [0, 0],
        }
        const [dr, dc] = delta[dir]
        const next = mkAddr(
          Math.max(0, Math.min(MAX_ROW, from.row + dr)),
          Math.max(0, Math.min(MAX_COL, from.col + dc)),
        )
        if (extend) {
          return { anchor: prev.anchor, range: mkRange(prev.anchor, next) }
        }
        return { anchor: next, range: singleRange(next) }
      })
    },
    [],
  )

  const startEdit = useCallback(
    (a: Addr, initial?: string) => {
      const engine = engineRef.current
      if (!engine) return
      const replacing = initial !== undefined
      setEditing({
        addr: a,
        value: replacing ? initial : engine.cellInput(activeSheet, a.row, a.col),
        replacing,
      })
    },
    [activeSheet],
  )

  const updateEdit = useCallback((value: string) => {
    setEditing((prev) => (prev ? { ...prev, value } : prev))
  }, [])

  const commitEdit = useCallback(
    (move: MoveDirection) => {
      const current = editingRef.current
      if (!current) return
      editingRef.current = null
      setEditing(null)
      apply({
        action: 'cell_edit',
        sheet: activeSheet,
        addr: current.addr,
        input: current.value,
      })
      if (move !== 'none') moveSelection(move, false)
    },
    [activeSheet, apply, moveSelection],
  )

  const cancelEdit = useCallback(() => setEditing(null), [])

  return {
    engine: engineRef.current,
    ready,
    sheets,
    activeSheet,
    selection,
    editing,
    version,
    error,
    warnings,
    canUndo: undoRedo.undo,
    canRedo: undoRedo.redo,
    apply,
    applyBatch,
    setActiveSheet,
    select,
    selectCell,
    moveSelection,
    startEdit,
    updateEdit,
    commitEdit,
    cancelEdit,
    dismissError: () => setError(null),
    setWarnings,
  }
}
