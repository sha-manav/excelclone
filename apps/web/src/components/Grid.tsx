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
import type { EngineHandle, Viewport } from '../engine/bridge'
import { colLetters, range as mkRange, rangeContains } from '../engine/actions'
import type { Addr, Range } from '../engine/actions'
import type { EditState, MoveDirection, Selection } from '../state/useWorkbook'
import {
  OVERSCAN,
  cellAlign,
  cellRect,
  clampColWidth,
  colWidth,
  columnAtX,
  columnLeft,
  createMetrics,
  fillHandleRect,
  fillTarget,
  firstVisibleRow,
  hitTest,
  lastVisibleRow,
  moveAddr,
  overflowHashes,
  pageJump,
  pointInRect,
  rangeRect,
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
import type { GridMetrics } from './grid-geometry'

export interface GridProps {
  engine: EngineHandle
  sheet: string
  /** Bump to re-read cells from the engine and repaint. */
  version: number
  selection: Selection
  editing: EditState | null
  /** Rows hidden by a filter; skipped entirely in layout. */
  hiddenRows: number[]
  onSelect(sel: Selection): void
  /** `initial` set means typing replaced the cell rather than opening it. */
  onStartEdit(addr: Addr, initial?: string): void
  onCommitEdit(move: MoveDirection): void
  onCancelEdit(): void
  onEditValueChange(value: string): void
  onFill(source: Range, target: Range): void
  onContextMenu(addr: Addr, clientX: number, clientY: number): void
  onAutofitColumn(col: number): void
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

type Drag =
  | { kind: 'select'; anchor: Addr; last: Addr }
  | { kind: 'fill'; source: Range; target: Range }
  | { kind: 'resize'; col: number; startX: number; startWidth: number }

interface Latest {
  engine: EngineHandle
  sheet: string
  version: number
  metrics: GridMetrics
  selection: Selection
  editing: EditState | null
  usedRows: number
  usedCols: number
  onSelect(sel: Selection): void
  onFill(source: Range, target: Range): void
  onStartEdit(addr: Addr, initial?: string): void
  onContextMenu(addr: Addr, clientX: number, clientY: number): void
  onAutofitColumn(col: number): void
}

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
    onSelect,
    onStartEdit,
    onCommitEdit,
    onCancelEdit,
    onEditValueChange,
    onFill,
    onContextMenu,
    onAutofitColumn,
  } = props

  const containerRef = useRef<HTMLDivElement>(null)
  const scrollRef = useRef<HTMLDivElement>(null)
  const canvasRef = useRef<HTMLCanvasElement>(null)
  const inputRef = useRef<HTMLInputElement>(null)

  const [colWidths, setColWidths] = useState<ReadonlyMap<number, number>>(() => new Map())
  const [rowHeights] = useState<ReadonlyMap<number, number>>(() => new Map())

  const sheetInfo = useMemo(() => {
    // `version` is never read here; it is the only signal that the used range
    // may have changed, which is what makes it a genuine dependency.
    void version
    return engine.sheets().find((s) => s.name === sheet) ?? null
  }, [engine, sheet, version])
  const usedRows = sheetInfo?.used_rows ?? 0
  const usedCols = sheetInfo?.used_cols ?? 0

  const hiddenSet = useMemo(() => new Set(hiddenRows), [hiddenRows])

  const metrics = useMemo(() => {
    const extent = virtualExtent(usedRows, usedCols)
    return createMetrics({
      colWidths,
      rowHeights,
      hiddenRows: hiddenSet,
      rowCount: extent.rows,
      colCount: extent.cols,
    })
  }, [colWidths, rowHeights, hiddenSet, usedRows, usedCols])

  // Everything the imperative layer (paint, window drag listeners, keyboard)
  // needs, refreshed every render so those handlers can stay identity-stable
  // and never close over stale props.
  const latest: Latest = {
    engine,
    sheet,
    version,
    metrics,
    selection,
    editing,
    usedRows,
    usedCols,
    onSelect,
    onFill,
    onStartEdit,
    onContextMenu,
    onAutofitColumn,
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
        vp: rows > 0 && cols > 0 ? L.engine.viewport(L.sheet, r0, c0, rows, cols) : null,
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

    if (multi) {
      ctx.fillStyle = COLOR_WASH
      ctx.fillRect(selRect.x, selRect.y, selRect.w, selRect.h)
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

    if (vp) {
      ctx.font = CELL_FONT
      ctx.textBaseline = 'middle'
      ctx.textAlign = 'left'
      let align: 'left' | 'right' = 'left'
      const hashWidth = ctx.measureText('#').width

      for (let ri = 0; ri < nRows; ri++) {
        const h = ys[ri + 1] - ys[ri]
        if (h <= 0) continue
        if (ys[ri] > cssH || ys[ri + 1] < hh) continue
        const vrow = vis.firstRow + ri - vp.row0
        if (vrow < 0 || vrow >= vp.rows) continue
        const midY = ys[ri] + h / 2

        for (let ci = 0; ci < nCols; ci++) {
          const vcol = vis.firstCol + ci - vp.col0
          if (vcol < 0 || vcol >= vp.cols) continue
          const idx = vrow * vp.cols + vcol
          const text = vp.values[idx]
          if (!text) continue
          const w = xs[ci + 1] - xs[ci]
          const avail = w - CELL_PAD * 2
          if (avail <= 0) continue

          const kind = vp.kinds[idx]
          const wanted = cellAlign(kind)
          if (wanted !== align) {
            align = wanted
            ctx.textAlign = wanted
          }
          ctx.fillStyle = kind === KIND_ERROR ? COLOR_ERROR : COLOR_TEXT

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
            ctx.rect(xs[ci], ys[ri], w, h)
            ctx.clip()
          }
          ctx.fillText(out, align === 'left' ? xs[ci] + CELL_PAD : xs[ci + 1] - CELL_PAD, midY)
          if (clip) ctx.restore()

          if (vp.formulas[idx]) {
            ctx.fillStyle = COLOR_FORMULA_MARK
            ctx.fillRect(xs[ci] + 1, ys[ri] + 1, 3, 3)
          }
        }
      }
    }

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

  const handleDragMove = useCallback(
    (e: MouseEvent) => {
      const d = dragRef.current
      const el = scrollRef.current
      if (!d || !el) return
      const L = latestRef.current
      const m = L.metrics

      if (d.kind === 'resize') {
        const width = clampColWidth(d.startWidth + (e.clientX - d.startX))
        setColWidths((prev) => {
          if (prev.get(d.col) === width) return prev
          const next = new Map(prev)
          next.set(d.col, width)
          return next
        })
        return
      }

      const p = pointOf(e.clientX, e.clientY)
      const row = rowAtY(m, Math.max(0, p.y - m.headerHeight) + p.scrollTop)
      const col = columnAtX(m, Math.max(0, p.x - m.headerWidth) + p.scrollLeft)

      if (d.kind === 'select') {
        if (row === d.last.row && col === d.last.col) return
        d.last = { row, col }
        L.onSelect(selectionFrom(d.anchor, { row, col }))
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

  const handleDragEnd = useCallback(() => {
    const d = dragRef.current
    dragRef.current = null
    window.removeEventListener('mousemove', handleDragMove)
    window.removeEventListener('mouseup', handleDragEnd)
    if (d && d.kind === 'fill') {
      fillPreviewRef.current = null
      if (!sameRange(d.target, d.source)) latestRef.current.onFill(d.source, d.target)
      invalidate()
    }
  }, [handleDragMove, invalidate])

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
    },
    [handleDragEnd, handleDragMove],
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
          if (e.shiftKey) {
            L.onSelect(selectionFrom(L.selection.anchor, addr))
            beginDrag({ kind: 'select', anchor: L.selection.anchor, last: addr })
          } else {
            L.onSelect(selectionAt(addr))
            beginDrag({ kind: 'select', anchor: addr, last: addr })
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
        hit.kind === 'col-border' ? 'col-resize' : onHandle ? 'crosshair' : 'cell'
    },
    [pointOf],
  )

  const handleDoubleClick = useCallback(
    (e: ReactMouseEvent<HTMLDivElement>) => {
      const L = latestRef.current
      const p = pointOf(e.clientX, e.clientY)
      const hit = hitTest(p.x, p.y, p.scrollTop, p.scrollLeft, L.metrics)
      if (hit.kind === 'col-border') L.onAutofitColumn(hit.col)
      else if (hit.kind === 'cell') L.onStartEdit({ row: hit.row, col: hit.col })
    },
    [pointOf],
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
      go(mod ? usedEdge(m, from, dir, L.usedRows, L.usedCols) : moveAddr(m, from, dir))
      return
    }

    switch (e.key) {
      case 'Tab':
        e.preventDefault()
        L.onSelect(selectionAt(moveAddr(m, active, e.shiftKey ? 'left' : 'right')))
        return
      case 'Enter':
        e.preventDefault()
        L.onSelect(selectionAt(moveAddr(m, active, e.shiftKey ? 'up' : 'down')))
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
