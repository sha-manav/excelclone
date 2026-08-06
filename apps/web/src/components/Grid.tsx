/**
 * The canvas grid.
 *
 * The grid owns no spreadsheet data: every repaint asks the engine for exactly
 * one viewport covering the visible block, so scrolling a sheet with 50k
 * populated cells costs one bridge call per frame rather than one per cell.
 *
 * Scrolling is delegated to a real scroll container (an oversized spacer div)
 * so the browser gives us native momentum, scrollbars and trackpad behaviour;
 * the canvas floats above it and only ever paints the visible block. Because
 * the scroll offsets live in the DOM rather than in React state, a scroll
 * event triggers a repaint but never a re-render.
 *
 * All coordinate math lives in ./grid-geometry so it can be unit tested.
 */

import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from 'react'
import type {
  FocusEvent as ReactFocusEvent,
  JSX,
  KeyboardEvent as ReactKeyboardEvent,
  MouseEvent as ReactMouseEvent,
} from 'react'
import { KIND_ERROR, KIND_NUMBER } from '../engine/bridge'
import type { CellFormat, EngineHandle, Viewport } from '../engine/bridge'
import { colLetters, range as mkRange, rangeA1, rangeContains } from '../engine/actions'
import type { Addr, Axis, Range } from '../engine/actions'
import type { EditState, MoveDirection, Selection } from '../state/useWorkbook'
import { applyPointing, bandRef, pointingSlot } from './formula-pointing'
import type { PointingSlot } from './formula-pointing'
import { FunctionMenu, useCompletion } from './FunctionMenu'
import {
  MergeMap,
  OVERSCAN,
  autoscrollDelta,
  cellRect,
  clampColWidth,
  clampRowHeight,
  colWidth,
  columnAtX,
  colViewportX,
  frozenHeight,
  frozenWidth,
  createMetrics,
  fillHandleRect,
  fillTarget,
  firstVisibleRow,
  hitTest,
  lastVisibleRow,
  moveWithMerges,
  fitGeneralNumber,
  overflowHashes,
  pageJump,
  pointInRect,
  rangeRect,
  resolvedAlign,
  rowAtY,
  rowHeight,
  rowViewportY,
  scrollToInclude,
  selectionAt,
  selectionFocus,
  selectionFrom,
  scrollableHeight,
  scrollableWidth,
  usedEdge,
  virtualExtent,
  visibleRange,
} from './grid-geometry'
import type { CellAlign, GridMetrics } from './grid-geometry'

export interface GridProps {
  engine: EngineHandle
  sheet: string
  /** Bump to re-read cells from the engine and repaint. */
  version: number
  selection: Selection
  editing: EditState | null
  /** Rows hidden by a filter; skipped entirely in layout. */
  hiddenRows: number[]
  /** Merged ranges in A1 form, as the engine reports them. */
  merged: string[]
  /** Extent to make scrollable: includes cells that carry only formatting. */
  paintedRows: number
  paintedCols: number
  /** Rows and columns held still while the rest of the sheet scrolls. */
  frozenRows: number
  frozenCols: number
  onSelect(sel: Selection): void
  /** `initial` set means typing replaced the cell rather than opening it. */
  onStartEdit(addr: Addr, initial?: string): void
  onCommitEdit(move: MoveDirection): void
  onCancelEdit(): void
  onEditValueChange(value: string): void
  onFill(source: Range, target: Range): void
  onContextMenu(addr: Addr, clientX: number, clientY: number): void
  /**
   * A finished resize gesture. Sizes live in the engine, not here, so this is
   * how a drag becomes a fact: the grid previews the width while the pointer
   * is down and then hands the final number over to be applied, recorded and
   * saved like any other edit.
   */
  onResize(axis: Axis, at: number, count: number, size: number | null): void
  /** Ranges to wash, used by find to show where the matches are. */
  highlights?: readonly Range[]
}

const CELL_FONT = '12px ui-monospace, SFMono-Regular, Menlo, Consolas, monospace'
const HEADER_FONT =
  '11px -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, sans-serif'
const CELL_PAD = 5

const COLOR_BG = '#ffffff'
const COLOR_GRID = '#e0e0e0'
const COLOR_TEXT = '#1a1a1a'
const COLOR_ERROR = '#b3261e'
const COLOR_ACCENT = '#1e7e45'
const COLOR_WASH = 'rgba(30, 126, 69, 0.1)'
// The range a formula is currently pointing at. Deliberately not the accent:
// while pointing, the selection outline and the pointed outline are on screen
// at once and mean different things.
const COLOR_POINT = '#1a73e8'
const COLOR_POINT_WASH = 'rgba(26, 115, 232, 0.12)'
const COLOR_HEADER_BG = '#f5f5f5'
const COLOR_HEADER_ACTIVE = '#dbeae1'
const COLOR_HEADER_TEXT = '#555555'
const COLOR_HEADER_LINE = '#c8c8c8'
const COLOR_FORMULA_MARK = 'rgba(30, 126, 69, 0.5)'
const COLOR_BORDER = '#333333'
const COLOR_FIND_HIT = 'rgba(255, 196, 0, 0.35)'

/** Canvas font strings for the four bold/italic combinations, built once. */
const CELL_FONTS: Record<string, string> = {
  '': CELL_FONT,
  b: `bold ${CELL_FONT}`,
  i: `italic ${CELL_FONT}`,
  bi: `italic bold ${CELL_FONT}`,
}

function fontFor(f: CellFormat): string {
  return CELL_FONTS[`${f.bold ? 'b' : ''}${f.italic ? 'i' : ''}`]
}

const EMPTY_CELL_FORMAT: CellFormat = {}

/** How far down a column autofit looks before settling on a width. */
const AUTOFIT_SCAN_ROWS = 1000
/** How far a double-click fill will follow a neighbouring run. */
const FILL_DOWN_SCAN_ROWS = 10_000

/**
 * How far a fill-handle double-click should reach: the end of the contiguous
 * run of values in the column immediately left of the selection, falling back
 * to the column on its right.
 *
 * Returns null when there is no neighbouring run, rather than filling to the
 * bottom of the sheet — a gesture that silently wrote ten thousand rows would
 * be much worse than one that does nothing.
 */
function fillDownTarget(L: Latest): Range | null {
  const src = L.selection.range
  const probe = (col: number): number => {
    if (col < 0) return src.end.row
    const rows = Math.min(
      Math.max(L.usedRows - src.end.row - 1, 0),
      FILL_DOWN_SCAN_ROWS,
    )
    if (rows <= 0) return src.end.row
    const vp = L.engine.viewport(L.sheet, src.end.row + 1, col, rows, 1)
    let last = src.end.row
    for (let i = 0; i < rows; i++) {
      if (!vp.values[i]) break
      last = src.end.row + 1 + i
    }
    return last
  }
  if (!L.sheetExists) return null
  const end = Math.max(probe(src.start.col - 1), probe(src.end.col + 1))
  if (end <= src.end.row) return null
  return { start: src.start, end: { row: end, col: src.end.col } }
}

/**
 * What a point left behind: the span it wrote, and the text it wrote it into.
 *
 * Both are needed to point again. The slot has to widen to cover the reference
 * just written, or the next point appends instead of replacing; the value has
 * to be carried because the next point may arrive before React has re-rendered
 * with the last one.
 */
interface Pointed {
  slot: PointingSlot
  value: string
}

type Drag =
  | { kind: 'select'; anchor: Addr; last: Addr }
  // Pointing carries what the last point produced, so every mouse move
  // rewrites the *same* span rather than appending a reference per pixel.
  | { kind: 'point'; pointed: Pointed; anchor: Addr; last: Addr }
  // The same, dragged across headers: `A:C` rather than `A1:C1000`.
  | { kind: 'point-band'; axis: 'col' | 'row'; pointed: Pointed; anchor: number; last: number }
  | { kind: 'fill'; source: Range; target: Range }
  | { kind: 'resize'; col: number; startX: number; startWidth: number; width: number }
  | { kind: 'resize-row'; row: number; startY: number; startHeight: number; height: number }

interface Latest {
  engine: EngineHandle
  sheet: string
  version: number
  metrics: GridMetrics
  merges: MergeMap
  /** False for the one render after the sheet was renamed or deleted. */
  sheetExists: boolean
  highlights: readonly Range[]
  selection: Selection
  editing: EditState | null
  usedRows: number
  usedCols: number
  onSelect(sel: Selection): void
  onFill(source: Range, target: Range): void
  onStartEdit(addr: Addr, initial?: string): void
  onCommitEdit(move: MoveDirection): void
  onEditValueChange(value: string): void
  onContextMenu(addr: Addr, clientX: number, clientY: number): void
  onResize(axis: Axis, at: number, count: number, size: number | null): void
}

const EMPTY_HIGHLIGHTS: readonly Range[] = []

/** Arrow keys, as the direction the selection moves when one commits an edit. */
const EDITOR_ARROWS: Record<string, MoveDirection | undefined> = {
  ArrowUp: 'up',
  ArrowDown: 'down',
  ArrowLeft: 'left',
  ArrowRight: 'right',
}

/**
 * Keys that produce a keydown without being input.
 *
 * They have to be told from real keystrokes because a run of arrows picking a
 * reference is ended by anything else — and holding Shift to widen the range
 * fires a keydown for Shift itself, which ended the run on the very gesture it
 * was meant to serve.
 */
const BARE_MODIFIERS = new Set(['Shift', 'Control', 'Alt', 'Meta', 'CapsLock', 'AltGraph'])

const sameRange = (a: Range, b: Range): boolean =>
  a.start.row === b.start.row &&
  a.start.col === b.start.col &&
  a.end.row === b.end.row &&
  a.end.col === b.end.col

export function Grid(props: GridProps): JSX.Element {
  const {
    engine,
    sheet,
    version,
    selection,
    editing,
    hiddenRows,
    merged,
    paintedRows,
    paintedCols,
    frozenRows,
    frozenCols,
    highlights,
    onSelect,
    onStartEdit,
    onCommitEdit,
    onCancelEdit,
    onEditValueChange,
    onFill,
    onContextMenu,
    onResize,
  } = props

  const containerRef = useRef<HTMLDivElement>(null)
  const scrollRef = useRef<HTMLDivElement>(null)
  const canvasRef = useRef<HTMLCanvasElement>(null)
  const inputRef = useRef<HTMLInputElement>(null)

  // Sizes live in the engine. This holds only the width being dragged right
  // now, so the column follows the pointer without a round trip per pixel;
  // it is cleared the moment the real resize lands.
  const [preview, setPreview] = useState<{ col: number; width: number } | null>(null)
  const [rowPreview, setRowPreview] = useState<{ row: number; height: number } | null>(null)

  const sheetInfo = useMemo(() => {
    // `version` is never read here; it is the only signal that the used range
    // may have changed, which is what makes it a genuine dependency.
    void version
    return engine.sheets().find((s) => s.name === sheet) ?? null
  }, [engine, sheet, version])
  const usedRows = sheetInfo?.used_rows ?? 0
  const usedCols = sheetInfo?.used_cols ?? 0
  // Renaming or deleting a sheet leaves `sheet` naming one the engine no
  // longer has, for the single render before the parent notices. Asking for
  // its viewport throws, and the throw lands inside a requestAnimationFrame
  // callback where nothing can catch it.
  const sheetExists = sheetInfo !== null

  const colWidths = useMemo(() => {
    const m = new Map(sheetInfo?.col_widths ?? [])
    if (preview) m.set(preview.col, preview.width)
    return m
  }, [sheetInfo, preview])
  const rowHeights = useMemo(() => {
    const m = new Map(sheetInfo?.row_heights ?? [])
    if (rowPreview) m.set(rowPreview.row, rowPreview.height)
    return m
  }, [sheetInfo, rowPreview])

  // Read once: the list is fixed for the life of the build, and it crosses the
  // wasm boundary.
  const functionNames = useMemo(() => engine.functionNames(), [engine])
  const completion = useCompletion(inputRef, functionNames, onEditValueChange)

  const hiddenSet = useMemo(() => new Set(hiddenRows), [hiddenRows])
  const mergedKey = merged.join('|')
  const merges = useMemo(
    () => MergeMap.fromA1(mergedKey ? mergedKey.split('|') : []),
    [mergedKey],
  )

  const metrics = useMemo(() => {
    // The scrollable extent follows the *painted* range, so a bold empty
    // column below the data is still reachable.
    const extent = virtualExtent(
      Math.max(usedRows, paintedRows),
      Math.max(usedCols, paintedCols),
    )
    return createMetrics({
      colWidths,
      rowHeights,
      hiddenRows: hiddenSet,
      rowCount: extent.rows,
      colCount: extent.cols,
      frozenRows,
      frozenCols,
    })
  }, [
    colWidths,
    rowHeights,
    hiddenSet,
    usedRows,
    usedCols,
    paintedRows,
    paintedCols,
    frozenRows,
    frozenCols,
  ])

  // Everything the imperative layer (paint, window drag listeners, keyboard)
  // needs, refreshed every render so those handlers can stay identity-stable
  // and never close over stale props.
  const latest: Latest = {
    engine,
    sheet,
    version,
    metrics,
    merges,
    sheetExists,
    highlights: highlights ?? EMPTY_HIGHLIGHTS,
    selection,
    editing,
    usedRows,
    usedCols,
    onSelect,
    onFill,
    onStartEdit,
    onCommitEdit,
    onEditValueChange,
    onContextMenu,
    onResize,
  }
  const latestRef = useRef<Latest>(latest)
  latestRef.current = latest

  const cacheRef = useRef<{ key: string; panes: Viewport[] }>({ key: '', panes: [] })
  const dragRef = useRef<Drag | null>(null)
  const fillPreviewRef = useRef<Range | null>(null)
  const rafRef = useRef(0)
  /** The range the open formula is pointing at, drawn while it is being picked. */
  const pointingRef = useRef<Range | null>(null)
  /**
   * A run of arrow keys picking a reference, if one is in progress.
   *
   * Held rather than recomputed because shift-extension needs the anchor the
   * run started from, which the formula text cannot supply: `=A2:A4` does not
   * say whether the user started at A2 and reached down or started at A4 and
   * reached up, and extending the wrong end is a different range. Any keystroke
   * that is not an arrow ends the run.
   */
  const arrowPointRef = useRef<{ pointed: Pointed; anchor: Addr; focus: Addr } | null>(null)
  /**
   * True once an arrow has been used to move the caret in this edit.
   *
   * Excel's rule, and it is stickier than it looks: an arrow that lands
   * somewhere no reference is expected does not merely move the caret, it
   * drops the whole edit from enter mode into edit mode — so every later arrow
   * belongs to the caret too, even one that stops after an operator. Without
   * that, going back to fix `=1+2` would start pointing halfway through, and
   * the fix would be impossible to type.
   */
  const caretArrowRef = useRef(false)
  /**
   * A caret to restore once React has rendered the value pointing just wrote.
   *
   * A controlled input puts the caret at the end when its value is replaced
   * from the outside, which would leave the user typing after `)` instead of
   * where they were. The element is carried along because pointing can be
   * driven from the formula bar as easily as from the cell editor.
   */
  const pendingCaretRef = useRef<{ el: HTMLInputElement; caret: number } | null>(null)

  /* ------------------------------------------------------------- painting */

  const paint = useCallback(() => {
    const canvas = canvasRef.current
    const scroller = scrollRef.current
    if (!canvas || !scroller) return
    const cssW = scroller.clientWidth
    const cssH = scroller.clientHeight
    if (cssW <= 0 || cssH <= 0) return
    const ctx = canvas.getContext('2d')
    if (!ctx) return

    // The backing store is sized in device pixels while every coordinate below
    // is in CSS pixels; scaling the context by dpr bridges the two so text is
    // rasterised at full resolution. Assigning width/height also clears the
    // canvas and resets its transform, so the transform is set every frame.
    const dpr = window.devicePixelRatio || 1
    const pxW = Math.round(cssW * dpr)
    const pxH = Math.round(cssH * dpr)
    if (canvas.width !== pxW || canvas.height !== pxH) {
      canvas.width = pxW
      canvas.height = pxH
      canvas.style.width = `${cssW}px`
      canvas.style.height = `${cssH}px`
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0)

    const L = latestRef.current
    const m = L.metrics
    const hw = m.headerWidth
    const hh = m.headerHeight
    const scrollTop = scroller.scrollTop
    const scrollLeft = scroller.scrollLeft

    ctx.fillStyle = COLOR_BG
    ctx.fillRect(0, 0, cssW, cssH)

    const vis = visibleRange(scrollTop, scrollLeft, cssW, cssH, m)

    // What to paint, in order: the frozen band first and then the scrolling
    // region. A *list* rather than a range because the two are not adjacent
    // in the sheet — scrolled down, row 1 sits directly above row 900 — and
    // every loop below indexes this rather than counting from a first row.
    const rowsAt: number[] = []
    for (let r = 0; r < m.frozenRows; r++) rowsAt.push(r)
    for (let r = vis.firstRow; r <= vis.lastRow; r++) rowsAt.push(r)
    const colsAt: number[] = []
    for (let c = 0; c < m.frozenCols; c++) colsAt.push(c)
    for (let c = vis.firstCol; c <= vis.lastCol; c++) colsAt.push(c)
    const nRows = rowsAt.length
    const nCols = colsAt.length
    if (nRows <= 0 || nCols <= 0) return

    // Per-frame edge tables: one allocation each, never one per cell. Each
    // entry is asked for its own position rather than accumulated from the
    // one before, because the step from the last frozen row to the first
    // scrolling row is not that row's height.
    const xs = new Array<number>(nCols + 1)
    for (let i = 0; i < nCols; i++) xs[i] = colViewportX(m, colsAt[i], scrollLeft)
    xs[nCols] = xs[nCols - 1] + colWidth(m, colsAt[nCols - 1])
    const ys = new Array<number>(nRows + 1)
    for (let i = 0; i < nRows; i++) ys[i] = rowViewportY(m, rowsAt[i], scrollTop)
    ys[nRows] = ys[nRows - 1] + rowHeight(m, rowsAt[nRows - 1])

    // Engine calls per repaint, cached so plain scrolling inside the overscan
    // band does not re-cross the wasm boundary. One rectangle when nothing is
    // frozen; up to four — the quadrants — when something is, because the
    // frozen band and the scrolling region are far apart and one rectangle
    // spanning both would fetch every row in between.
    const rowBands: [number, number][] = [[vis.firstRow, vis.lastRow]]
    if (m.frozenRows > 0) rowBands.unshift([0, m.frozenRows - 1])
    const colBands: [number, number][] = [[vis.firstCol, vis.lastCol]]
    if (m.frozenCols > 0) colBands.unshift([0, m.frozenCols - 1])
    const wanted: [number, number, number, number][] = []
    for (const [ra, rb] of rowBands) {
      for (const [ca, cb] of colBands) {
        const r0 = Math.max(0, ra - OVERSCAN)
        const c0 = Math.max(0, ca - OVERSCAN)
        const rows = Math.min(m.rowCount, rb + OVERSCAN + 1) - r0
        const cols = Math.min(m.colCount, cb + OVERSCAN + 1) - c0
        if (rows > 0 && cols > 0) wanted.push([r0, c0, rows, cols])
      }
    }
    const key = `${L.sheet}|${L.version}|${wanted.map((w) => w.join(':')).join('|')}`
    if (cacheRef.current.key !== key) {
      cacheRef.current = {
        key,
        panes: L.sheetExists
          ? wanted.map(([r0, c0, rows, cols]) =>
              L.engine.viewport(L.sheet, r0, c0, rows, cols),
            )
          : [],
      }
    }
    const panes = cacheRef.current.panes
    /** The viewport holding a cell and its index in it, or null. */
    const lookup = (row: number, col: number): [Viewport, number] | null => {
      for (const p of panes) {
        const vr = row - p.row0
        const vc = col - p.col0
        if (vr >= 0 && vr < p.rows && vc >= 0 && vc < p.cols) {
          return [p, vr * p.cols + vc]
        }
      }
      return null
    }

    const selRect = rangeRect(m, L.selection.range, scrollTop, scrollLeft)
    const multi =
      L.selection.range.start.row !== L.selection.range.end.row ||
      L.selection.range.start.col !== L.selection.range.end.col

    ctx.save()
    ctx.beginPath()
    ctx.rect(hw, hh, cssW - hw, cssH - hh)
    ctx.clip()

    // The palette entry for a visible cell; index 0 is always the default.
    const formatAt = (ri: number, ci: number): CellFormat => {
      const found = lookup(rowsAt[ri], colsAt[ci])
      if (!found) return EMPTY_CELL_FORMAT
      const [p, idx] = found
      return p.palette[p.styles[idx]] ?? EMPTY_CELL_FORMAT
    }

    // Fills go down first, under the gridlines, exactly as in Excel.
    for (let ri = 0; ri < nRows; ri++) {
      const h = ys[ri + 1] - ys[ri]
      if (h <= 0) continue
      for (let ci = 0; ci < nCols; ci++) {
        const fill = formatAt(ri, ci).fill_color
        if (!fill) continue
        ctx.fillStyle = fill
        ctx.fillRect(xs[ci], ys[ri], xs[ci + 1] - xs[ci], h)
      }
    }

    // Grid lines as a single path; the half-pixel offset keeps 1px strokes
    // from straddling two device pixels and going grey.
    ctx.strokeStyle = COLOR_GRID
    ctx.lineWidth = 1
    ctx.beginPath()
    for (let i = 0; i <= nCols; i++) {
      const x = Math.round(xs[i]) + 0.5
      ctx.moveTo(x, hh)
      ctx.lineTo(x, cssH)
    }
    for (let i = 0; i <= nRows; i++) {
      if (i > 0 && ys[i] === ys[i - 1]) continue
      const y = Math.round(ys[i]) + 0.5
      ctx.moveTo(hw, y)
      ctx.lineTo(cssW, y)
    }
    ctx.stroke()

    // A merged block is one cell to the eye: repaint over it to erase the
    // interior gridlines, then put the single outline back.
    const mergesToPaint = L.merges.isEmpty
      ? []
      : L.merges.ranges.filter(
          (r) =>
            r.end.row >= rowsAt[0] &&
            r.start.row <= rowsAt[nRows - 1] &&
            r.end.col >= colsAt[0] &&
            r.start.col <= colsAt[nCols - 1],
        )
    for (const mr of mergesToPaint) {
      const rect = rangeRect(m, mr, scrollTop, scrollLeft)
      if (rect.w <= 0 || rect.h <= 0) continue
      const anchor = lookup(mr.start.row, mr.start.col)
      const anchorFill = anchor
        ? (anchor[0].palette[anchor[0].styles[anchor[1]]] ?? EMPTY_CELL_FORMAT).fill_color
        : undefined
      ctx.fillStyle = anchorFill ?? COLOR_BG
      ctx.fillRect(rect.x, rect.y, rect.w, rect.h)
      ctx.strokeStyle = COLOR_GRID
      ctx.lineWidth = 1
      ctx.strokeRect(
        Math.round(rect.x) + 0.5,
        Math.round(rect.y) + 0.5,
        Math.round(rect.w) - 1,
        Math.round(rect.h) - 1,
      )
    }

    for (const hl of L.highlights) {
      const r = rangeRect(m, hl, scrollTop, scrollLeft)
      if (r.w <= 0 || r.h <= 0) continue
      ctx.fillStyle = COLOR_FIND_HIT
      ctx.fillRect(r.x, r.y, r.w, r.h)
    }

    if (multi) {
      ctx.fillStyle = COLOR_WASH
      ctx.fillRect(selRect.x, selRect.y, selRect.w, selRect.h)
    }

    if (panes.length > 0) {
      ctx.font = CELL_FONT
      ctx.textBaseline = 'middle'
      ctx.textAlign = 'left'
      let align: CellAlign = 'left'
      let font = CELL_FONT
      const hashWidth = ctx.measureText('#').width

      for (let ri = 0; ri < nRows; ri++) {
        const h = ys[ri + 1] - ys[ri]
        if (h <= 0) continue
        if (ys[ri] > cssH || ys[ri + 1] < hh) continue
        const midY = ys[ri] + h / 2
        const row = rowsAt[ri]

        for (let ci = 0; ci < nCols; ci++) {
          const col = colsAt[ci]
          const found = lookup(row, col)
          if (!found) continue
          const [vp, idx] = found
          const text = vp.values[idx]
          if (!text) continue

          // Text belongs to the merge's anchor and spans the whole block; a
          // covered cell holds no value, but a stale one must not surface.
          const merge = L.merges.isEmpty ? null : L.merges.at(row, col)
          if (merge && (merge.start.row !== row || merge.start.col !== col)) continue
          let left = xs[ci]
          let right = xs[ci + 1]
          if (merge) {
            const rect = rangeRect(m, merge, scrollTop, scrollLeft)
            left = rect.x
            right = rect.x + rect.w
          }
          const w = right - left
          const avail = w - CELL_PAD * 2
          if (avail <= 0) continue

          const kind = vp.kinds[idx]
          const style = vp.palette[vp.styles[idx]] ?? EMPTY_CELL_FORMAT
          const wantedFont = fontFor(style)
          if (wantedFont !== font) {
            font = wantedFont
            ctx.font = wantedFont
          }
          const wanted = resolvedAlign(kind, style.align)
          if (wanted !== align) {
            align = wanted
            ctx.textAlign = wanted
          }
          ctx.fillStyle =
            kind === KIND_ERROR ? COLOR_ERROR : (style.font_color ?? COLOR_TEXT)

          // measureText is the expensive call here, so skip it whenever the
          // string is obviously short enough for the column.
          let out = text
          let clip = false
          if (text.length * 6 > avail && ctx.measureText(text).width > avail) {
            if (kind !== KIND_NUMBER) {
              clip = true
            } else {
              // General is "as much precision as there is room for", so how
              // many decimals a number shows is a question about the column,
              // not about the value — and only this side knows the column. A
              // cell with an explicit format is left alone: somebody asked for
              // those digits, and quietly showing fewer would be a lie about
              // what the cell says.
              const fitted =
                style.number_format === undefined
                  ? fitGeneralNumber(text, avail, (s) => ctx.measureText(s).width)
                  : null
              out = fitted ?? overflowHashes(avail, hashWidth)
            }
          }

          if (clip) {
            ctx.save()
            ctx.beginPath()
            ctx.rect(left, ys[ri], w, h)
            ctx.clip()
          }
          const tx =
            align === 'left'
              ? left + CELL_PAD
              : align === 'right'
                ? right - CELL_PAD
                : (left + right) / 2
          ctx.fillText(out, tx, midY)
          if (clip) ctx.restore()

          if (vp.formulas[idx]) {
            ctx.fillStyle = COLOR_FORMULA_MARK
            ctx.fillRect(xs[ci] + 1, ys[ri] + 1, 3, 3)
          }
        }
      }
      ctx.font = CELL_FONT
    }

    // Explicit borders go over the gridlines, so a thin black edge reads as
    // deliberate rather than as a slightly darker gridline.
    ctx.strokeStyle = COLOR_BORDER
    ctx.lineWidth = 1
    ctx.beginPath()
    for (let ri = 0; ri < nRows; ri++) {
      const h = ys[ri + 1] - ys[ri]
      if (h <= 0) continue
      for (let ci = 0; ci < nCols; ci++) {
        const b = formatAt(ri, ci).borders
        if (!b) continue
        const x0 = Math.round(xs[ci]) + 0.5
        const x1 = Math.round(xs[ci + 1]) - 0.5
        const y0 = Math.round(ys[ri]) + 0.5
        const y1 = Math.round(ys[ri + 1]) - 0.5
        if (b.top) {
          ctx.moveTo(x0, y0)
          ctx.lineTo(x1, y0)
        }
        if (b.bottom) {
          ctx.moveTo(x0, y1)
          ctx.lineTo(x1, y1)
        }
        if (b.left) {
          ctx.moveTo(x0, y0)
          ctx.lineTo(x0, y1)
        }
        if (b.right) {
          ctx.moveTo(x1, y0)
          ctx.lineTo(x1, y1)
        }
      }
    }
    ctx.stroke()

    // Selection chrome sits above the text but below the headers.
    ctx.textAlign = 'left'
    if (multi) {
      ctx.strokeStyle = COLOR_ACCENT
      ctx.lineWidth = 1
      ctx.strokeRect(
        Math.round(selRect.x) + 0.5,
        Math.round(selRect.y) + 0.5,
        Math.round(selRect.w) - 1,
        Math.round(selRect.h) - 1,
      )
    }
    const act = cellRect(m, L.selection.anchor.row, L.selection.anchor.col, scrollTop, scrollLeft)
    if (act.h > 0) {
      ctx.strokeStyle = COLOR_ACCENT
      ctx.lineWidth = 2
      ctx.strokeRect(act.x + 1, act.y + 1, act.w - 2, act.h - 2)
    }

    // The range the open formula points at. Painted after the selection so it
    // reads as the thing currently being chosen, and washed as well as outlined
    // because a formula being built over a range nobody can see is the same
    // problem the pointing gesture exists to solve.
    const pointed = L.editing ? pointingRef.current : null
    if (pointed) {
      const pr = rangeRect(m, pointed, scrollTop, scrollLeft)
      // A whole column is a rectangle 25 million pixels tall. Clamping to a
      // little past the viewport keeps the coordinates in a range the canvas
      // draws accurately, and loses nothing: an edge that far off screen was
      // never going to be seen, and a reference that reaches past the fold
      // ought not to look like it stops there.
      const x0 = Math.max(pr.x, -4)
      const y0 = Math.max(pr.y, -4)
      const x1 = Math.min(pr.x + pr.w, cssW + 4)
      const y1 = Math.min(pr.y + pr.h, cssH + 4)
      if (x1 > x0 && y1 > y0) {
        ctx.fillStyle = COLOR_POINT_WASH
        ctx.fillRect(x0, y0, x1 - x0, y1 - y0)
        ctx.setLineDash([4, 3])
        ctx.strokeStyle = COLOR_POINT
        ctx.lineWidth = 2
        ctx.strokeRect(
          Math.round(x0) + 1,
          Math.round(y0) + 1,
          Math.round(x1 - x0) - 2,
          Math.round(y1 - y0) - 2,
        )
        ctx.setLineDash([])
      }
    }

    const preview = fillPreviewRef.current
    if (preview) {
      const pr = rangeRect(m, preview, scrollTop, scrollLeft)
      ctx.setLineDash([4, 3])
      ctx.strokeStyle = COLOR_ACCENT
      ctx.lineWidth = 1
      ctx.strokeRect(
        Math.round(pr.x) + 0.5,
        Math.round(pr.y) + 0.5,
        Math.round(pr.w) - 1,
        Math.round(pr.h) - 1,
      )
      ctx.setLineDash([])
    } else {
      const fh = fillHandleRect(m, L.selection.range, scrollTop, scrollLeft)
      ctx.fillStyle = COLOR_BG
      ctx.fillRect(fh.x - 1, fh.y - 1, fh.w + 2, fh.h + 2)
      ctx.fillStyle = COLOR_ACCENT
      ctx.fillRect(fh.x, fh.y, fh.w, fh.h)
    }
    ctx.restore()

    /* ---------------------------------------------------------- headers */

    ctx.fillStyle = COLOR_HEADER_BG
    ctx.fillRect(0, 0, cssW, hh)
    ctx.fillRect(0, 0, hw, cssH)

    const selR = L.selection.range
    ctx.save()
    ctx.beginPath()
    ctx.rect(hw, 0, cssW - hw, hh)
    ctx.clip()
    ctx.fillStyle = COLOR_HEADER_ACTIVE
    for (let ci = 0; ci < nCols; ci++) {
      const col = colsAt[ci]
      if (col < selR.start.col || col > selR.end.col) continue
      ctx.fillRect(xs[ci], 0, xs[ci + 1] - xs[ci], hh)
    }
    ctx.font = HEADER_FONT
    ctx.fillStyle = COLOR_HEADER_TEXT
    ctx.textAlign = 'center'
    ctx.textBaseline = 'middle'
    ctx.strokeStyle = COLOR_GRID
    ctx.lineWidth = 1
    ctx.beginPath()
    for (let ci = 0; ci < nCols; ci++) {
      ctx.fillText(colLetters(vis.firstCol + ci), (xs[ci] + xs[ci + 1]) / 2, hh / 2)
      const x = Math.round(xs[ci + 1]) + 0.5
      ctx.moveTo(x, 0)
      ctx.lineTo(x, hh)
    }
    ctx.stroke()
    ctx.restore()

    ctx.save()
    ctx.beginPath()
    ctx.rect(0, hh, hw, cssH - hh)
    ctx.clip()
    ctx.fillStyle = COLOR_HEADER_ACTIVE
    for (let ri = 0; ri < nRows; ri++) {
      const row = vis.firstRow + ri
      if (row < selR.start.row || row > selR.end.row) continue
      const h = ys[ri + 1] - ys[ri]
      if (h > 0) ctx.fillRect(0, ys[ri], hw, h)
    }
    ctx.font = HEADER_FONT
    ctx.fillStyle = COLOR_HEADER_TEXT
    ctx.textAlign = 'center'
    ctx.strokeStyle = COLOR_GRID
    ctx.beginPath()
    for (let ri = 0; ri < nRows; ri++) {
      const h = ys[ri + 1] - ys[ri]
      if (h <= 0) continue
      ctx.fillText(String(vis.firstRow + ri + 1), hw / 2, ys[ri] + h / 2)
      const y = Math.round(ys[ri + 1]) + 0.5
      ctx.moveTo(0, y)
      ctx.lineTo(hw, y)
    }
    ctx.stroke()
    // A doubled rule marks where a filter swallowed one or more rows.
    ctx.strokeStyle = COLOR_ACCENT
    ctx.beginPath()
    for (let ri = 0; ri < nRows; ri++) {
      const row = vis.firstRow + ri
      if (row === 0 || !m.hiddenRows.has(row - 1)) continue
      const y = Math.round(ys[ri]) + 0.5
      ctx.moveTo(2, y)
      ctx.lineTo(hw - 2, y)
      ctx.moveTo(2, y + 2)
      ctx.lineTo(hw - 2, y + 2)
    }
    ctx.stroke()
    ctx.restore()

    ctx.fillStyle = COLOR_HEADER_BG
    ctx.fillRect(0, 0, hw, hh)
    ctx.strokeStyle = COLOR_HEADER_LINE
    ctx.lineWidth = 1
    ctx.beginPath()
    ctx.moveTo(Math.round(hw) + 0.5, 0)
    ctx.lineTo(Math.round(hw) + 0.5, cssH)
    ctx.moveTo(0, Math.round(hh) + 0.5)
    ctx.lineTo(cssW, Math.round(hh) + 0.5)
    ctx.stroke()

    // Keep the editor glued to its cell while the sheet scrolls under it.
    const ed = L.editing
    const input = inputRef.current
    if (ed && input) {
      const box = cellRect(m, ed.addr.row, ed.addr.col, scrollTop, scrollLeft)
      input.style.left = `${box.x}px`
      input.style.top = `${box.y}px`
      input.style.width = `${box.w}px`
      input.style.height = `${Math.max(box.h, m.defaultRowHeight)}px`
    }
  }, [])

  const invalidate = useCallback(() => {
    if (rafRef.current) return
    rafRef.current = requestAnimationFrame(() => {
      rafRef.current = 0
      paint()
    })
  }, [paint])

  // Repaint after every render (props, selection, sizes) and every scroll;
  // the rAF above collapses a burst of invalidations into one frame.
  useEffect(invalidate)

  useEffect(
    () => () => {
      if (rafRef.current) cancelAnimationFrame(rafRef.current)
      // Clearing the handle matters as much as cancelling it: StrictMode
      // mounts, cleans up, then mounts again, and a stale non-zero handle
      // would make every later invalidate() early-return and the grid would
      // never paint at all.
      rafRef.current = 0
    },
    [],
  )

  useEffect(() => {
    const el = scrollRef.current
    if (!el || typeof ResizeObserver === 'undefined') return
    const ro = new ResizeObserver(() => invalidate())
    ro.observe(el)
    return () => ro.disconnect()
  }, [invalidate])

  /* ---------------------------------------------------------- pointer ops */

  const pointOf = useCallback((clientX: number, clientY: number) => {
    const el = scrollRef.current
    if (!el) return { x: 0, y: 0, scrollTop: 0, scrollLeft: 0 }
    const rect = el.getBoundingClientRect()
    return {
      x: clientX - rect.left,
      y: clientY - rect.top,
      scrollTop: el.scrollTop,
      scrollLeft: el.scrollLeft,
    }
  }, [])

  /**
   * The input a pointed reference should be written into.
   *
   * Usually the cell editor, but the formula bar edits the same `editing`
   * state, and pointing from there has to put the reference where *that*
   * caret is rather than where the hidden one was.
   */
  const pointingInput = useCallback((): HTMLInputElement | null => {
    const active = document.activeElement
    if (active instanceof HTMLInputElement && active.classList.contains('formula-bar__input')) {
      return active
    }
    return inputRef.current
  }, [])

  /**
   * Write `a`..`b` into the formula at `slot`, keeping the editor focused, and
   * return the slot the *next* point should use.
   *
   * That return value is the whole reason this is not a void function. A drag
   * calls it once per cell crossed, and the slot it started with is an
   * insertion — so without widening it to cover what was just written, every
   * mouse move leaves its reference behind and `=AVERAGE(` grows into
   * `=AVERAGE(A1:A3A1:A2A1`.
   */
  const pointAt = useCallback(
    (slot: PointingSlot, a: Addr, b: Addr, base?: string): Pointed | null => {
      const L = latestRef.current
      if (!L.editing) return null
      // `base` is the text the *previous* point produced. Several points can
      // land between two renders — a fast drag, a held arrow key — and
      // `L.editing.value` only catches up when React re-renders, so splicing
      // into it would undo the point before last.
      const source = base ?? L.editing.value
      const range = L.merges.expand(selectionFrom(a, b).range)
      const ref = rangeA1(range)
      const next = applyPointing(source, slot, ref)
      pointingRef.current = range
      const el = pointingInput()
      if (el) pendingCaretRef.current = { el, caret: next.caret }
      L.onEditValueChange(next.value)
      invalidate()
      return { slot: { start: slot.start, end: slot.start + ref.length }, value: next.value }
    },
    [invalidate, pointingInput],
  )

  /**
   * Write `A:C` or `1:5` into the formula: a band of whole columns or rows,
   * picked off the headers.
   *
   * The outline covers the whole band, which is a rectangle a million rows
   * tall — clipping that to the viewport is the paint code's job, not this
   * one's, because the reference genuinely does reach past the fold.
   */
  const pointBand = useCallback(
    (slot: PointingSlot, axis: 'col' | 'row', a: number, b: number, base?: string): Pointed | null => {
      const L = latestRef.current
      if (!L.editing) return null
      const m = L.metrics
      const lo = Math.min(a, b)
      const hi = Math.max(a, b)
      const ref = bandRef(axis, lo, hi)
      const next = applyPointing(base ?? L.editing.value, slot, ref)
      pointingRef.current =
        axis === 'col'
          ? { start: { row: 0, col: lo }, end: { row: Math.max(0, m.rowCount - 1), col: hi } }
          : { start: { row: lo, col: 0 }, end: { row: hi, col: Math.max(0, m.colCount - 1) } }
      const el = pointingInput()
      if (el) pendingCaretRef.current = { el, caret: next.caret }
      L.onEditValueChange(next.value)
      invalidate()
      return { slot: { start: slot.start, end: slot.start + ref.length }, value: next.value }
    },
    [invalidate, pointingInput],
  )

  /**
   * Move the pointed reference with an arrow key. Returns false when the
   * formula is not expecting one, so the caret keeps the keystroke.
   *
   * The first arrow of a run starts one step from the cell being edited, which
   * is what makes `=` then Up mean "the cell above". Later arrows walk from
   * where the run has got to; shift walks the far end and leaves the anchor,
   * so a range can be widened and narrowed again without restarting.
   */
  const pointByArrow = useCallback(
    (dir: MoveDirection, extend: boolean, toEdge: boolean): boolean => {
      const L = latestRef.current
      if (!L.editing || dir === 'none') return false
      const m = L.metrics
      const step = (from: Addr): Addr =>
        toEdge
          ? usedEdge(m, from, dir, L.usedRows, L.usedCols)
          : moveWithMerges(m, L.merges, from, dir)

      const run = arrowPointRef.current
      if (run) {
        const focus = step(extend ? run.focus : run.anchor)
        const anchor = extend ? run.anchor : focus
        const next = pointAt(run.pointed.slot, anchor, focus, run.pointed.value)
        if (!next) return false
        arrowPointRef.current = { pointed: next, anchor, focus }
        return true
      }

      // Starting fresh: only if a reference may go in where the caret is.
      const caret = pointingInput()?.selectionStart ?? L.editing.value.length
      const slot = pointingSlot(L.editing.value, caret)
      if (!slot) return false
      const focus = step(L.editing.addr)
      const next = pointAt(slot, focus, focus)
      if (!next) return false
      arrowPointRef.current = { pointed: next, anchor: focus, focus }
      return true
    },
    [pointAt, pointingInput],
  )

  /** Extend the drag to the cell under the pointer, in content coordinates. */
  const applyDragAt = useCallback(
    (clientX: number, clientY: number) => {
      const d = dragRef.current
      if (!d || d.kind === 'resize' || d.kind === 'resize-row') return
      const L = latestRef.current
      const m = L.metrics
      const p = pointOf(clientX, clientY)
      const row = rowAtY(m, Math.max(0, p.y - m.headerHeight) + p.scrollTop)
      const col = columnAtX(m, Math.max(0, p.x - m.headerWidth) + p.scrollLeft)

      if (d.kind === 'select') {
        if (row === d.last.row && col === d.last.col) return
        d.last = { row, col }
        const sel = selectionFrom(d.anchor, { row, col })
        L.onSelect({ anchor: sel.anchor, range: L.merges.expand(sel.range) })
        return
      }
      if (d.kind === 'point') {
        if (row === d.last.row && col === d.last.col) return
        d.last = { row, col }
        const next = pointAt(d.pointed.slot, d.anchor, d.last, d.pointed.value)
        if (next) d.pointed = next
        return
      }
      if (d.kind === 'point-band') {
        const at = d.axis === 'col' ? col : row
        if (at === d.last) return
        d.last = at
        const next = pointBand(d.pointed.slot, d.axis, d.anchor, at, d.pointed.value)
        if (next) d.pointed = next
        return
      }
      const target = fillTarget(d.source, row, col)
      if (sameRange(target, d.target)) return
      d.target = target
      fillPreviewRef.current = target
      invalidate()
    },
    [invalidate, pointAt, pointBand, pointOf],
  )

  /**
   * Keep scrolling — and keep extending the selection — while the pointer
   * sits outside the content box. Without this a drag simply stops at the
   * edge and there is no way to select past the fold with the mouse.
   */
  const autoscrollRef = useRef(0)
  const pointerRef = useRef({ x: 0, y: 0 })

  const stopAutoscroll = useCallback(() => {
    if (autoscrollRef.current) {
      cancelAnimationFrame(autoscrollRef.current)
      autoscrollRef.current = 0
    }
  }, [])

  const stepAutoscroll = useCallback(() => {
    autoscrollRef.current = 0
    const el = scrollRef.current
    if (!dragRef.current || !el) return
    const p = pointOf(pointerRef.current.x, pointerRef.current.y)
    const { dx, dy } = autoscrollDelta(
      p.x,
      p.y,
      el.clientWidth,
      el.clientHeight,
      latestRef.current.metrics,
    )
    if (dx === 0 && dy === 0) return
    el.scrollLeft += dx
    el.scrollTop += dy
    applyDragAt(pointerRef.current.x, pointerRef.current.y)
    autoscrollRef.current = requestAnimationFrame(stepAutoscroll)
  }, [applyDragAt, pointOf])

  const handleDragMove = useCallback(
    (e: MouseEvent) => {
      const d = dragRef.current
      const el = scrollRef.current
      if (!d || !el) return

      if (d.kind === 'resize') {
        const width = clampColWidth(d.startWidth + (e.clientX - d.startX))
        d.width = width
        setPreview((prev) =>
          prev && prev.col === d.col && prev.width === width ? prev : { col: d.col, width },
        )
        return
      }

      if (d.kind === 'resize-row') {
        const height = clampRowHeight(d.startHeight + (e.clientY - d.startY))
        d.height = height
        setRowPreview((prev) =>
          prev && prev.row === d.row && prev.height === height ? prev : { row: d.row, height },
        )
        return
      }

      pointerRef.current = { x: e.clientX, y: e.clientY }
      applyDragAt(e.clientX, e.clientY)

      const p = pointOf(e.clientX, e.clientY)
      const { dx, dy } = autoscrollDelta(
        p.x,
        p.y,
        el.clientWidth,
        el.clientHeight,
        latestRef.current.metrics,
      )
      if (dx !== 0 || dy !== 0) {
        if (!autoscrollRef.current) {
          autoscrollRef.current = requestAnimationFrame(stepAutoscroll)
        }
      } else {
        stopAutoscroll()
      }
    },
    [applyDragAt, pointOf, stepAutoscroll, stopAutoscroll],
  )

  const handleDragEnd = useCallback(() => {
    const d = dragRef.current
    dragRef.current = null
    stopAutoscroll()
    window.removeEventListener('mousemove', handleDragMove)
    window.removeEventListener('mouseup', handleDragEnd)
    if (d && d.kind === 'fill') {
      fillPreviewRef.current = null
      if (!sameRange(d.target, d.source)) latestRef.current.onFill(d.source, d.target)
      invalidate()
    }
    if (d && d.kind === 'resize') {
      // A drag that ended where it started is not a resize, and recording one
      // would put a no-op on the undo stack and in the capture log.
      if (d.width !== d.startWidth) {
        latestRef.current.onResize('col', d.col, 1, d.width)
      }
      setPreview(null)
      invalidate()
    }
    if (d && d.kind === 'resize-row') {
      if (d.height !== d.startHeight) {
        latestRef.current.onResize('row', d.row, 1, d.height)
      }
      setRowPreview(null)
      invalidate()
    }
  }, [handleDragMove, invalidate, stopAutoscroll])

  const beginDrag = useCallback(
    (drag: Drag) => {
      dragRef.current = drag
      window.addEventListener('mousemove', handleDragMove)
      window.addEventListener('mouseup', handleDragEnd)
    },
    [handleDragEnd, handleDragMove],
  )

  useEffect(
    () => () => {
      window.removeEventListener('mousemove', handleDragMove)
      window.removeEventListener('mouseup', handleDragEnd)
      stopAutoscroll()
    },
    [handleDragEnd, handleDragMove, stopAutoscroll],
  )

  const handleMouseDown = useCallback(
    (e: ReactMouseEvent<HTMLDivElement>) => {
      if (e.button !== 0) return
      const el = scrollRef.current
      if (!el) return
      const L = latestRef.current
      const m = L.metrics
      const p = pointOf(e.clientX, e.clientY)
      // Presses on the native scrollbars land inside the element but outside
      // its client box; they belong to the browser, not to the selection.
      if (p.x >= el.clientWidth || p.y >= el.clientHeight) {
        if (L.editing) L.onCommitEdit('none')
        containerRef.current?.focus()
        return
      }

      // Pointing: a formula that is mid-expression captures the press and
      // turns it into a reference, which is the gesture behind "=AVERAGE(",
      // drag, ")". `preventDefault` is what keeps the editor focused — without
      // it the press blurs the input, blur commits, and the user is left
      // looking at the parse error for the half-written formula they were
      // still composing.
      const early = hitTest(p.x, p.y, p.scrollTop, p.scrollLeft, m)
      const pointableHit =
        early.kind === 'cell' || early.kind === 'col-header' || early.kind === 'row-header'
      if (L.editing && pointableHit) {
        const caret = pointingInput()?.selectionStart ?? L.editing.value.length
        const slot = pointingSlot(L.editing.value, caret)
        if (slot) {
          e.preventDefault()
          pointerRef.current = { x: e.clientX, y: e.clientY }
          arrowPointRef.current = null
          if (early.kind === 'cell') {
            const addr = L.merges.anchor(early.row, early.col)
            const last = { row: early.row, col: early.col }
            const pointed = pointAt(slot, addr, last)
            if (pointed) beginDrag({ kind: 'point', pointed, anchor: addr, last })
            return
          }
          // A header names a whole band, which is what `A:C` and `1:5` are for
          // — and the reason they had to exist in the engine before this
          // gesture could do anything but produce a formula it rejects.
          const axis = early.kind === 'col-header' ? 'col' : 'row'
          const at = early.kind === 'col-header' ? early.col : early.row
          const pointed = pointBand(slot, axis, at, at)
          if (pointed) beginDrag({ kind: 'point-band', axis, pointed, anchor: at, last: at })
          return
        }
      }

      // A press on the grid ends any edit in progress, exactly as it does in
      // Excel: what was typed lands in the cell it was typed into, and the
      // selection is then free to follow the mouse.
      //
      // Leaving the editor open here was the single worst thing about this
      // grid. The selection moved underneath a floating input that still had
      // focus, so the cell you had clicked never looked selected, dragging a
      // range appeared to do nothing, and Delete went into the input instead
      // of clearing the cells — the app read as unresponsive rather than
      // merely unfinished. Committing first is also what makes the focus()
      // below safe: it blurs the input, and blur commits too, so the two
      // paths have to agree on the outcome.
      if (L.editing) L.onCommitEdit('none')
      containerRef.current?.focus()

      // The fill handle overlaps whatever cell it sits on, so it wins.
      if (p.x >= m.headerWidth && p.y >= m.headerHeight) {
        const fh = fillHandleRect(m, L.selection.range, p.scrollTop, p.scrollLeft)
        if (pointInRect(p.x, p.y, fh, 1)) {
          fillPreviewRef.current = L.selection.range
          beginDrag({ kind: 'fill', source: L.selection.range, target: L.selection.range })
          invalidate()
          return
        }
      }

      const hit = early
      switch (hit.kind) {
        case 'corner':
          L.onSelect(
            selectionFrom(
              { row: 0, col: 0 },
              { row: Math.max(0, L.usedRows - 1), col: Math.max(0, L.usedCols - 1) },
            ),
          )
          return
        case 'col-border':
          beginDrag({
            kind: 'resize',
            col: hit.col,
            startX: e.clientX,
            startWidth: colWidth(m, hit.col),
            width: colWidth(m, hit.col),
          })
          return
        case 'col-header':
          L.onSelect({
            anchor: { row: firstVisibleRow(m), col: hit.col },
            range: mkRange(
              { row: firstVisibleRow(m), col: hit.col },
              { row: lastVisibleRow(m), col: hit.col },
            ),
          })
          return
        case 'row-border':
          beginDrag({
            kind: 'resize-row',
            row: hit.row,
            startY: e.clientY,
            startHeight: rowHeight(m, hit.row),
            height: rowHeight(m, hit.row),
          })
          return
        case 'row-header':
          L.onSelect({
            anchor: { row: hit.row, col: 0 },
            range: mkRange(
              { row: hit.row, col: 0 },
              { row: hit.row, col: Math.max(0, m.colCount - 1) },
            ),
          })
          return
        case 'cell': {
          const addr = { row: hit.row, col: hit.col }
          pointerRef.current = { x: e.clientX, y: e.clientY }
          if (e.shiftKey) {
            const sel = selectionFrom(L.selection.anchor, addr)
            L.onSelect({ anchor: sel.anchor, range: L.merges.expand(sel.range) })
            beginDrag({ kind: 'select', anchor: L.selection.anchor, last: addr })
          } else {
            // Clicking anywhere inside a merged block selects the block, so
            // the cursor never lands on a cell the user cannot see.
            const anchor = L.merges.anchor(addr.row, addr.col)
            const sel = selectionAt(anchor)
            L.onSelect({ anchor, range: L.merges.expand(sel.range) })
            beginDrag({ kind: 'select', anchor, last: addr })
          }
          return
        }
      }
    },
    [beginDrag, invalidate, pointAt, pointBand, pointOf, pointingInput],
  )

  const handleMouseMove = useCallback(
    (e: ReactMouseEvent<HTMLDivElement>) => {
      const el = scrollRef.current
      if (!el || dragRef.current) return
      const L = latestRef.current
      const p = pointOf(e.clientX, e.clientY)
      const hit = hitTest(p.x, p.y, p.scrollTop, p.scrollLeft, L.metrics)
      const onHandle =
        p.x >= L.metrics.headerWidth &&
        p.y >= L.metrics.headerHeight &&
        pointInRect(
          p.x,
          p.y,
          fillHandleRect(L.metrics, L.selection.range, p.scrollTop, p.scrollLeft),
          1,
        )
      el.style.cursor =
        hit.kind === 'col-border'
          ? 'col-resize'
          : hit.kind === 'row-border'
            ? 'row-resize'
            : onHandle
              ? 'crosshair'
              : 'cell'
    },
    [pointOf],
  )

  /**
   * Width that fits the widest value in a column.
   *
   * Only the first `AUTOFIT_SCAN_ROWS` rows are measured. Excel scans the
   * whole column; we cap it because measuring a million strings blocks the
   * main thread, and a header plus the first thousand rows decides the width
   * in practice. A value further down that no longer fits still renders as
   * `#####` rather than being silently truncated, so the cap is visible.
   */
  const autofitColumn = useCallback((col: number) => {
    const canvas = canvasRef.current
    const ctx = canvas?.getContext('2d')
    const L = latestRef.current
    if (!ctx || !L.sheetExists) return
    const rows = Math.min(L.metrics.rowCount, Math.max(L.usedRows, 1), AUTOFIT_SCAN_ROWS)
    const vp = L.engine.viewport(L.sheet, 0, col, rows, 1)
    let widest = 0
    for (let i = 0; i < vp.values.length; i++) {
      const text = vp.values[i]
      if (!text) continue
      ctx.font = fontFor(vp.palette[vp.styles[i]] ?? EMPTY_CELL_FORMAT)
      widest = Math.max(widest, ctx.measureText(text).width)
    }
    ctx.font = CELL_FONT
    const width = clampColWidth(widest + CELL_PAD * 2 + 2)
    L.onResize('col', col, 1, width)
  }, [])

  const handleDoubleClick = useCallback(
    (e: ReactMouseEvent<HTMLDivElement>) => {
      const L = latestRef.current
      const m = L.metrics
      const p = pointOf(e.clientX, e.clientY)

      // Double-clicking the fill handle fills down to the length of the
      // neighbouring run, the way it does in Excel — the gesture for "apply
      // this formula to the whole table" without dragging past the fold.
      if (p.x >= m.headerWidth && p.y >= m.headerHeight) {
        const fh = fillHandleRect(m, L.selection.range, p.scrollTop, p.scrollLeft)
        if (pointInRect(p.x, p.y, fh, 1)) {
          const target = fillDownTarget(L)
          if (target) L.onFill(L.selection.range, target)
          return
        }
      }

      const hit = hitTest(p.x, p.y, p.scrollTop, p.scrollLeft, m)
      if (hit.kind === 'col-border') {
        autofitColumn(hit.col)
        return
      }
      if (hit.kind !== 'cell') return
      // The second press of a double-click while pointing would otherwise
      // abandon the formula and start editing whatever was being pointed at.
      if (L.editing && pointingSlot(L.editing.value, L.editing.value.length)) return
      L.onStartEdit({ row: hit.row, col: hit.col })
    },
    [autofitColumn, pointOf],
  )

  const handleContextMenu = useCallback(
    (e: ReactMouseEvent<HTMLDivElement>) => {
      e.preventDefault()
      const L = latestRef.current
      const p = pointOf(e.clientX, e.clientY)
      const hit = hitTest(p.x, p.y, p.scrollTop, p.scrollLeft, L.metrics)
      if (hit.kind !== 'cell') return
      const addr = { row: hit.row, col: hit.col }
      if (!rangeContains(L.selection.range, addr)) L.onSelect(selectionAt(addr))
      L.onContextMenu(addr, e.clientX, e.clientY)
    },
    [pointOf],
  )

  /* --------------------------------------------------------------- keys */

  const handleKeyDown = useCallback((e: ReactKeyboardEvent<HTMLDivElement>) => {
    const L = latestRef.current
    // While editing, the overlay input owns the keyboard — including arrows,
    // which must move the caret rather than the selection.
    if (L.editing) return

    const m = L.metrics
    const sel = L.selection
    const active = sel.anchor
    const mod = e.ctrlKey || e.metaKey
    const extend = e.shiftKey
    const from = extend ? selectionFocus(sel) : active
    const go = (to: Addr) => L.onSelect(extend ? selectionFrom(sel.anchor, to) : selectionAt(to))

    let dir: MoveDirection | null = null
    switch (e.key) {
      case 'ArrowUp':
        dir = 'up'
        break
      case 'ArrowDown':
        dir = 'down'
        break
      case 'ArrowLeft':
        dir = 'left'
        break
      case 'ArrowRight':
        dir = 'right'
        break
      default:
        break
    }
    if (dir) {
      e.preventDefault()
      go(
        mod
          ? usedEdge(m, from, dir, L.usedRows, L.usedCols)
          : moveWithMerges(m, L.merges, from, dir),
      )
      return
    }

    switch (e.key) {
      case 'Tab':
        e.preventDefault()
        L.onSelect(selectionAt(moveWithMerges(m, L.merges, active, e.shiftKey ? 'left' : 'right')))
        return
      case 'Enter':
        e.preventDefault()
        L.onSelect(selectionAt(moveWithMerges(m, L.merges, active, e.shiftKey ? 'up' : 'down')))
        return
      case 'F2':
        e.preventDefault()
        L.onStartEdit(active)
        return
      case 'Home':
        e.preventDefault()
        go(mod ? { row: firstVisibleRow(m), col: 0 } : { row: from.row, col: 0 })
        return
      case 'End':
        e.preventDefault()
        go({
          row: mod ? usedEdge(m, from, 'down', L.usedRows, L.usedCols).row : from.row,
          col: Math.max(0, L.usedCols - 1),
        })
        return
      case 'PageUp':
      case 'PageDown': {
        e.preventDefault()
        const h = scrollRef.current?.clientHeight ?? 0
        go({
          row: pageJump(m, from.row, e.key === 'PageUp' ? 'up' : 'down', h),
          col: from.col,
        })
        return
      }
      // Clearing cells belongs to the parent, so let these bubble untouched.
      case 'Delete':
      case 'Backspace':
      case 'Escape':
        return
      default:
        break
    }

    if (mod) {
      if (e.key === 'a' || e.key === 'A') {
        e.preventDefault()
        L.onSelect(
          selectionFrom(
            { row: 0, col: 0 },
            { row: Math.max(0, L.usedRows - 1), col: Math.max(0, L.usedCols - 1) },
          ),
        )
      }
      return
    }
    if (e.altKey) return

    // Any printable character starts an edit with that character typed in.
    if (e.key.length === 1) {
      e.preventDefault()
      L.onStartEdit(active, e.key)
    }
  }, [])

  const handleEditorKeyDown = useCallback(
    (e: ReactKeyboardEvent<HTMLInputElement>) => {
      // The menu answers Enter, Tab, Escape and the arrows first when it is
      // open, because while a list of functions is on screen those keys are
      // obviously about the list. It reports what it took rather than
      // swallowing the event, so everything it declines still lands here.
      if (completion.handleKeyDown(e)) return

      const arrow = EDITOR_ARROWS[e.key]
      // A run of arrows picks one reference. Anything else — a character, a
      // bracket, Enter — ends the run, because the slot it was rewriting is no
      // longer the last thing in the formula. Holding a modifier is not
      // "anything else": Shift arrives as its own keydown, and treating that
      // as input ended the run on the exact gesture it exists for.
      if (!arrow && !BARE_MODIFIERS.has(e.key)) arrowPointRef.current = null

      switch (e.key) {
        case 'Enter':
          e.preventDefault()
          onCommitEdit(e.shiftKey ? 'up' : 'down')
          return
        case 'Tab':
          e.preventDefault()
          onCommitEdit(e.shiftKey ? 'left' : 'right')
          return
        case 'Escape':
          e.preventDefault()
          onCancelEdit()
          return
        default:
          break
      }

      const dir = arrow
      if (!dir || !editing) return

      // Pointing by keyboard. `=SUM(` then Up is Excel's other way of naming a
      // cell, and it is the one people who never touch the mouse use — so the
      // arrows belong to the grid whenever the formula is expecting an operand,
      // and to the caret whenever it is not.
      //
      // Only in enter mode, and only until an arrow has been used for the
      // caret. An edit opened with F2 is somebody amending text, and an edit
      // that has already moved its caret once has said the same thing.
      const canPoint = editing.value.startsWith('=') && editing.replacing && !caretArrowRef.current
      if (canPoint && pointByArrow(dir, e.shiftKey, e.ctrlKey || e.metaKey)) {
        e.preventDefault()
        return
      }
      // The arrow is the caret's from here on, for the rest of this edit.
      if (editing.value.startsWith('=')) caretArrowRef.current = true
      // Excel has two editing modes, and the arrow keys are the only place the
      // difference shows. Typing over a cell is *enter mode*: an arrow commits
      // and moves, which is what lets a row be filled by typing and arrowing
      // without ever reaching for Enter. F2 or a double-click is *edit mode*:
      // the arrows belong to the caret, because the point of opening an
      // existing value is to amend it.
      //
      // Half-typed formulas stay on the caret whichever mode they started in.
      // Excel would enter pointing mode here and let the arrows pick the next
      // operand; until that exists, moving the caret is the harmless reading
      // of the keystroke, where committing would turn `=A1+` into an error the
      // user never asked for.
      if (!editing.replacing || editing.value.startsWith('=')) return
      e.preventDefault()
      onCommitEdit(dir)
    },
    [completion, editing, onCancelEdit, onCommitEdit, pointByArrow],
  )

  const handleEditorBlur = useCallback(
    (e: ReactFocusEvent<HTMLInputElement>) => {
      // Losing focus commits, so clicking a toolbar button or another part of
      // the app cannot leave a value stranded in an editor nobody can see.
      //
      // The formula bar is the exception: moving into it continues the same
      // edit rather than abandoning it, and the two inputs share `editing`, so
      // committing here would tear down the state the formula bar is about to
      // keep typing into.
      const to = e.relatedTarget as HTMLElement | null
      if (to?.closest('.formula-bar')) return
      completion.close()
      onCommitEdit('none')
    },
    [completion, onCommitEdit],
  )

  /* ------------------------------------------------------------- effects */

  // Follow the selection when the keyboard walks it off screen.
  useEffect(() => {
    const el = scrollRef.current
    if (!el) return
    const focus = selectionFocus(selection)
    const next = scrollToInclude(
      focus.row,
      focus.col,
      el.scrollTop,
      el.scrollLeft,
      el.clientWidth,
      el.clientHeight,
      metrics,
    )
    if (next.scrollTop !== el.scrollTop) el.scrollTop = next.scrollTop
    if (next.scrollLeft !== el.scrollLeft) el.scrollLeft = next.scrollLeft
  }, [selection, metrics])

  // Pointing replaces the input's value from the outside, and a controlled
  // input answers that by putting the caret at the end. Restoring it here —
  // before the browser paints — is what makes `=SUM(A1:A3` still accept the
  // `)` the user is about to type.
  useLayoutEffect(() => {
    const pending = pendingCaretRef.current
    if (!pending) return
    pendingCaretRef.current = null
    pending.el.focus()
    pending.el.setSelectionRange(pending.caret, pending.caret)
  })

  const editKey = editing ? `${editing.addr.row}:${editing.addr.col}` : null
  useEffect(() => {
    if (editKey === null) {
      pointingRef.current = null
      arrowPointRef.current = null
      caretArrowRef.current = false
      containerRef.current?.focus()
      return
    }
    const input = inputRef.current
    if (!input) return
    input.focus()
    // Caret at the end in both cases: when typing opened the editor the old
    // contents were already replaced by the typed character upstream, so
    // re-selecting here would eat the user's next keystroke.
    const end = input.value.length
    input.setSelectionRange(end, end)
  }, [editKey])

  /* -------------------------------------------------------------- render */

  const scrollTop = scrollRef.current?.scrollTop ?? 0
  const scrollLeft = scrollRef.current?.scrollLeft ?? 0
  const editBox = editing
    ? cellRect(metrics, editing.addr.row, editing.addr.col, scrollTop, scrollLeft)
    : null

  return (
    <div
      ref={containerRef}
      tabIndex={0}
      onKeyDown={handleKeyDown}
      style={{
        position: 'relative',
        flex: 1,
        alignSelf: 'stretch',
        width: '100%',
        height: '100%',
        minHeight: 0,
        outline: 'none',
        background: COLOR_BG,
      }}
    >
      <div
        ref={scrollRef}
        data-testid="grid-scroll"
        onScroll={invalidate}
        onMouseDown={handleMouseDown}
        onMouseMove={handleMouseMove}
        onDoubleClick={handleDoubleClick}
        onContextMenu={handleContextMenu}
        style={{ position: 'absolute', inset: 0, overflow: 'auto' }}
      >
        <div
          style={{
            // The frozen band is always on screen, so it is not part of what
            // there is to scroll through.
            width: metrics.headerWidth + frozenWidth(metrics) + scrollableWidth(metrics),
            height: metrics.headerHeight + frozenHeight(metrics) + scrollableHeight(metrics),
          }}
        />
      </div>
      <canvas
        ref={canvasRef}
        style={{ position: 'absolute', top: 0, left: 0, pointerEvents: 'none' }}
      />
      {editing && editBox && (
        <>
          <input
            ref={inputRef}
            data-testid="cell-editor"
            value={editing.value}
            spellCheck={false}
            autoComplete="off"
            onChange={(e) => {
              // Typing moves on from whatever was pointed at, so the outline
              // stops describing the formula and has to go — and the next
              // arrow starts a new reference rather than moving the old one.
              pointingRef.current = null
              arrowPointRef.current = null
              onEditValueChange(e.target.value)
              completion.refresh(e.target.value, e.target.selectionStart ?? e.target.value.length)
              invalidate()
            }}
            // `onSelect` is how a caret move is heard — clicking into the middle
            // of a name should offer that name, not whatever was last typed.
            onSelect={(e) => {
              const el = e.currentTarget
              completion.refresh(el.value, el.selectionStart ?? el.value.length)
            }}
            onKeyDown={handleEditorKeyDown}
            onBlur={handleEditorBlur}
            style={{
              position: 'absolute',
              left: editBox.x,
              top: editBox.y,
              width: editBox.w,
              height: Math.max(editBox.h, metrics.defaultRowHeight),
              font: CELL_FONT,
              padding: `0 ${CELL_PAD - 1}px`,
              border: `2px solid ${COLOR_ACCENT}`,
              outline: 'none',
              background: COLOR_BG,
              color: COLOR_TEXT,
            }}
          />
          <FunctionMenu
            api={completion}
            style={{
              left: editBox.x,
              top: editBox.y + Math.max(editBox.h, metrics.defaultRowHeight),
              minWidth: Math.max(editBox.w, 260),
            }}
          />
        </>
      )}
    </div>
  )
}
