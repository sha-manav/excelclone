/**
 * Pure layout math for the canvas grid.
 *
 * Nothing here touches the DOM, the canvas, or React — the grid component owns
 * all of that and calls into this file for every coordinate decision. Keeping
 * the arithmetic separable is what makes the grid testable at all: pixel
 * positions, hit targets and fill-drag semantics are exactly the parts that
 * are easy to get subtly wrong and impossible to eyeball.
 *
 * Two coordinate spaces appear throughout:
 *   - *content* space: (0,0) is the top-left of cell A1, headers excluded.
 *   - *viewport* space: (0,0) is the top-left of the visible box, headers
 *     included. viewport = content - scroll + headerSize.
 */

import { range as mkRange, singleRange } from '../engine/actions'
import type { Addr, Range } from '../engine/actions'
import type { MoveDirection, Selection } from '../state/useWorkbook'

export const DEFAULT_COL_WIDTH = 100
export const DEFAULT_ROW_HEIGHT = 24
export const HEADER_WIDTH = 46
export const HEADER_HEIGHT = 24
export const MIN_COL_WIDTH = 24
export const MIN_ROW_HEIGHT = 12
/** Half-width of the draggable strip straddling a column-header border. */
export const RESIZE_HANDLE_PX = 4
export const FILL_HANDLE_PX = 7
/** Extra rows/cols painted outside the visible box so scrolling never flashes. */
export const OVERSCAN = 2
export const EXTENT_ROW_PAD = 200
export const EXTENT_COL_PAD = 50
export const MAX_ROW = 1_048_575
export const MAX_COL = 16_383

export interface GridMetrics {
  readonly colWidths: ReadonlyMap<number, number>
  readonly rowHeights: ReadonlyMap<number, number>
  readonly hiddenRows: ReadonlySet<number>
  readonly defaultColWidth: number
  readonly defaultRowHeight: number
  readonly headerWidth: number
  readonly headerHeight: number
  /** Exclusive bounds of the scrollable virtual extent. */
  readonly rowCount: number
  readonly colCount: number
}

export interface MetricsInit {
  colWidths?: ReadonlyMap<number, number>
  rowHeights?: ReadonlyMap<number, number>
  hiddenRows?: Iterable<number>
  defaultColWidth?: number
  defaultRowHeight?: number
  headerWidth?: number
  headerHeight?: number
  rowCount?: number
  colCount?: number
}

const EMPTY_SIZES: ReadonlyMap<number, number> = new Map()

export function createMetrics(init: MetricsInit = {}): GridMetrics {
  return {
    colWidths: init.colWidths ?? EMPTY_SIZES,
    rowHeights: init.rowHeights ?? EMPTY_SIZES,
    hiddenRows: new Set(init.hiddenRows ?? []),
    defaultColWidth: init.defaultColWidth ?? DEFAULT_COL_WIDTH,
    defaultRowHeight: init.defaultRowHeight ?? DEFAULT_ROW_HEIGHT,
    headerWidth: init.headerWidth ?? HEADER_WIDTH,
    headerHeight: init.headerHeight ?? HEADER_HEIGHT,
    rowCount: init.rowCount ?? EXTENT_ROW_PAD,
    colCount: init.colCount ?? EXTENT_COL_PAD,
  }
}

/* ------------------------------------------------------------------ sizes */

export function isRowHidden(m: GridMetrics, row: number): boolean {
  return m.hiddenRows.has(row)
}

export function colWidth(m: GridMetrics, col: number): number {
  return m.colWidths.get(col) ?? m.defaultColWidth
}

/** Hidden rows have zero height, so they simply vanish from every offset sum. */
export function rowHeight(m: GridMetrics, row: number): number {
  if (m.hiddenRows.has(row)) return 0
  return m.rowHeights.get(row) ?? m.defaultRowHeight
}

/**
 * Cumulative left edges for `count` columns starting at `col0`, relative to
 * `col0`'s own left edge. Length is `count + 1`; the last entry is the total.
 */
export function columnOffsets(
  m: GridMetrics,
  col0: number,
  count: number,
): number[] {
  const out = new Array<number>(Math.max(0, count) + 1)
  out[0] = 0
  for (let i = 0; i < count; i++) out[i + 1] = out[i] + colWidth(m, col0 + i)
  return out
}

/** As {@link columnOffsets}; hidden rows contribute a zero-width step. */
export function rowOffsets(
  m: GridMetrics,
  row0: number,
  count: number,
): number[] {
  const out = new Array<number>(Math.max(0, count) + 1)
  out[0] = 0
  for (let i = 0; i < count; i++) out[i + 1] = out[i] + rowHeight(m, row0 + i)
  return out
}

/**
 * Content-space x of a column's left edge. Computed from the default size plus
 * the sparse overrides rather than by walking every column, so this stays O(map)
 * instead of O(col) for far-right columns.
 */
export function columnLeft(m: GridMetrics, col: number): number {
  let x = col * m.defaultColWidth
  for (const [c, w] of m.colWidths) if (c < col) x += w - m.defaultColWidth
  return x
}

export function rowTop(m: GridMetrics, row: number): number {
  let y = row * m.defaultRowHeight
  for (const [r, h] of m.rowHeights) {
    if (r < row && !m.hiddenRows.has(r)) y += h - m.defaultRowHeight
  }
  // A hidden row keeps neither its override nor the default.
  for (const r of m.hiddenRows) if (r < row) y -= m.defaultRowHeight
  return y
}

export function totalWidth(m: GridMetrics): number {
  let w = m.colCount * m.defaultColWidth
  for (const [c, cw] of m.colWidths) if (c < m.colCount) w += cw - m.defaultColWidth
  return w
}

export function totalHeight(m: GridMetrics): number {
  let h = m.rowCount * m.defaultRowHeight
  for (const [r, rh] of m.rowHeights) {
    if (r < m.rowCount && !m.hiddenRows.has(r)) h += rh - m.defaultRowHeight
  }
  for (const r of m.hiddenRows) if (r < m.rowCount) h -= m.defaultRowHeight
  return h
}

/* ------------------------------------------------------------- pick a cell */

export function columnAtX(m: GridMetrics, x: number): number {
  if (m.colCount <= 0) return 0
  let acc = 0
  for (let c = 0; c < m.colCount; c++) {
    acc += colWidth(m, c)
    if (x < acc) return c
  }
  return m.colCount - 1
}

/** Never returns a hidden row: zero-height rows cannot contain a point. */
export function rowAtY(m: GridMetrics, y: number): number {
  let acc = 0
  let last = 0
  for (let r = 0; r < m.rowCount; r++) {
    const h = rowHeight(m, r)
    if (h === 0) continue
    last = r
    acc += h
    if (y < acc) return r
  }
  return last
}

export function firstVisibleRow(m: GridMetrics): number {
  for (let r = 0; r < m.rowCount; r++) if (!m.hiddenRows.has(r)) return r
  return 0
}

export function lastVisibleRow(m: GridMetrics): number {
  for (let r = m.rowCount - 1; r >= 0; r--) if (!m.hiddenRows.has(r)) return r
  return 0
}

/**
 * Step one row in `delta` direction, skipping hidden rows. Stays put when the
 * only rows left in that direction are hidden.
 */
export function nextVisibleRow(m: GridMetrics, row: number, delta: number): number {
  if (delta === 0) return row
  const step = delta > 0 ? 1 : -1
  for (let r = row + step; r >= 0 && r <= MAX_ROW; r += step) {
    if (!m.hiddenRows.has(r)) return r
  }
  return row
}

/* ------------------------------------------------------------ visible span */

export interface VisibleRange {
  firstRow: number
  /** Inclusive. `lastRow < firstRow` means nothing is visible. */
  lastRow: number
  firstCol: number
  /** Inclusive. `lastCol < firstCol` means nothing is visible. */
  lastCol: number
}

/**
 * `viewportWidth`/`viewportHeight` are the whole box including the frozen
 * headers, which is what a DOM client rect gives us.
 */
export function visibleRange(
  scrollTop: number,
  scrollLeft: number,
  viewportWidth: number,
  viewportHeight: number,
  m: GridMetrics,
): VisibleRange {
  const contentW = viewportWidth - m.headerWidth
  const contentH = viewportHeight - m.headerHeight
  const firstRow = rowAtY(m, Math.max(0, scrollTop))
  const firstCol = columnAtX(m, Math.max(0, scrollLeft))
  if (contentW <= 0 || contentH <= 0) {
    return { firstRow, lastRow: firstRow - 1, firstCol, lastCol: firstCol - 1 }
  }
  const lastRow = Math.max(firstRow, rowAtY(m, scrollTop + contentH - 1))
  const lastCol = Math.max(firstCol, columnAtX(m, scrollLeft + contentW - 1))
  return { firstRow, lastRow, firstCol, lastCol }
}

/* ------------------------------------------------------------------- hits */

export type GridHit =
  | { kind: 'corner' }
  | { kind: 'col-header'; col: number }
  /** The border on the *right* edge of `col`; dragging it resizes `col`. */
  | { kind: 'col-border'; col: number }
  | { kind: 'row-header'; row: number }
  /** The border on the *bottom* edge of `row`; dragging it resizes `row`. */
  | { kind: 'row-border'; row: number }
  | { kind: 'cell'; row: number; col: number }

/**
 * `x`/`y` are viewport-space (relative to the grid box, headers included).
 */
export function hitTest(
  x: number,
  y: number,
  scrollTop: number,
  scrollLeft: number,
  m: GridMetrics,
  tolerance: number = RESIZE_HANDLE_PX,
): GridHit {
  const inColHeader = y < m.headerHeight
  const inRowHeader = x < m.headerWidth
  if (inColHeader && inRowHeader) return { kind: 'corner' }

  if (inColHeader) {
    const cx = x - m.headerWidth + scrollLeft
    const col = columnAtX(m, cx)
    const left = columnLeft(m, col)
    // A border belongs to the column on its left, so the strip just inside the
    // left edge of column N resizes column N-1.
    if (col > 0 && cx - left <= tolerance) return { kind: 'col-border', col: col - 1 }
    if (left + colWidth(m, col) - cx <= tolerance) return { kind: 'col-border', col }
    return { kind: 'col-header', col }
  }

  if (inRowHeader) {
    const cy = y - m.headerHeight + scrollTop
    const row = rowAtY(m, cy)
    const top = rowTop(m, row)
    // Mirror of the column rule: the strip just inside the top edge of row N
    // resizes row N-1. A hidden row has no height, so its border is the one
    // above it and dragging there would resize something invisible.
    if (row > 0 && cy - top <= tolerance && !isRowHidden(m, row - 1)) {
      return { kind: 'row-border', row: row - 1 }
    }
    if (!isRowHidden(m, row) && top + rowHeight(m, row) - cy <= tolerance) {
      return { kind: 'row-border', row }
    }
    return { kind: 'row-header', row }
  }

  return {
    kind: 'cell',
    row: rowAtY(m, y - m.headerHeight + scrollTop),
    col: columnAtX(m, x - m.headerWidth + scrollLeft),
  }
}

export interface Rect {
  x: number
  y: number
  w: number
  h: number
}

/** Viewport-space box of a cell, headers included in the offset. */
export function cellRect(
  m: GridMetrics,
  row: number,
  col: number,
  scrollTop: number,
  scrollLeft: number,
): Rect {
  return {
    x: m.headerWidth + columnLeft(m, col) - scrollLeft,
    y: m.headerHeight + rowTop(m, row) - scrollTop,
    w: colWidth(m, col),
    h: rowHeight(m, row),
  }
}

/** Viewport-space box spanning a whole range. */
export function rangeRect(
  m: GridMetrics,
  r: Range,
  scrollTop: number,
  scrollLeft: number,
): Rect {
  const start = cellRect(m, r.start.row, r.start.col, scrollTop, scrollLeft)
  const endLeft = columnLeft(m, r.end.col) + colWidth(m, r.end.col)
  const endTop = rowTop(m, r.end.row) + rowHeight(m, r.end.row)
  return {
    x: start.x,
    y: start.y,
    w: m.headerWidth + endLeft - scrollLeft - start.x,
    h: m.headerHeight + endTop - scrollTop - start.y,
  }
}

/** The little square at the bottom-right of the selection, in viewport space. */
export function fillHandleRect(
  m: GridMetrics,
  r: Range,
  scrollTop: number,
  scrollLeft: number,
): Rect {
  const box = rangeRect(m, r, scrollTop, scrollLeft)
  const s = FILL_HANDLE_PX
  return { x: box.x + box.w - s / 2, y: box.y + box.h - s / 2, w: s, h: s }
}

export function pointInRect(x: number, y: number, r: Rect, slop = 0): boolean {
  return (
    x >= r.x - slop && x <= r.x + r.w + slop && y >= r.y - slop && y <= r.y + r.h + slop
  )
}

/* -------------------------------------------------------------- fill drag */

/**
 * Where a fill-handle drag landing on (row,col) would take the selection.
 *
 * Excel extends along one axis only — whichever the pointer left the source by
 * further — and the result always contains the source, so a drag that ends
 * inside the source is a no-op rather than a shrink.
 */
export function fillTarget(source: Range, row: number, col: number): Range {
  const rowOver =
    row < source.start.row
      ? source.start.row - row
      : row > source.end.row
        ? row - source.end.row
        : 0
  const colOver =
    col < source.start.col
      ? source.start.col - col
      : col > source.end.col
        ? col - source.end.col
        : 0

  if (rowOver === 0 && colOver === 0) return { ...source }

  if (rowOver >= colOver) {
    return {
      start: { row: Math.min(source.start.row, row), col: source.start.col },
      end: { row: Math.max(source.end.row, row), col: source.end.col },
    }
  }
  return {
    start: { row: source.start.row, col: Math.min(source.start.col, col) },
    end: { row: source.end.row, col: Math.max(source.end.col, col) },
  }
}

/* ------------------------------------------------------------- scrolling */

export interface ScrollOffsets {
  scrollTop: number
  scrollLeft: number
}

/**
 * Smallest scroll change that brings (row,col) fully inside the content box.
 * Returns the current offsets untouched when the cell is already visible or the
 * box has no room.
 */
export function scrollToInclude(
  row: number,
  col: number,
  scrollTop: number,
  scrollLeft: number,
  viewportWidth: number,
  viewportHeight: number,
  m: GridMetrics,
): ScrollOffsets {
  const contentW = viewportWidth - m.headerWidth
  const contentH = viewportHeight - m.headerHeight
  let top = scrollTop
  let left = scrollLeft

  if (contentW > 0) {
    const x = columnLeft(m, col)
    const w = colWidth(m, col)
    if (x < left) left = x
    else if (x + w > left + contentW) left = x + w - contentW
  }
  if (contentH > 0) {
    const y = rowTop(m, row)
    const h = rowHeight(m, row)
    if (y < top) top = y
    else if (y + h > top + contentH) top = y + h - contentH
  }

  return { scrollTop: Math.max(0, top), scrollLeft: Math.max(0, left) }
}

/** Row a PageUp/PageDown from `row` lands on; always moves at least one row. */
export function pageJump(
  m: GridMetrics,
  row: number,
  dir: 'up' | 'down',
  viewportHeight: number,
): number {
  const contentH = Math.max(1, viewportHeight - m.headerHeight)
  const step = dir === 'down' ? 1 : -1
  let used = 0
  let landed = row
  for (let r = row + step; r >= 0 && r < m.rowCount; r += step) {
    if (m.hiddenRows.has(r)) continue
    used += rowHeight(m, r)
    landed = r
    if (used >= contentH) break
  }
  return landed
}

/* ------------------------------------------------------------- selection */

export function selectionFrom(anchor: Addr, focus: Addr): Selection {
  return { anchor, range: mkRange(anchor, focus) }
}

export function selectionAt(a: Addr): Selection {
  return { anchor: a, range: singleRange(a) }
}

/** The corner of a selection that the keyboard is currently steering. */
export function selectionFocus(sel: Selection): Addr {
  return {
    row: sel.anchor.row === sel.range.start.row ? sel.range.end.row : sel.range.start.row,
    col: sel.anchor.col === sel.range.start.col ? sel.range.end.col : sel.range.start.col,
  }
}

export function moveAddr(m: GridMetrics, from: Addr, dir: MoveDirection): Addr {
  switch (dir) {
    case 'up':
      return { row: nextVisibleRow(m, from.row, -1), col: from.col }
    case 'down':
      return { row: nextVisibleRow(m, from.row, 1), col: from.col }
    case 'left':
      return { row: from.row, col: Math.max(0, from.col - 1) }
    case 'right':
      return { row: from.row, col: Math.min(MAX_COL, from.col + 1) }
    case 'none':
      return from
  }
}

/**
 * An arrow key, with merges taken into account.
 *
 * Two rules, both Excel's. Leaving a merged block steps from its *far* edge,
 * so pressing Right in a block spanning A1:C1 lands on D1 rather than on B1,
 * which the user cannot see. Arriving in one lands on its anchor, so the
 * selection never sits on a covered cell.
 *
 * Without this, arrowing across a merged header walked invisibly through its
 * covered cells and the selection appeared to stop moving.
 */
export function moveWithMerges(
  m: GridMetrics,
  merges: MergeMap,
  from: Addr,
  dir: MoveDirection,
): Addr {
  const here = merges.at(from.row, from.col)
  const edge = here
    ? {
        up: { row: here.start.row, col: here.start.col },
        down: { row: here.end.row, col: here.start.col },
        left: { row: here.start.row, col: here.start.col },
        right: { row: here.start.row, col: here.end.col },
        none: from,
      }[dir]
    : from
  const landed = moveAddr(m, edge, dir)
  return merges.anchor(landed.row, landed.col)
}

/**
 * Ctrl/Cmd+Arrow: jump to the edge of the used range in that direction, which
 * is where Excel lands when the run of cells continues to the boundary.
 */
export function usedEdge(
  m: GridMetrics,
  from: Addr,
  dir: MoveDirection,
  usedRows: number,
  usedCols: number,
): Addr {
  const lastUsedRow = Math.max(0, usedRows - 1)
  const lastUsedCol = Math.max(0, usedCols - 1)
  switch (dir) {
    case 'up':
      return { row: firstVisibleRow(m), col: from.col }
    case 'down': {
      let r = Math.min(lastUsedRow, m.rowCount - 1)
      while (r > 0 && m.hiddenRows.has(r)) r--
      return { row: Math.max(r, from.row), col: from.col }
    }
    case 'left':
      return { row: from.row, col: 0 }
    case 'right':
      return { row: from.row, col: Math.max(lastUsedCol, from.col) }
    case 'none':
      return from
  }
}

/* ------------------------------------------------------------------ misc */

/** Scrollable extent: the used range plus a usable margin, never unbounded. */
export function virtualExtent(
  usedRows: number,
  usedCols: number,
): { rows: number; cols: number } {
  return {
    rows: Math.min(MAX_ROW + 1, Math.max(0, usedRows) + EXTENT_ROW_PAD),
    cols: Math.min(MAX_COL + 1, Math.max(0, usedCols) + EXTENT_COL_PAD),
  }
}

export type CellAlign = 'left' | 'right' | 'center'

/** Numbers, booleans and errors hug the right edge; everything else the left. */
export function cellAlign(kind: number): CellAlign {
  return kind === 2 || kind === 0 ? 'left' : 'right'
}

/**
 * A cell's alignment: an explicit format wins, otherwise the value's type
 * decides. Excel behaves the same way, which is why a number that arrives as
 * text suddenly jumps to the left and gives itself away.
 */
export function resolvedAlign(kind: number, align?: string): CellAlign {
  if (align === 'left' || align === 'right' || align === 'center') return align
  return cellAlign(kind)
}

/* ---------------------------------------------------------------- merges */

/**
 * Merged regions of the active sheet, in the form the painter needs.
 *
 * Lookup is a linear scan: a sheet has tens of merges, not thousands, and a
 * per-cell index would cost more to build every repaint than it saves.
 */
export class MergeMap {
  readonly ranges: readonly Range[]

  constructor(ranges: readonly Range[]) {
    this.ranges = ranges
  }

  /** Parse the A1 strings the engine reports. */
  static fromA1(list: readonly string[]): MergeMap {
    const out: Range[] = []
    for (const text of list) {
      const r = parseRangeA1(text)
      if (r) out.push(r)
    }
    return new MergeMap(out)
  }

  get isEmpty(): boolean {
    return this.ranges.length === 0
  }

  /** The merge covering an address, if any. */
  at(row: number, col: number): Range | null {
    for (const r of this.ranges) {
      if (row >= r.start.row && row <= r.end.row && col >= r.start.col && col <= r.end.col) {
        return r
      }
    }
    return null
  }

  /**
   * Where a click on this address should actually land. Clicking anywhere in
   * a merged block selects the whole block, so the selection can never sit on
   * a covered cell the user cannot see.
   */
  anchor(row: number, col: number): Addr {
    const m = this.at(row, col)
    return m ? m.start : { row, col }
  }

  /** Grow a selection so it contains every merge it partially overlaps. */
  expand(range: Range): Range {
    let out = range
    // One pass is not enough: absorbing a merge can bring the range into
    // contact with another one.
    for (let i = 0; i < this.ranges.length; i++) {
      let grew = false
      for (const m of this.ranges) {
        if (
          m.start.row > out.end.row ||
          m.end.row < out.start.row ||
          m.start.col > out.end.col ||
          m.end.col < out.start.col
        ) {
          continue
        }
        const next = {
          start: {
            row: Math.min(out.start.row, m.start.row),
            col: Math.min(out.start.col, m.start.col),
          },
          end: {
            row: Math.max(out.end.row, m.end.row),
            col: Math.max(out.end.col, m.end.col),
          },
        }
        if (
          next.start.row !== out.start.row ||
          next.start.col !== out.start.col ||
          next.end.row !== out.end.row ||
          next.end.col !== out.end.col
        ) {
          out = next
          grew = true
        }
      }
      if (!grew) break
    }
    return out
  }
}

const A1_CELL = /^\$?([A-Za-z]+)\$?(\d+)$/

function parseAddrA1(text: string): Addr | null {
  const m = A1_CELL.exec(text.trim())
  if (!m) return null
  let col = 0
  for (const ch of m[1].toUpperCase()) col = col * 26 + (ch.charCodeAt(0) - 64)
  const row = Number(m[2])
  if (!Number.isFinite(row) || row < 1) return null
  return { row: row - 1, col: col - 1 }
}

/** "B2" or "B2:D4" as the engine spells them. */
export function parseRangeA1(text: string): Range | null {
  const [a, b] = text.split(':')
  const start = parseAddrA1(a ?? '')
  if (!start) return null
  if (b === undefined) return { start, end: start }
  const end = parseAddrA1(b)
  if (!end) return null
  return mkRange(start, end)
}

/* ------------------------------------------------------------ autoscroll */

/** How fast a drag past the edge scrolls, in CSS pixels per frame. */
export const AUTOSCROLL_MAX_PX = 24
/** How far past the edge counts as "asking to scroll". */
export const AUTOSCROLL_BAND_PX = 32

/**
 * Scroll delta for a drag whose pointer has left the content box.
 *
 * The speed ramps with distance so nudging the edge creeps and dragging well
 * past it moves quickly — a fixed step makes selecting a long range either
 * unbearably slow or impossible to stop on the right row.
 */
export function autoscrollDelta(
  x: number,
  y: number,
  width: number,
  height: number,
  m: GridMetrics,
): { dx: number; dy: number } {
  const axis = (pos: number, lo: number, hi: number): number => {
    if (pos < lo) return -ramp(lo - pos)
    if (pos > hi) return ramp(pos - hi)
    return 0
  }
  return {
    dx: axis(x, m.headerWidth, width),
    dy: axis(y, m.headerHeight, height),
  }
}

function ramp(over: number): number {
  const t = Math.min(1, over / AUTOSCROLL_BAND_PX)
  return Math.max(1, Math.round(t * AUTOSCROLL_MAX_PX))
}

/**
 * Excel's "too narrow for this number" rendering: as many `#` as fit, so the
 * user sees the column is the problem rather than a truncated wrong number.
 */
export function overflowHashes(available: number, hashWidth: number): string {
  if (hashWidth <= 0) return '####'
  const n = Math.floor(available / hashWidth)
  return '#'.repeat(Math.min(32, Math.max(1, n)))
}

export function clampColWidth(w: number): number {
  return Math.max(MIN_COL_WIDTH, Math.round(w))
}

export function clampRowHeight(h: number): number {
  return Math.max(MIN_ROW_HEIGHT, Math.round(h))
}
