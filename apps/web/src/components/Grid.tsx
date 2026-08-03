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

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type {
  JSX,
  KeyboardEvent as ReactKeyboardEvent,
  MouseEvent as ReactMouseEvent,
} from 'react'
import { KIND_ERROR, KIND_NUMBER } from '../engine/bridge'
import type { CellFormat, EngineHandle, Viewport } from '../engine/bridge'
import { colLetters, range as mkRange, rangeContains } from '../engine/actions'
import type { Addr, Axis, Range } from '../engine/actions'
import type { EditState, MoveDirection, Selection } from '../state/useWorkbook'
import {
  MergeMap,
  OVERSCAN,
  autoscrollDelta,
  cellRect,
  clampColWidth,
  clampRowHeight,
  colWidth,
  columnAtX,
  columnLeft,
  createMetrics,
  fillHandleRect,
  fillTarget,
  firstVisibleRow,
  hitTest,
  lastVisibleRow,
  moveWithMerges,
  overflowHashes,
  pageJump,
  pointInRect,
  rangeRect,
  resolvedAlign,
  rowAtY,
  rowHeight,
  rowTop,
  scrollToInclude,
  selectionAt,
  selectionFocus,
  selectionFrom,
  totalHeight,
  totalWidth,
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

type Drag =
  | { kind: 'select'; anchor: Addr; last: Addr }
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
  onContextMenu(addr: Addr, clientX: number, clientY: number): void
  onResize(axis: Axis, at: number, count: number, size: number | null): void
}

const EMPTY_HIGHLIGHTS: readonly Range[] = []

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
    })
  }, [colWidths, rowHeights, hiddenSet, usedRows, usedCols, paintedRows, paintedCols])

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
    onContextMenu,
    onResize,
  }
  const latestRef = useRef<Latest>(latest)
  latestRef.current = latest

  const cacheRef = useRef<{ key: string; vp: Viewport | null }>({ key: '', vp: null })
  const dragRef = useRef<Drag | null>(null)
  const fillPreviewRef = useRef<Range | null>(null)
  const rafRef = useRef(0)

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
    const nRows = vis.lastRow - vis.firstRow + 1
    const nCols = vis.lastCol - vis.firstCol + 1
    if (nRows <= 0 || nCols <= 0) return

    // Per-frame edge tables: one allocation each, never one per cell.
    const xs = new Array<number>(nCols + 1)
    xs[0] = hw + columnLeft(m, vis.firstCol) - scrollLeft
    for (let i = 0; i < nCols; i++) xs[i + 1] = xs[i] + colWidth(m, vis.firstCol + i)
    const ys = new Array<number>(nRows + 1)
    ys[0] = hh + rowTop(m, vis.firstRow) - scrollTop
    for (let i = 0; i < nRows; i++) ys[i + 1] = ys[i] + rowHeight(m, vis.firstRow + i)

    // One engine call per repaint, cached so plain scrolling inside the
    // overscan band does not re-cross the wasm boundary.
    const r0 = Math.max(0, vis.firstRow - OVERSCAN)
    const c0 = Math.max(0, vis.firstCol - OVERSCAN)
    const rows = Math.min(m.rowCount, vis.lastRow + OVERSCAN + 1) - r0
    const cols = Math.min(m.colCount, vis.lastCol + OVERSCAN + 1) - c0
    const key = `${L.sheet}|${L.version}|${r0}|${c0}|${rows}|${cols}`
    if (cacheRef.current.key !== key) {
      cacheRef.current = {
        key,
        vp:
          L.sheetExists && rows > 0 && cols > 0
            ? L.engine.viewport(L.sheet, r0, c0, rows, cols)
            : null,
      }
    }
    const vp = cacheRef.current.vp

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
      if (!vp) return EMPTY_CELL_FORMAT
      const vrow = vis.firstRow + ri - vp.row0
      const vcol = vis.firstCol + ci - vp.col0
      if (vrow < 0 || vrow >= vp.rows || vcol < 0 || vcol >= vp.cols) {
        return EMPTY_CELL_FORMAT
      }
      return vp.palette[vp.styles[vrow * vp.cols + vcol]] ?? EMPTY_CELL_FORMAT
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
            r.end.row >= vis.firstRow &&
            r.start.row <= vis.lastRow &&
            r.end.col >= vis.firstCol &&
            r.start.col <= vis.lastCol,
        )
    for (const mr of mergesToPaint) {
      const rect = rangeRect(m, mr, scrollTop, scrollLeft)
      if (rect.w <= 0 || rect.h <= 0) continue
      const anchorFill = L.merges.isEmpty
        ? undefined
        : formatAt(mr.start.row - vis.firstRow, mr.start.col - vis.firstCol).fill_color
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

    if (vp) {
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
        const vrow = vis.firstRow + ri - vp.row0
        if (vrow < 0 || vrow >= vp.rows) continue
        const midY = ys[ri] + h / 2
        const row = vis.firstRow + ri

        for (let ci = 0; ci < nCols; ci++) {
          const vcol = vis.firstCol + ci - vp.col0
          if (vcol < 0 || vcol >= vp.cols) continue
          const idx = vrow * vp.cols + vcol
          const text = vp.values[idx]
          if (!text) continue
          const col = vis.firstCol + ci

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
            if (kind === KIND_NUMBER) out = overflowHashes(avail, hashWidth)
            else clip = true
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
      const col = vis.firstCol + ci
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
      const target = fillTarget(d.source, row, col)
      if (sameRange(target, d.target)) return
      d.target = target
      fillPreviewRef.current = target
      invalidate()
    },
    [invalidate, pointOf],
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
      containerRef.current?.focus()
      // Presses on the native scrollbars land inside the element but outside
      // its client box; they belong to the browser, not to the selection.
      if (p.x >= el.clientWidth || p.y >= el.clientHeight) return

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

      const hit = hitTest(p.x, p.y, p.scrollTop, p.scrollLeft, m)
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
    [beginDrag, invalidate, pointOf],
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
      if (hit.kind === 'col-border') autofitColumn(hit.col)
      else if (hit.kind === 'cell') L.onStartEdit({ row: hit.row, col: hit.col })
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
      switch (e.key) {
        case 'Enter':
          e.preventDefault()
          onCommitEdit(e.shiftKey ? 'up' : 'down')
          break
        case 'Tab':
          e.preventDefault()
          onCommitEdit(e.shiftKey ? 'left' : 'right')
          break
        case 'Escape':
          e.preventDefault()
          onCancelEdit()
          break
        default:
          break
      }
    },
    [onCancelEdit, onCommitEdit],
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

  const editKey = editing ? `${editing.addr.row}:${editing.addr.col}` : null
  useEffect(() => {
    if (editKey === null) {
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
        onScroll={invalidate}
        onMouseDown={handleMouseDown}
        onMouseMove={handleMouseMove}
        onDoubleClick={handleDoubleClick}
        onContextMenu={handleContextMenu}
        style={{ position: 'absolute', inset: 0, overflow: 'auto' }}
      >
        <div
          style={{
            width: metrics.headerWidth + totalWidth(metrics),
            height: metrics.headerHeight + totalHeight(metrics),
          }}
        />
      </div>
      <canvas
        ref={canvasRef}
        style={{ position: 'absolute', top: 0, left: 0, pointerEvents: 'none' }}
      />
      {editing && editBox && (
        <input
          ref={inputRef}
          value={editing.value}
          spellCheck={false}
          autoComplete="off"
          onChange={(e) => onEditValueChange(e.target.value)}
          onKeyDown={handleEditorKeyDown}
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
      )}
    </div>
  )
}
