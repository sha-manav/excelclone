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
import { MAX_COL, MAX_ROW, MergeMap, parseRangeA1 } from './components/grid-geometry'
import { useWorkbook } from './state/useWorkbook'
import { useCapture } from './state/useCapture'
import {
  range as mkRange,
  rangeA1,
  rangeCols,
  rangeRows,
  singleRange,
  type Action,
  type Addr,
  type Axis,
  type FormatPatch,
  type Range,
  type SortKey,
} from './engine/actions'
import { readClipboard, toHtml, toTsv, type Block } from './engine/clipboard'
import type { CellFormat } from './engine/bridge'
import type { MoveDirection } from './state/useWorkbook'
import { isStandalone } from './standalone'

const TRANSPARENCY_PATH = '/transparency'

/** `A1:B2` -> `$A$1:$B$2`, which is how xlsx spells a defined name's target. */
const absolute = (a1Range: string): string => a1Range.replace(/([A-Z]+)([0-9]+)/g, '$$$1$$$2')
const EMPTY_FORMAT: CellFormat = {}

/** Clipboard state lives in the app, not the engine: copying changes nothing. */
interface Clipboard {
  sheet: string
  range: Range
  cut: boolean
  /**
   * Exactly the text put on the system clipboard when this was copied.
   *
   * A paste compares it with what the system clipboard now holds. Equal means
   * nothing has happened since and the internal range paste applies, formulas
   * and all; different means someone copied elsewhere and what is on the
   * clipboard is theirs, not ours.
   */
  text: string
}

type Overlay =
  | { kind: 'sort'; range: Range }
  | { kind: 'filter'; range: Range; column: number }
  | null

export default function App() {
  // A build with no server behind it: the spreadsheet entire, and none of the
  // controls for a capture pipeline that has nowhere to send anything.
  const standalone = isStandalone()
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
  // Fixed for the life of the build, and it crosses the wasm boundary, so it
  // is read once the engine is up rather than on every render.
  const functionNames = useMemo(
    () => (wb.engine && wb.ready ? wb.engine.functionNames() : []),
    [wb.engine, wb.ready],
  )

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
        // c, x and v are deliberately absent: preventing the default would
        // suppress the browser's own copy/cut/paste events, and those are the
        // only route to the system clipboard that needs no permission prompt.
        // They are handled below.
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
    [wb, sel.range, apply, patchFormat, activeFormat],
  )

  useEffect(() => {
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [onKeyDown])

  /* ------------------------------------------------------------ clipboard */

  /** The selected block as displayed text, which is what Excel copies out. */
  const selectedBlock = useCallback((): Block => {
    if (!wb.engine) return []
    const r = sel.range
    const rows = r.end.row - r.start.row + 1
    const cols = r.end.col - r.start.col + 1
    const vp = wb.engine.viewport(wb.activeSheet, r.start.row, r.start.col, rows, cols)
    const out: Block = []
    for (let i = 0; i < rows; i++) {
      out.push(Array.from({ length: cols }, (_, j) => vp.values[i * cols + j] ?? ''))
    }
    return out
  }, [wb.engine, wb.activeSheet, sel.range])

  const onCopyOrCut = useCallback(
    (e: ClipboardEvent, cut: boolean) => {
      if (wb.editing || !wb.engine) return
      const target = e.target as HTMLElement | null
      if (target && (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA')) return
      e.preventDefault()
      const block = selectedBlock()
      const tsv = toTsv(block)
      e.clipboardData?.setData('text/plain', tsv)
      e.clipboardData?.setData('text/html', toHtml(block))
      // The internal clipboard is kept as well as the system one: a paste
      // back into Gridline carries formulas and adjusts their references,
      // which the text on the system clipboard cannot. `text` is how the
      // paste handler tells "this is the block I copied" from "someone else
      // put something here since".
      setClipboard({ sheet: wb.activeSheet, range: sel.range, cut, text: tsv })
    },
    [wb.editing, wb.engine, wb.activeSheet, sel.range, selectedBlock],
  )

  const onPaste = useCallback(
    (e: ClipboardEvent) => {
      if (wb.editing || !wb.engine) return
      const target = e.target as HTMLElement | null
      if (target && (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA')) return
      e.preventDefault()
      const incoming = e.clipboardData?.getData('text/plain') ?? ''

      // Our own block, untouched since we copied it: paste it the rich way.
      if (clipboard && incoming === clipboard.text) {
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
        return
      }

      const block = readClipboard(e.clipboardData)
      if (!block || block.length === 0) return
      // Text from outside has no formulas to adjust and no source range to
      // read from, so it lands as edits — one per cell, batched so the whole
      // paste is a single Ctrl+Z.
      const actions: Action[] = []
      for (let i = 0; i < block.length; i++) {
        for (let j = 0; j < block[i].length; j++) {
          const row = sel.range.start.row + i
          const col = sel.range.start.col + j
          if (row > MAX_ROW || col > MAX_COL) continue
          actions.push({
            action: 'cell_edit',
            sheet: wb.activeSheet,
            addr: { row, col },
            input: block[i][j],
          })
        }
      }
      if (actions.length === 0) return
      wb.applyBatch(actions)
      // Select what landed, which is what Excel does and what makes a second
      // paste elsewhere obvious.
      const end = {
        row: Math.min(MAX_ROW, sel.range.start.row + block.length - 1),
        col: Math.min(MAX_COL, sel.range.start.col + block[0].length - 1),
      }
      wb.select({ anchor: sel.range.start, range: mkRange(sel.range.start, end) })
    },
    [wb, sel.range, clipboard, apply],
  )

  useEffect(() => {
    const copy = (e: ClipboardEvent) => onCopyOrCut(e, false)
    const cut = (e: ClipboardEvent) => onCopyOrCut(e, true)
    document.addEventListener('copy', copy)
    document.addEventListener('cut', cut)
    document.addEventListener('paste', onPaste)
    return () => {
      document.removeEventListener('copy', copy)
      document.removeEventListener('cut', cut)
      document.removeEventListener('paste', onPaste)
    }
  }, [onCopyOrCut, onPaste])

  const handleFill = useCallback(
    (source: Range, target: Range) => {
      apply({ action: 'fill_apply', sheet: wb.activeSheet, source, target })
    },
    [apply, wb.activeSheet],
  )

  const handleResize = useCallback(
    (axis: Axis, at: number, count: number, size: number | null) => {
      apply({ action: 'resize', sheet: wb.activeSheet, axis, at, count, size })
    },
    [apply, wb.activeSheet],
  )

  /**
   * The name box, with Excel's rule: an address goes there, an existing name
   * goes where it points, anything else becomes a new name over the current
   * selection.
   */
  /**
   * Freeze everything above and to the left of the cursor, or unfreeze when
   * something already is. One button rather than two, because "freeze" and
   * "unfreeze" are the same gesture in Excel and the toolbar shows which one
   * it currently means.
   */
  /**
   * "Highlight cells greater than N" over the selection. A prompt rather than
   * a dialog because the rule has exactly one parameter; anything richer
   * needs a real editor, and half a dialog would be worse than a prompt.
   */
  const addHighlightRule = useCallback(() => {
    const answer = window.prompt('Highlight cells greater than:', '0')
    if (answer === null || answer.trim() === '') return
    apply({
      action: 'cond_add',
      sheet: wb.activeSheet,
      rule: {
        range: sel.range,
        test: { test: 'cell_is', op: 'greater_than', operands: [answer.trim()] },
        format: { fill_color: '#ffd7d7', font_color: '#9c0006' },
      },
    })
  }, [apply, wb.activeSheet, sel.range])

  const toggleFreeze = useCallback(() => {
    const frozen = (activeInfo?.frozen_rows ?? 0) > 0 || (activeInfo?.frozen_cols ?? 0) > 0
    apply({
      action: 'freeze_panes',
      sheet: wb.activeSheet,
      rows: frozen ? 0 : active.row,
      cols: frozen ? 0 : active.col,
    })
  }, [apply, wb.activeSheet, activeInfo, active])

  const handleNameBox = useCallback(
    (text: string) => {
      const asRange = parseRangeA1(text)
      if (asRange) {
        wb.select({ anchor: asRange.start, range: asRange })
        return
      }
      const key = text.toUpperCase()
      const existing = wb.engine?.definedNames().find(([n]) => n === key)
      if (existing) {
        // The stored form is sheet-qualified and absolute; strip both to get
        // something the range parser understands.
        const bare = existing[1].split('!').pop()?.replace(/\$/g, '') ?? ''
        const target = parseRangeA1(bare)
        if (target) wb.select({ anchor: target.start, range: target })
        return
      }
      apply({
        action: 'name_define',
        name: text,
        refers_to: `${wb.activeSheet}!${absolute(rangeA1(sel.range))}`,
      })
    },
    [wb, sel.range, apply],
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
  if (path === TRANSPARENCY_PATH && !standalone) {
    return (
      <TransparencyPage
        mode={capture.mode}
        state={capture.state}
        dropped={capture.dropped}
        pending={capture.pending}
        rejected={capture.rejected}
        registration={capture.registration}
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
          frozen={(activeInfo?.frozen_rows ?? 0) > 0 || (activeInfo?.frozen_cols ?? 0) > 0}
          onFreezeToggle={toggleFreeze}
          onHighlightRule={addHighlightRule}
        />

        <div className="toolbar__group">
          <button onClick={() => setFinding((v) => !v)} title="Find & replace (Cmd/Ctrl+F)">
            Find
          </button>
          {!standalone && (
            <button
              onClick={() => setShowRoutines((v) => !v)}
              aria-pressed={showRoutines}
              className={showRoutines ? 'is-on' : undefined}
              title="Work Gridline noticed you repeating"
            >
              Routines
            </button>
          )}
        </div>

        <div className="toolbar__spacer" />
        <span className="toolbar__status">{rangeA1(sel.range)}</span>
        {!standalone && (
          <>
            <CaptureChip
              state={capture.state}
              mode={capture.mode}
              dropped={capture.dropped}
              pending={capture.pending}
              rejected={capture.rejected}
              registration={capture.registration}
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
          </>
        )}
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
        functionNames={functionNames}
        onChange={wb.updateEdit}
        onCommit={() => wb.commitEdit('down')}
        onCancel={wb.cancelEdit}
        onBeginEdit={() => wb.startEdit(active)}
        onNameBox={handleNameBox}
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
              frozenRows={activeInfo?.frozen_rows ?? 0}
              frozenCols={activeInfo?.frozen_cols ?? 0}
              highlights={highlights}
              onSelect={wb.select}
              onStartEdit={wb.startEdit}
              onCommitEdit={(move: MoveDirection) => wb.commitEdit(move)}
              onCancelEdit={wb.cancelEdit}
              onEditValueChange={wb.updateEdit}
              onFill={handleFill}
              onContextMenu={handleContextMenu}
              onResize={handleResize}
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

      {!standalone && capture.needsConsent && (
        <ConsentModal
          onChoose={capture.choose}
          onOpenTransparency={() => navigate(TRANSPARENCY_PATH)}
        />
      )}
    </div>
  )
}
