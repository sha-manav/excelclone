import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { Grid } from './components/Grid'
import { FormulaBar } from './components/FormulaBar'
import { SheetTabs } from './components/SheetTabs'
import { CaptureChip } from './components/CaptureChip'
import { ConsentModal } from './components/ConsentModal'
import { TransparencyPage } from './components/TransparencyPage'
import { FormatToolbar } from './components/FormatToolbar'
import { ContextMenu } from './components/ContextMenu'
import type { MenuItem } from './components/ContextMenu'
import { SortDialog } from './components/SortDialog'
import { FilterMenu } from './components/FilterMenu'
import { FindReplacePanel } from './components/FindReplacePanel'
import { WarningsDrawer } from './components/WarningsDrawer'
import { RoutinesPanel } from './components/RoutinesPanel'
import { api, type RoutineRecord } from './capture/api'
import { MergeMap, parseRangeA1 } from './components/grid-geometry'
import { useWorkbook } from './state/useWorkbook'
import { useCapture } from './state/useCapture'
import {
  range as mkRange,
  rangeA1,
  rangeCols,
  rangeRows,
  singleRange,
  type Addr,
  type FormatPatch,
  type Range,
  type SortKey,
} from './engine/actions'
import type { CellFormat } from './engine/bridge'
import type { MoveDirection } from './state/useWorkbook'

const TRANSPARENCY_PATH = '/transparency'
const EMPTY_FORMAT: CellFormat = {}

/** Clipboard state lives in the app, not the engine: copying changes nothing. */
interface Clipboard {
  sheet: string
  range: Range
  cut: boolean
}

type Overlay =
  | { kind: 'sort'; range: Range }
  | { kind: 'filter'; range: Range; column: number }
  | null

export default function App() {
  const wb = useWorkbook()
  const [clipboard, setClipboard] = useState<Clipboard | null>(null)
  const [overlay, setOverlay] = useState<Overlay>(null)
  const [showWarnings, setShowWarnings] = useState(false)
  const [finding, setFinding] = useState(false)
  const [matches, setMatches] = useState<string[]>([])
  const [matchIndex, setMatchIndex] = useState(0)
  const [menu, setMenu] = useState<{ addr: Addr; x: number; y: number } | null>(null)
  const [showRoutines, setShowRoutines] = useState(false)
  const [routines, setRoutines] = useState<RoutineRecord[]>([])
  const [routinesLoading, setRoutinesLoading] = useState(false)
  const [routinesError, setRoutinesError] = useState<string | null>(null)
  const fileInputRef = useRef<HTMLInputElement>(null)

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
  const sel = wb.selection
  const cellInput =
    wb.engine && wb.ready
      ? wb.engine.cellInput(wb.activeSheet, active.row, active.col)
      : ''

  // The toolbar's pressed states come from the engine's view of the anchor,
  // re-read on every applied action rather than mirrored in React state.
  const activeFormat: CellFormat = useMemo(() => {
    if (!wb.engine || !wb.ready) return EMPTY_FORMAT
    void wb.version
    return wb.engine.cellFormat(wb.activeSheet, active.row, active.col)
  }, [wb.engine, wb.ready, wb.version, wb.activeSheet, active.row, active.col])

  const mergedList = activeInfo?.merged ?? []
  const mergedKey = mergedList.join('|')
  const merges = useMemo(
    () => MergeMap.fromA1(mergedKey ? mergedKey.split('|') : []),
    [mergedKey],
  )
  const mergeAtAnchor = merges.at(active.row, active.col)

  const apply = wb.apply

  // Read during render, so a stale sheet name — the one render after a
  // rename — must come back empty rather than throwing and taking the whole
  // tree down with it.
  const filterValues = useMemo(() => {
    if (!wb.engine || overlay?.kind !== 'filter') return []
    void wb.version
    try {
      return wb.engine.columnValues(
        wb.activeSheet,
        rangeA1(overlay.range),
        overlay.column,
      )
    } catch {
      return []
    }
  }, [wb.engine, wb.version, wb.activeSheet, overlay])

  /* ------------------------------------------------------------ formatting */

  const patchFormat = useCallback(
    (patches: FormatPatch[]) => {
      apply({
        action: 'format_apply',
        sheet: wb.activeSheet,
        range: sel.range,
        patches,
      })
    },
    [apply, wb.activeSheet, sel.range],
  )

  const clearFormatting = useCallback(() => {
    apply({ action: 'format_clear', sheet: wb.activeSheet, range: sel.range })
  }, [apply, wb.activeSheet, sel.range])

  const toggleMerge = useCallback(() => {
    if (mergeAtAnchor) {
      apply({
        action: 'merge_clear',
        sheet: wb.activeSheet,
        range: mergeAtAnchor,
      })
      return
    }
    if (rangeRows(sel.range) === 1 && rangeCols(sel.range) === 1) return
    apply({ action: 'merge_apply', sheet: wb.activeSheet, range: sel.range })
  }, [apply, wb.activeSheet, sel.range, mergeAtAnchor])

  /* ------------------------------------------------------ find and replace */

  const runSearch = useCallback(
    (find: string, matchCase: boolean, wholeCell: boolean) => {
      if (!wb.engine || find === '') {
        setMatches([])
        setMatchIndex(0)
        return
      }
      const hits = wb.engine.findMatches(wb.activeSheet, find, matchCase, wholeCell)
      setMatches(hits)
      setMatchIndex(0)
      setLastSearch({ find, matchCase, wholeCell })
    },
    [wb.engine, wb.activeSheet],
  )
  const [lastSearch, setLastSearch] = useState({
    find: '',
    matchCase: false,
    wholeCell: false,
  })

  const stepMatch = useCallback(
    (delta: number) => {
      if (matches.length === 0) return
      const next = (matchIndex + delta + matches.length) % matches.length
      setMatchIndex(next)
      const addr = parseRangeA1(matches[next])
      if (addr) wb.select({ anchor: addr.start, range: singleRange(addr.start) })
    },
    [matches, matchIndex, wb],
  )

  // Jump to the first hit as soon as there is one, so typing a term moves the
  // grid rather than leaving the user to press Next.
  useEffect(() => {
    if (matches.length === 0) return
    const addr = parseRangeA1(matches[0])
    if (addr) wb.select({ anchor: addr.start, range: singleRange(addr.start) })
    // `wb` changes every render; the effect must run on new matches only.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [matches])

  const replaceAll = useCallback(
    (replace: string) => {
      if (lastSearch.find === '') return
      apply({
        action: 'find_replace',
        sheet: wb.activeSheet,
        range: null,
        find: lastSearch.find,
        replace,
        match_case: lastSearch.matchCase,
        whole_cell: lastSearch.wholeCell,
      })
      setMatches([])
      setMatchIndex(0)
    },
    [apply, wb.activeSheet, lastSearch],
  )

  const highlights = useMemo(
    () =>
      matches
        .map((a) => parseRangeA1(a))
        .filter((r): r is Range => r !== null),
    [matches],
  )

  /* ------------------------------------------------------------- routines */

  const workbookId = capture.controller.currentWorkbookId()

  const loadRoutines = useCallback(async () => {
    setRoutinesLoading(true)
    const res = await api.getRoutines(workbookId)
    setRoutinesLoading(false)
    if (!res.ok) {
      // The panel is a suggestion box. A server that is down means no
      // suggestions, not a broken spreadsheet, so this is a line of text in
      // the panel rather than the error toast.
      setRoutinesError(res.error ?? 'could not reach the server')
      return
    }
    setRoutinesError(null)
    setRoutines(res.data ?? [])
  }, [workbookId])

  useEffect(() => {
    if (showRoutines) void loadRoutines()
  }, [showRoutines, loadRoutines])

  const runRoutine = useCallback(
    (routine: RoutineRecord) => {
      if (!wb.engine) return
      try {
        const actions = wb.engine.routineActions(
          routine.body,
          wb.activeSheet,
          active.row,
          active.col,
        )
        if (actions.length === 0) return
        // Through the same batch path every other gesture uses, so it lands
        // in one undo step and the capture pipeline sees the actions without
        // being taught what a routine is.
        if (!wb.applyBatch(actions)) return
        capture.controller.recordShellAction('routine.run', {
          routine_id: routine.id,
          steps: actions.length,
          anchor: rangeA1(singleRange(active)),
        })
        void api.postRoutineFeedback(routine.id, 'accepted').then(() => loadRoutines())
      } catch (e) {
        wb.reportError(String(e))
      }
    },
    [wb, active, capture, loadRoutines],
  )

  const dismissRoutine = useCallback((routine: RoutineRecord) => {
    // Optimistic: the panel is not worth a spinner, and a dismissal the
    // server never received simply reappears on the next refresh.
    setRoutines((prev) => prev.filter((r) => r.id !== routine.id))
    void api.postRoutineFeedback(routine.id, 'dismissed')
  }, [])

  /* ---------------------------------------------------------------- files */

  const openFile = useCallback(
    async (file: File) => {
      if (!wb.engine) return
      const bytes = new Uint8Array(await file.arrayBuffer())
      try {
        const outcome = file.name.toLowerCase().endsWith('.csv')
          ? wb.engine.importCsv(bytes, file.name.replace(/\.csv$/i, '').slice(0, 31) || 'Sheet1')
          : wb.engine.importXlsx(bytes)
        wb.reset(outcome.sheets[0] ?? 'Sheet1')
        wb.setWarnings(outcome.warnings)
        setShowWarnings(outcome.warnings.length > 0)
        capture.controller.recordShellAction('file.import', {
          format: file.name.toLowerCase().endsWith('.csv') ? 'csv' : 'xlsx',
          sheets: outcome.sheets.length,
          warnings: outcome.warnings.length,
        })
      } catch (e) {
        wb.reportError(String(e))
      }
    },
    [wb, capture],
  )

  const download = useCallback((bytes: Uint8Array, name: string, type: string) => {
    // `bytes` is a view onto wasm memory; copy before handing it to Blob, or
    // a later allocation can move the buffer out from under the download.
    const blob = new Blob([new Uint8Array(bytes)], { type })
    const url = URL.createObjectURL(blob)
    const a = document.createElement('a')
    a.href = url
    a.download = name
    // The anchor has to be in the document, and the URL has to outlive the
    // click: revoking it synchronously cancels the download before the
    // browser has read a byte. Caught by driving the real browser — nothing
    // about this code looks wrong on the page.
    a.style.display = 'none'
    document.body.appendChild(a)
    a.click()
    window.setTimeout(() => {
      a.remove()
      URL.revokeObjectURL(url)
    }, 0)
  }, [])

  const exportXlsx = useCallback(() => {
    if (!wb.engine) return
    try {
      download(
        wb.engine.exportXlsx(),
        'gridline.xlsx',
        'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
      )
      capture.controller.recordShellAction('file.export', { format: 'xlsx' })
    } catch (e) {
      wb.reportError(String(e))
    }
  }, [wb, download, capture])

  const exportCsv = useCallback(() => {
    if (!wb.engine) return
    try {
      download(wb.engine.exportCsv(wb.activeSheet), `${wb.activeSheet}.csv`, 'text/csv')
      capture.controller.recordShellAction('file.export', { format: 'csv' })
    } catch (e) {
      wb.reportError(String(e))
    }
  }, [wb, download, capture])

  /* ------------------------------------------------------------- keyboard */

  const onKeyDown = useCallback(
    (e: KeyboardEvent) => {
      if (wb.editing) return
      const target = e.target as HTMLElement | null
      if (target && (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA')) {
        if (e.key === 'Escape') setFinding(false)
        return
      }
      const mod = e.metaKey || e.ctrlKey

      if (e.key === 'Escape') {
        setFinding(false)
        setOverlay(null)
        return
      }
      if (e.key === 'Delete' || e.key === 'Backspace') {
        e.preventDefault()
        apply({ action: 'range_clear', sheet: wb.activeSheet, range: sel.range })
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
          apply({
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
          apply({ action: e.shiftKey ? 'redo' : 'undo' })
          break
        case 'y':
          e.preventDefault()
          apply({ action: 'redo' })
          break
        case 'b':
          e.preventDefault()
          patchFormat([{ set: 'bold', value: !activeFormat.bold }])
          break
        case 'i':
          e.preventDefault()
          patchFormat([{ set: 'italic', value: !activeFormat.italic }])
          break
        case 'f':
        case 'h':
          e.preventDefault()
          setFinding(true)
          break
        case 'd': {
          e.preventDefault()
          // Fill down from the first row of the selection.
          const src = mkRange(sel.range.start, {
            row: sel.range.start.row,
            col: sel.range.end.col,
          })
          if (sel.range.end.row > sel.range.start.row) {
            apply({
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
            apply({
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
    [wb, sel.range, clipboard, apply, patchFormat, activeFormat],
  )

  useEffect(() => {
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [onKeyDown])

  const handleFill = useCallback(
    (source: Range, target: Range) => {
      apply({ action: 'fill_apply', sheet: wb.activeSheet, source, target })
    },
    [apply, wb.activeSheet],
  )

  const handleContextMenu = useCallback((addr: Addr, x: number, y: number) => {
    setMenu({ addr, x, y })
  }, [])

  const menuItems: MenuItem[] = useMemo(() => {
    if (!menu) return []
    const rows = rangeRows(sel.range)
    const cols = rangeCols(sel.range)
    const at = menu.addr
    return [
      {
        label: rows === 1 ? 'Insert row' : `Insert ${rows} rows`,
        onSelect: () =>
          apply({
            action: 'row_insert',
            sheet: wb.activeSheet,
            at: sel.range.start.row,
            count: rows,
          }),
      },
      {
        label: rows === 1 ? 'Delete row' : `Delete ${rows} rows`,
        onSelect: () =>
          apply({
            action: 'row_delete',
            sheet: wb.activeSheet,
            at: sel.range.start.row,
            count: rows,
          }),
      },
      {
        label: cols === 1 ? 'Insert column' : `Insert ${cols} columns`,
        onSelect: () =>
          apply({
            action: 'col_insert',
            sheet: wb.activeSheet,
            at: sel.range.start.col,
            count: cols,
          }),
      },
      {
        label: cols === 1 ? 'Delete column' : `Delete ${cols} columns`,
        onSelect: () =>
          apply({
            action: 'col_delete',
            sheet: wb.activeSheet,
            at: sel.range.start.col,
            count: cols,
          }),
      },
      {},
      {
        label: 'Sort ascending',
        onSelect: () =>
          apply({
            action: 'sort_apply',
            sheet: wb.activeSheet,
            range: sel.range,
            keys: [{ column: at.col, ascending: true }],
            has_header: false,
          }),
      },
      {
        label: 'Sort descending',
        onSelect: () =>
          apply({
            action: 'sort_apply',
            sheet: wb.activeSheet,
            range: sel.range,
            keys: [{ column: at.col, ascending: false }],
            has_header: false,
          }),
      },
      {
        label: 'Custom sort…',
        disabled: rows < 2,
        title: rows < 2 ? 'Select the rows to sort first' : undefined,
        onSelect: () => setOverlay({ kind: 'sort', range: sel.range }),
      },
      {
        label: 'Filter…',
        disabled: rows < 2,
        title: rows < 2 ? 'Select the column including its header' : undefined,
        onSelect: () => setOverlay({ kind: 'filter', range: sel.range, column: at.col }),
      },
      ...(activeInfo?.hidden_rows.length
        ? [
            {
              label: 'Clear filter',
              onSelect: () => apply({ action: 'filter_clear', sheet: wb.activeSheet }),
            },
          ]
        : []),
      {},
      {
        label: mergeAtAnchor ? 'Unmerge cells' : 'Merge cells',
        disabled: !mergeAtAnchor && rows === 1 && cols === 1,
        title:
          !mergeAtAnchor && rows === 1 && cols === 1
            ? 'Select more than one cell to merge'
            : undefined,
        onSelect: toggleMerge,
      },
      {
        label: 'Clear contents',
        onSelect: () =>
          apply({ action: 'range_clear', sheet: wb.activeSheet, range: sel.range }),
      },
      { label: 'Clear formatting', onSelect: clearFormatting },
    ]
  }, [
    menu,
    sel.range,
    wb.activeSheet,
    apply,
    activeInfo,
    mergeAtAnchor,
    toggleMerge,
    clearFormatting,
  ])

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

  return (
    <div className="app">
      <header className="toolbar">
        <span className="logo">Gridline</span>
        <div className="toolbar__group">
          <button onClick={() => fileInputRef.current?.click()}>Open</button>
          <input
            ref={fileInputRef}
            type="file"
            accept=".xlsx,.csv"
            aria-label="Open a workbook"
            style={{ display: 'none' }}
            onChange={(e) => {
              const file = e.target.files?.[0]
              // Reset first, so opening the same file twice still fires.
              e.target.value = ''
              if (file) void openFile(file)
            }}
          />
          <button onClick={exportXlsx}>Save .xlsx</button>
          <button onClick={exportCsv}>Save .csv</button>
          {wb.warnings.length > 0 && (
            <button
              className="warn-badge"
              onClick={() => setShowWarnings((v) => !v)}
              title="What the importer could not model"
            >
              {wb.warnings.length} note{wb.warnings.length === 1 ? '' : 's'}
            </button>
          )}
        </div>
        <div className="toolbar__group">
          <button
            disabled={!wb.canUndo}
            onClick={() => apply({ action: 'undo' })}
            title="Undo (Cmd/Ctrl+Z)"
          >
            Undo
          </button>
          <button
            disabled={!wb.canRedo}
            onClick={() => apply({ action: 'redo' })}
            title="Redo (Cmd/Ctrl+Shift+Z)"
          >
            Redo
          </button>
        </div>

        <FormatToolbar
          active={activeFormat}
          hasRange={rangeRows(sel.range) > 1 || rangeCols(sel.range) > 1}
          merged={mergeAtAnchor !== null}
          onPatch={patchFormat}
          onClearFormatting={clearFormatting}
          onMergeToggle={toggleMerge}
        />

        <div className="toolbar__group">
          <button onClick={() => setFinding((v) => !v)} title="Find & replace (Cmd/Ctrl+F)">
            Find
          </button>
          <button
            onClick={() => setShowRoutines((v) => !v)}
            aria-pressed={showRoutines}
            className={showRoutines ? 'is-on' : undefined}
            title="Work Gridline noticed you repeating"
          >
            Routines
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

      {finding && (
        <FindReplacePanel
          matches={matches}
          current={matchIndex}
          onSearch={runSearch}
          onStep={stepMatch}
          onReplaceAll={replaceAll}
          onClose={() => {
            setFinding(false)
            setMatches([])
          }}
        />
      )}

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

      <div className="workspace">
        <div className="grid-host">
          {wb.engine && (
            <Grid
              engine={wb.engine}
              sheet={wb.activeSheet}
              version={wb.version}
              selection={sel}
              editing={wb.editing}
              hiddenRows={activeInfo?.hidden_rows ?? []}
              merged={mergedList}
              paintedRows={activeInfo?.painted_rows ?? 0}
              paintedCols={activeInfo?.painted_cols ?? 0}
              highlights={highlights}
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
        {showWarnings && (
          <WarningsDrawer
            warnings={wb.warnings}
            onClose={() => setShowWarnings(false)}
          />
        )}
        {showRoutines && wb.engine && (
          <RoutinesPanel
            engine={wb.engine}
            routines={routines}
            sheet={wb.activeSheet}
            anchor={active}
            version={wb.version}
            loading={routinesLoading}
            error={routinesError}
            onRun={runRoutine}
            onDismiss={dismissRoutine}
            onRefresh={() => void loadRoutines()}
            onClose={() => setShowRoutines(false)}
          />
        )}
      </div>

      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          items={menuItems}
          onClose={() => setMenu(null)}
        />
      )}

      {overlay?.kind === 'sort' && (
        <SortDialog
          range={overlay.range}
          onCancel={() => setOverlay(null)}
          onApply={(keys: SortKey[], hasHeader: boolean) => {
            setOverlay(null)
            apply({
              action: 'sort_apply',
              sheet: wb.activeSheet,
              range: overlay.range,
              keys,
              has_header: hasHeader,
            })
          }}
        />
      )}

      {overlay?.kind === 'filter' && wb.engine && (
        <FilterMenu
          range={overlay.range}
          column={overlay.column}
          values={filterValues}
          allowed={null}
          onCancel={() => setOverlay(null)}
          onClear={() => {
            setOverlay(null)
            apply({ action: 'filter_clear', sheet: wb.activeSheet })
          }}
          onApply={(allowed) => {
            setOverlay(null)
            apply({
              action: 'filter_apply',
              sheet: wb.activeSheet,
              spec: { range: overlay.range, column: overlay.column, allowed },
            })
          }}
        />
      )}

      <SheetTabs
        sheets={wb.sheets}
        active={wb.activeSheet}
        onSelect={wb.setActiveSheet}
        onAdd={() => {
          const base = 'Sheet'
          let n = wb.sheets.length + 1
          while (wb.sheets.some((s) => s.name === `${base}${n}`)) n += 1
          apply({ action: 'sheet_add', name: `${base}${n}` })
        }}
        onRename={(from, to) => apply({ action: 'sheet_rename', from, to })}
        onDelete={(name) => apply({ action: 'sheet_delete', name })}
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
