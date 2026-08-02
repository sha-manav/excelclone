import { useCallback, useEffect, useRef, useState } from 'react'
import { Grid } from './components/Grid'
import { FormulaBar } from './components/FormulaBar'
import { SheetTabs } from './components/SheetTabs'
import { CaptureChip } from './components/CaptureChip'
import { ConsentModal } from './components/ConsentModal'
import { TransparencyPage } from './components/TransparencyPage'
import { useWorkbook } from './state/useWorkbook'
import { useCapture } from './state/useCapture'
import { range as mkRange, rangeA1, type Addr, type Range } from './engine/actions'
import type { MoveDirection } from './state/useWorkbook'

const TRANSPARENCY_PATH = '/transparency'

/** Clipboard state lives in the app, not the engine: copying changes nothing. */
interface Clipboard {
  sheet: string
  range: Range
  cut: boolean
}

export default function App() {
  const wb = useWorkbook()
  const [clipboard, setClipboard] = useState<Clipboard | null>(null)
  const gridHostRef = useRef<HTMLDivElement>(null)

  // Routing, such as it is: two screens do not justify a router, and the
  // transparency page has to be linkable rather than a modal.
  const [path, setPath] = useState(() => window.location.pathname)
  useEffect(() => {
    const onPop = () => setPath(window.location.pathname)
    window.addEventListener('popstate', onPop)
    return () => window.removeEventListener('popstate', onPop)
  }, [])
  const navigate = useCallback((to: string) => {
    window.history.pushState({}, '', to)
    setPath(to)
  }, [])

  const capture = useCapture({
    engine: wb.engine,
    sheet: wb.activeSheet,
    selection: rangeA1(wb.selection.range),
  })

  const activeInfo = wb.sheets.find((s) => s.name === wb.activeSheet)
  const active = wb.selection.anchor
  const cellInput =
    wb.engine && wb.ready
      ? wb.engine.cellInput(wb.activeSheet, active.row, active.col)
      : ''

  // Keyboard shortcuts that act on the workbook rather than the grid itself.
  const onKeyDown = useCallback(
    (e: KeyboardEvent) => {
      if (wb.editing) return
      const target = e.target as HTMLElement | null
      if (target && (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA')) return
      const mod = e.metaKey || e.ctrlKey
      const sel = wb.selection

      if (e.key === 'Delete' || e.key === 'Backspace') {
        e.preventDefault()
        wb.apply({ action: 'range_clear', sheet: wb.activeSheet, range: sel.range })
        return
      }
      if (!mod) return

      switch (e.key.toLowerCase()) {
        case 'c':
          e.preventDefault()
          setClipboard({ sheet: wb.activeSheet, range: sel.range, cut: false })
          break
        case 'x':
          e.preventDefault()
          setClipboard({ sheet: wb.activeSheet, range: sel.range, cut: true })
          break
        case 'v': {
          if (!clipboard) return
          e.preventDefault()
          wb.apply({
            action: 'range_paste',
            source_sheet: clipboard.sheet,
            source: clipboard.range,
            target_sheet: wb.activeSheet,
            target: sel.range,
            mode: 'formulas',
            cut: clipboard.cut,
          })
          // A cut is consumed by its paste, as in Excel.
          if (clipboard.cut) setClipboard(null)
          break
        }
        case 'z':
          e.preventDefault()
          wb.apply({ action: e.shiftKey ? 'redo' : 'undo' })
          break
        case 'y':
          e.preventDefault()
          wb.apply({ action: 'redo' })
          break
        case 'd': {
          e.preventDefault()
          // Fill down from the first row of the selection.
          const src = mkRange(sel.range.start, {
            row: sel.range.start.row,
            col: sel.range.end.col,
          })
          if (sel.range.end.row > sel.range.start.row) {
            wb.apply({
              action: 'fill_apply',
              sheet: wb.activeSheet,
              source: src,
              target: sel.range,
            })
          }
          break
        }
        case 'r': {
          e.preventDefault()
          const src = mkRange(sel.range.start, {
            row: sel.range.end.row,
            col: sel.range.start.col,
          })
          if (sel.range.end.col > sel.range.start.col) {
            wb.apply({
              action: 'fill_apply',
              sheet: wb.activeSheet,
              source: src,
              target: sel.range,
            })
          }
          break
        }
      }
    },
    [wb, clipboard],
  )

  useEffect(() => {
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [onKeyDown])

  const handleFill = useCallback(
    (source: Range, target: Range) => {
      wb.apply({ action: 'fill_apply', sheet: wb.activeSheet, source, target })
    },
    [wb],
  )

  const handleContextMenu = useCallback(
    (addr: Addr, x: number, y: number) => {
      setMenu({ addr, x, y })
    },
    [],
  )

  const [menu, setMenu] = useState<{ addr: Addr; x: number; y: number } | null>(null)
  useEffect(() => {
    if (!menu) return
    const close = () => setMenu(null)
    window.addEventListener('click', close)
    return () => window.removeEventListener('click', close)
  }, [menu])

  if (!wb.ready) {
    return (
      <div className="app">
        <div className="loading">Loading engine…</div>
      </div>
    )
  }

  // The transparency page is reachable before consent is answered — the modal
  // links to it, so covering it with the modal would make the link useless.
  if (path === TRANSPARENCY_PATH) {
    return (
      <TransparencyPage
        mode={capture.mode}
        state={capture.state}
        dropped={capture.dropped}
        pending={capture.pending}
        durable={capture.durable}
        onChangeMode={capture.choose}
        onBack={() => navigate('/')}
      />
    )
  }

  const sel = wb.selection

  return (
    <div className="app">
      <header className="toolbar">
        <span className="logo">Gridline</span>
        <div className="toolbar__group">
          <button
            disabled={!wb.canUndo}
            onClick={() => wb.apply({ action: 'undo' })}
            title="Undo (Cmd/Ctrl+Z)"
          >
            Undo
          </button>
          <button
            disabled={!wb.canRedo}
            onClick={() => wb.apply({ action: 'redo' })}
            title="Redo (Cmd/Ctrl+Shift+Z)"
          >
            Redo
          </button>
        </div>
        <div className="toolbar__group">
          <button
            onClick={() =>
              wb.apply({
                action: 'row_insert',
                sheet: wb.activeSheet,
                at: sel.range.start.row,
                count: sel.range.end.row - sel.range.start.row + 1,
              })
            }
          >
            Insert rows
          </button>
          <button
            onClick={() =>
              wb.apply({
                action: 'row_delete',
                sheet: wb.activeSheet,
                at: sel.range.start.row,
                count: sel.range.end.row - sel.range.start.row + 1,
              })
            }
          >
            Delete rows
          </button>
        </div>
        <div className="toolbar__spacer" />
        <span className="toolbar__status">{rangeA1(sel.range)}</span>
        <CaptureChip
          state={capture.state}
          mode={capture.mode}
          dropped={capture.dropped}
          pending={capture.pending}
          onToggle={capture.toggle}
        />
        <a
          className="toolbar__link"
          href={TRANSPARENCY_PATH}
          onClick={(e) => {
            e.preventDefault()
            navigate(TRANSPARENCY_PATH)
          }}
        >
          What&rsquo;s captured
        </a>
      </header>

      <FormulaBar
        active={active}
        cellInput={cellInput}
        editing={wb.editing !== null}
        editValue={wb.editing?.value ?? ''}
        onChange={wb.updateEdit}
        onCommit={() => wb.commitEdit('down')}
        onCancel={wb.cancelEdit}
        onBeginEdit={() => wb.startEdit(active)}
      />

      <div className="grid-host" ref={gridHostRef}>
        {wb.engine && (
          <Grid
            engine={wb.engine}
            sheet={wb.activeSheet}
            version={wb.version}
            selection={sel}
            editing={wb.editing}
            hiddenRows={activeInfo?.hidden_rows ?? []}
            onSelect={wb.select}
            onStartEdit={wb.startEdit}
            onCommitEdit={(move: MoveDirection) => wb.commitEdit(move)}
            onCancelEdit={wb.cancelEdit}
            onEditValueChange={wb.updateEdit}
            onFill={handleFill}
            onContextMenu={handleContextMenu}
            onAutofitColumn={() => {}}
          />
        )}
      </div>

      {menu && (
        <ul className="context-menu" style={{ left: menu.x, top: menu.y }}>
          <li
            onClick={() =>
              wb.apply({
                action: 'row_insert',
                sheet: wb.activeSheet,
                at: menu.addr.row,
                count: 1,
              })
            }
          >
            Insert row
          </li>
          <li
            onClick={() =>
              wb.apply({
                action: 'row_delete',
                sheet: wb.activeSheet,
                at: menu.addr.row,
                count: 1,
              })
            }
          >
            Delete row
          </li>
          <li
            onClick={() =>
              wb.apply({
                action: 'col_insert',
                sheet: wb.activeSheet,
                at: menu.addr.col,
                count: 1,
              })
            }
          >
            Insert column
          </li>
          <li
            onClick={() =>
              wb.apply({
                action: 'col_delete',
                sheet: wb.activeSheet,
                at: menu.addr.col,
                count: 1,
              })
            }
          >
            Delete column
          </li>
          <li
            onClick={() =>
              wb.apply({
                action: 'sort_apply',
                sheet: wb.activeSheet,
                range: sel.range,
                keys: [{ column: sel.range.start.col, ascending: true }],
                has_header: false,
              })
            }
          >
            Sort ascending
          </li>
          <li
            onClick={() =>
              wb.apply({
                action: 'sort_apply',
                sheet: wb.activeSheet,
                range: sel.range,
                keys: [{ column: sel.range.start.col, ascending: false }],
                has_header: false,
              })
            }
          >
            Sort descending
          </li>
        </ul>
      )}

      <SheetTabs
        sheets={wb.sheets}
        active={wb.activeSheet}
        onSelect={wb.setActiveSheet}
        onAdd={() => {
          const base = 'Sheet'
          let n = wb.sheets.length + 1
          while (wb.sheets.some((s) => s.name === `${base}${n}`)) n += 1
          wb.apply({ action: 'sheet_add', name: `${base}${n}` })
        }}
        onRename={(from, to) => wb.apply({ action: 'sheet_rename', from, to })}
        onDelete={(name) => wb.apply({ action: 'sheet_delete', name })}
      />

      {wb.error && (
        <div className="error-toast" role="alert">
          <span>{wb.error}</span>
          <button onClick={wb.dismissError}>Dismiss</button>
        </div>
      )}

      {capture.needsConsent && (
        <ConsentModal
          onChoose={capture.choose}
          onOpenTransparency={() => navigate(TRANSPARENCY_PATH)}
        />
      )}
    </div>
  )
}
