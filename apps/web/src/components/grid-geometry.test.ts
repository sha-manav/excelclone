import { describe, expect, it } from 'vitest'
import type { Range } from '../engine/actions'
import {
  DEFAULT_COL_WIDTH,
  DEFAULT_ROW_HEIGHT,
  MAX_COL,
  cellAlign,
  cellRect,
  clampColWidth,
  colWidth,
  columnAtX,
  columnLeft,
  columnOffsets,
  createMetrics,
  fillHandleRect,
  fillTarget,
  firstVisibleRow,
  hitTest,
  isRowHidden,
  lastVisibleRow,
  moveAddr,
  nextVisibleRow,
  overflowHashes,
  pageJump,
  pointInRect,
  rangeRect,
  rowAtY,
  rowHeight,
  rowOffsets,
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

const HW = 46 // header width
const HH = 24 // header height

/** Plain 100x24 grid, 200 rows x 50 cols of extent. */
const plain = createMetrics({ rowCount: 200, colCount: 50 })

/** Column 1 is 50 wide, column 3 is 200 wide. */
const sized = createMetrics({
  rowCount: 200,
  colCount: 50,
  colWidths: new Map([
    [1, 50],
    [3, 200],
  ]),
  rowHeights: new Map([[2, 40]]),
})

/** Rows 1 and 2 hidden by a filter; row 2 also has a custom height. */
const filtered = createMetrics({
  rowCount: 200,
  colCount: 50,
  hiddenRows: [1, 2],
  rowHeights: new Map([[2, 40]]),
})

const r = (r0: number, c0: number, r1: number, c1: number): Range => ({
  start: { row: r0, col: c0 },
  end: { row: r1, col: c1 },
})

describe('sizes', () => {
  it('falls back to the defaults', () => {
    expect(colWidth(plain, 7)).toBe(DEFAULT_COL_WIDTH)
    expect(rowHeight(plain, 7)).toBe(DEFAULT_ROW_HEIGHT)
  })

  it('honours per-column and per-row overrides', () => {
    expect(colWidth(sized, 1)).toBe(50)
    expect(colWidth(sized, 3)).toBe(200)
    expect(rowHeight(sized, 2)).toBe(40)
  })

  it('gives hidden rows zero height regardless of any override', () => {
    expect(isRowHidden(filtered, 2)).toBe(true)
    expect(rowHeight(filtered, 1)).toBe(0)
    expect(rowHeight(filtered, 2)).toBe(0)
    expect(rowHeight(filtered, 3)).toBe(DEFAULT_ROW_HEIGHT)
  })
})

describe('columnOffsets', () => {
  it('accumulates uniform widths', () => {
    expect(columnOffsets(plain, 0, 3)).toEqual([0, 100, 200, 300])
  })

  it('accumulates custom widths from an arbitrary start', () => {
    expect(columnOffsets(sized, 1, 3)).toEqual([0, 50, 150, 350])
  })

  it('returns just the origin for a zero count', () => {
    expect(columnOffsets(plain, 4, 0)).toEqual([0])
  })
})

describe('rowOffsets', () => {
  it('accumulates uniform heights', () => {
    expect(rowOffsets(plain, 0, 3)).toEqual([0, 24, 48, 72])
  })

  it('collapses hidden rows to zero-width steps', () => {
    expect(rowOffsets(filtered, 0, 4)).toEqual([0, 24, 24, 24, 48])
  })

  it('starts inside a hidden run without drifting', () => {
    expect(rowOffsets(filtered, 1, 3)).toEqual([0, 0, 0, 24])
  })
})

describe('columnLeft / rowTop', () => {
  it('matches a manual accumulation with overrides', () => {
    expect(columnLeft(sized, 0)).toBe(0)
    expect(columnLeft(sized, 2)).toBe(150)
    expect(columnLeft(sized, 4)).toBe(450)
  })

  it('skips hidden rows entirely', () => {
    expect(rowTop(filtered, 1)).toBe(24)
    expect(rowTop(filtered, 3)).toBe(24)
    expect(rowTop(filtered, 4)).toBe(48)
  })

  it('agrees with rowOffsets on the same span', () => {
    const offsets = rowOffsets(filtered, 0, 10)
    for (let i = 0; i <= 10; i++) expect(rowTop(filtered, i)).toBe(offsets[i])
  })
})

describe('totals', () => {
  it('sums the whole virtual extent', () => {
    expect(totalWidth(plain)).toBe(50 * 100)
    expect(totalHeight(plain)).toBe(200 * 24)
  })

  it('applies overrides once', () => {
    expect(totalWidth(sized)).toBe(50 * 100 - 50 + 100)
    expect(totalHeight(sized)).toBe(200 * 24 + 16)
  })

  it('drops hidden rows from the total, override or not', () => {
    expect(totalHeight(filtered)).toBe(198 * 24)
  })

  it('ignores overrides beyond the extent', () => {
    const m = createMetrics({ colCount: 2, colWidths: new Map([[9, 500]]) })
    expect(totalWidth(m)).toBe(200)
  })
})

describe('columnAtX / rowAtY', () => {
  it('picks the column containing a pixel', () => {
    expect(columnAtX(plain, 0)).toBe(0)
    expect(columnAtX(plain, 99)).toBe(0)
    expect(columnAtX(plain, 100)).toBe(1)
    expect(columnAtX(sized, 100)).toBe(1)
    expect(columnAtX(sized, 149)).toBe(1)
    expect(columnAtX(sized, 150)).toBe(2)
  })

  it('clamps outside the extent', () => {
    expect(columnAtX(plain, -40)).toBe(0)
    expect(columnAtX(plain, 1e9)).toBe(49)
  })

  it('never lands on a hidden row', () => {
    expect(rowAtY(filtered, 0)).toBe(0)
    expect(rowAtY(filtered, 23)).toBe(0)
    expect(rowAtY(filtered, 24)).toBe(3)
    expect(rowAtY(filtered, 47)).toBe(3)
    expect(rowAtY(filtered, 48)).toBe(4)
  })

  it('handles a hidden first row', () => {
    const m = createMetrics({ rowCount: 10, hiddenRows: [0] })
    expect(rowAtY(m, 0)).toBe(1)
    expect(firstVisibleRow(m)).toBe(1)
  })

  it('handles a fully hidden sheet without looping forever', () => {
    const m = createMetrics({ rowCount: 3, hiddenRows: [0, 1, 2] })
    expect(rowAtY(m, 10)).toBe(0)
    expect(firstVisibleRow(m)).toBe(0)
    expect(lastVisibleRow(m)).toBe(0)
  })
})

describe('nextVisibleRow', () => {
  it('steps over a hidden run', () => {
    expect(nextVisibleRow(filtered, 0, 1)).toBe(3)
    expect(nextVisibleRow(filtered, 3, -1)).toBe(0)
  })

  it('stays put at the top edge', () => {
    expect(nextVisibleRow(plain, 0, -1)).toBe(0)
  })

  it('stays put when every row above is hidden', () => {
    const m = createMetrics({ rowCount: 10, hiddenRows: [0, 1, 2] })
    expect(nextVisibleRow(m, 3, -1)).toBe(3)
  })

  it('is a no-op for a zero delta', () => {
    expect(nextVisibleRow(filtered, 5, 0)).toBe(5)
  })
})

describe('visibleRange', () => {
  it('covers exactly the rows and columns on screen', () => {
    const v = visibleRange(0, 0, HW + 250, HH + 100, plain)
    expect(v).toEqual({ firstRow: 0, lastRow: 4, firstCol: 0, lastCol: 2 })
  })

  it('follows the scroll offsets', () => {
    const v = visibleRange(48, 100, HW + 250, HH + 100, plain)
    expect(v).toEqual({ firstRow: 2, lastRow: 6, firstCol: 1, lastCol: 3 })
  })

  it('reports an empty span for a zero-size viewport', () => {
    const v = visibleRange(0, 0, 0, 0, plain)
    expect(v.lastRow).toBeLessThan(v.firstRow)
    expect(v.lastCol).toBeLessThan(v.firstCol)
  })

  it('reports an empty span when only the headers fit', () => {
    const v = visibleRange(0, 0, HW, HH, plain)
    expect(v.lastRow).toBeLessThan(v.firstRow)
  })

  it('never starts or ends on a hidden row', () => {
    const v = visibleRange(0, 0, HW + 250, HH + 100, filtered)
    expect(v.firstRow).toBe(0)
    expect(v.lastRow).toBe(6)
    expect(isRowHidden(filtered, v.lastRow)).toBe(false)
  })

  it('handles a hidden row exactly at the scroll boundary', () => {
    // scrollTop 24 is the first pixel of row 3, rows 1-2 being hidden.
    const v = visibleRange(24, 0, HW + 100, HH + 24, filtered)
    expect(v.firstRow).toBe(3)
    expect(v.lastRow).toBe(3)
  })

  it('clamps at the far edge of the extent', () => {
    const v = visibleRange(1e9, 1e9, HW + 500, HH + 500, plain)
    expect(v.firstRow).toBe(199)
    expect(v.lastRow).toBe(199)
    expect(v.firstCol).toBe(49)
    expect(v.lastCol).toBe(49)
  })
})

describe('hitTest', () => {
  it('finds the corner box', () => {
    expect(hitTest(4, 4, 0, 0, plain)).toEqual({ kind: 'corner' })
  })

  it('finds a column header', () => {
    expect(hitTest(HW + 50, 5, 0, 0, plain)).toEqual({ kind: 'col-header', col: 0 })
  })

  it('finds a row header', () => {
    expect(hitTest(5, HH + 30, 0, 0, plain)).toEqual({ kind: 'row-header', row: 1 })
  })

  it('finds a cell', () => {
    expect(hitTest(HW + 150, HH + 30, 0, 0, plain)).toEqual({
      kind: 'cell',
      row: 1,
      col: 1,
    })
  })

  it('accounts for scroll offsets', () => {
    expect(hitTest(HW + 10, HH + 10, 48, 200, plain)).toEqual({
      kind: 'cell',
      row: 2,
      col: 2,
    })
  })

  it('reports the border from either side and attributes it to the left column', () => {
    expect(hitTest(HW + 98, 5, 0, 0, plain)).toEqual({ kind: 'col-border', col: 0 })
    expect(hitTest(HW + 102, 5, 0, 0, plain)).toEqual({ kind: 'col-border', col: 0 })
  })

  it('does not report a border at the very left of column A', () => {
    expect(hitTest(HW + 1, 5, 0, 0, plain)).toEqual({ kind: 'col-header', col: 0 })
  })

  it('skips hidden rows when picking a row header', () => {
    expect(hitTest(5, HH + 30, 0, 0, filtered)).toEqual({ kind: 'row-header', row: 3 })
  })

  it('reports a row border from either side and attributes it to the row above', () => {
    expect(hitTest(5, HH + 22, 0, 0, plain)).toEqual({ kind: 'row-border', row: 0 })
    expect(hitTest(5, HH + 26, 0, 0, plain)).toEqual({ kind: 'row-border', row: 0 })
    // The top edge of row 1 is not a border anyone can drag.
    expect(hitTest(5, HH + 1, 0, 0, plain)).toEqual({ kind: 'row-header', row: 0 })
  })

  it('does not offer a border for a row a filter has hidden', () => {
    // Rows 1 and 2 are hidden in `filtered`, so every pixel they would have
    // occupied belongs to row 3 — and the border there resizes row 3, not the
    // invisible row 2, which the user cannot see to judge.
    expect(hitTest(5, HH + 25, 0, 0, filtered)).toEqual({ kind: 'row-header', row: 3 })
  })

  it('respects a custom tolerance', () => {
    expect(hitTest(HW + 90, 5, 0, 0, plain, 12)).toEqual({ kind: 'col-border', col: 0 })
    expect(hitTest(HW + 90, 5, 0, 0, plain, 2)).toEqual({ kind: 'col-header', col: 0 })
  })
})

describe('rects', () => {
  it('places a cell in viewport space', () => {
    expect(cellRect(plain, 2, 1, 24, 50)).toEqual({ x: HW + 50, y: HH + 24, w: 100, h: 24 })
  })

  it('spans a whole range', () => {
    expect(rangeRect(plain, r(1, 1, 2, 3), 0, 0)).toEqual({
      x: HW + 100,
      y: HH + 24,
      w: 300,
      h: 48,
    })
  })

  it('collapses hidden rows inside a range', () => {
    expect(rangeRect(filtered, r(0, 0, 3, 0), 0, 0).h).toBe(48)
  })

  it('centres the fill handle on the bottom-right corner', () => {
    const box = rangeRect(plain, r(0, 0, 0, 0), 0, 0)
    const handle = fillHandleRect(plain, r(0, 0, 0, 0), 0, 0)
    expect(handle.x + handle.w / 2).toBe(box.x + box.w)
    expect(handle.y + handle.h / 2).toBe(box.y + box.h)
    expect(pointInRect(box.x + box.w, box.y + box.h, handle)).toBe(true)
    expect(pointInRect(box.x, box.y, handle)).toBe(false)
  })

  it('honours the slop when testing a point', () => {
    const rect = { x: 10, y: 10, w: 4, h: 4 }
    expect(pointInRect(8, 8, rect)).toBe(false)
    expect(pointInRect(8, 8, rect, 3)).toBe(true)
  })
})

describe('fillTarget', () => {
  const source = r(0, 0, 1, 1)

  it('extends downward', () => {
    expect(fillTarget(source, 5, 1)).toEqual(r(0, 0, 5, 1))
  })

  it('extends rightward', () => {
    expect(fillTarget(source, 1, 5)).toEqual(r(0, 0, 1, 5))
  })

  it('extends upward, still containing the source', () => {
    const src = r(4, 2, 5, 3)
    expect(fillTarget(src, 1, 2)).toEqual(r(1, 2, 5, 3))
  })

  it('extends leftward, still containing the source', () => {
    const src = r(4, 4, 5, 5)
    expect(fillTarget(src, 4, 1)).toEqual(r(4, 1, 5, 5))
  })

  it('returns the source unchanged when the drag stays inside it', () => {
    expect(fillTarget(source, 1, 1)).toEqual(source)
    expect(fillTarget(source, 0, 0)).toEqual(source)
  })

  it('picks the dominant axis when the drag is diagonal', () => {
    expect(fillTarget(source, 6, 3)).toEqual(r(0, 0, 6, 1))
    expect(fillTarget(source, 3, 6)).toEqual(r(0, 0, 1, 6))
  })

  it('prefers rows on an exact tie', () => {
    expect(fillTarget(source, 3, 3)).toEqual(r(0, 0, 3, 1))
  })

  it('never mutates the source', () => {
    const src = r(2, 2, 3, 3)
    const copy = structuredClone(src)
    fillTarget(src, 9, 2)
    expect(src).toEqual(copy)
  })
})

describe('scrollToInclude', () => {
  const w = HW + 300
  const h = HH + 96

  it('leaves an already visible cell alone', () => {
    expect(scrollToInclude(1, 1, 0, 0, w, h, plain)).toEqual({
      scrollTop: 0,
      scrollLeft: 0,
    })
  })

  it('scrolls down by exactly the overhang', () => {
    expect(scrollToInclude(4, 0, 0, 0, w, h, plain)).toEqual({
      scrollTop: 24,
      scrollLeft: 0,
    })
  })

  it('scrolls right by exactly the overhang', () => {
    expect(scrollToInclude(0, 3, 0, 0, w, h, plain)).toEqual({
      scrollTop: 0,
      scrollLeft: 100,
    })
  })

  it('scrolls back up to the cell top', () => {
    expect(scrollToInclude(2, 0, 200, 0, w, h, plain).scrollTop).toBe(48)
  })

  it('never produces a negative offset', () => {
    const out = scrollToInclude(0, 0, 500, 500, w, h, plain)
    expect(out).toEqual({ scrollTop: 0, scrollLeft: 0 })
  })

  it('is a no-op for a zero-size viewport', () => {
    expect(scrollToInclude(50, 50, 17, 33, 0, 0, plain)).toEqual({
      scrollTop: 17,
      scrollLeft: 33,
    })
  })

  it('uses collapsed offsets when rows are hidden', () => {
    // Row 5 sits at y=72 once rows 1-2 are gone, so a 96px box already shows it.
    expect(scrollToInclude(5, 0, 0, 0, w, h, filtered).scrollTop).toBe(0)
    expect(scrollToInclude(7, 0, 0, 0, w, h, filtered).scrollTop).toBe(48)
  })
})

describe('pageJump', () => {
  it('moves about one viewport down', () => {
    expect(pageJump(plain, 0, 'down', HH + 100)).toBe(5)
  })

  it('moves about one viewport up', () => {
    expect(pageJump(plain, 10, 'up', HH + 100)).toBe(5)
  })

  it('clamps at the top', () => {
    expect(pageJump(plain, 2, 'up', HH + 1000)).toBe(0)
  })

  it('clamps at the bottom of the extent', () => {
    expect(pageJump(plain, 195, 'down', HH + 1000)).toBe(199)
  })

  it('moves at least one row for a tiny viewport', () => {
    expect(pageJump(plain, 3, 'down', HH)).toBe(4)
  })

  it('counts only visible rows', () => {
    expect(pageJump(filtered, 0, 'down', HH + 48)).toBe(4)
  })
})

describe('selection helpers', () => {
  it('normalises a backwards drag', () => {
    const sel = selectionFrom({ row: 5, col: 5 }, { row: 2, col: 1 })
    expect(sel.anchor).toEqual({ row: 5, col: 5 })
    expect(sel.range).toEqual(r(2, 1, 5, 5))
  })

  it('collapses to a single cell', () => {
    const sel = selectionAt({ row: 3, col: 4 })
    expect(sel.range).toEqual(r(3, 4, 3, 4))
  })

  it('reports the moving corner opposite the anchor', () => {
    expect(selectionFocus(selectionFrom({ row: 1, col: 1 }, { row: 4, col: 6 }))).toEqual({
      row: 4,
      col: 6,
    })
    expect(selectionFocus(selectionFrom({ row: 4, col: 6 }, { row: 1, col: 1 }))).toEqual({
      row: 1,
      col: 1,
    })
    expect(selectionFocus(selectionAt({ row: 2, col: 2 }))).toEqual({ row: 2, col: 2 })
  })
})

describe('moveAddr', () => {
  it('moves in each direction', () => {
    const from = { row: 5, col: 5 }
    expect(moveAddr(plain, from, 'up')).toEqual({ row: 4, col: 5 })
    expect(moveAddr(plain, from, 'down')).toEqual({ row: 6, col: 5 })
    expect(moveAddr(plain, from, 'left')).toEqual({ row: 5, col: 4 })
    expect(moveAddr(plain, from, 'right')).toEqual({ row: 5, col: 6 })
    expect(moveAddr(plain, from, 'none')).toBe(from)
  })

  it('clamps at the origin', () => {
    expect(moveAddr(plain, { row: 0, col: 0 }, 'up')).toEqual({ row: 0, col: 0 })
    expect(moveAddr(plain, { row: 0, col: 0 }, 'left')).toEqual({ row: 0, col: 0 })
  })

  it('clamps at the last column', () => {
    expect(moveAddr(plain, { row: 0, col: MAX_COL }, 'right').col).toBe(MAX_COL)
  })

  it('hops over hidden rows', () => {
    expect(moveAddr(filtered, { row: 0, col: 2 }, 'down')).toEqual({ row: 3, col: 2 })
  })
})

describe('usedEdge', () => {
  it('jumps to the used-range corners', () => {
    const from = { row: 4, col: 4 }
    expect(usedEdge(plain, from, 'up', 20, 8)).toEqual({ row: 0, col: 4 })
    expect(usedEdge(plain, from, 'down', 20, 8)).toEqual({ row: 19, col: 4 })
    expect(usedEdge(plain, from, 'left', 20, 8)).toEqual({ row: 4, col: 0 })
    expect(usedEdge(plain, from, 'right', 20, 8)).toEqual({ row: 4, col: 7 })
    expect(usedEdge(plain, from, 'none', 20, 8)).toBe(from)
  })

  it('never moves backwards past an empty used range', () => {
    const from = { row: 30, col: 30 }
    expect(usedEdge(plain, from, 'down', 0, 0).row).toBe(30)
    expect(usedEdge(plain, from, 'right', 0, 0).col).toBe(30)
  })

  it('lands on a visible row at the bottom edge', () => {
    const m = createMetrics({ rowCount: 200, hiddenRows: [8, 9] })
    expect(usedEdge(m, { row: 0, col: 0 }, 'down', 10, 4).row).toBe(7)
  })

  it('lands on a visible row at the top edge', () => {
    const m = createMetrics({ rowCount: 200, hiddenRows: [0, 1] })
    expect(usedEdge(m, { row: 9, col: 0 }, 'up', 10, 4).row).toBe(2)
  })
})

describe('virtualExtent', () => {
  it('pads the used range', () => {
    expect(virtualExtent(10, 3)).toEqual({ rows: 210, cols: 53 })
  })

  it('gives an empty sheet a usable extent', () => {
    expect(virtualExtent(0, 0)).toEqual({ rows: 200, cols: 50 })
  })

  it('caps at the sheet limits', () => {
    expect(virtualExtent(2_000_000, 90_000)).toEqual({
      rows: 1_048_576,
      cols: 16_384,
    })
  })
})

describe('text helpers', () => {
  it('aligns by cell kind', () => {
    expect(cellAlign(0)).toBe('left')
    expect(cellAlign(1)).toBe('right')
    expect(cellAlign(2)).toBe('left')
    expect(cellAlign(3)).toBe('right')
    expect(cellAlign(4)).toBe('right')
  })

  it('fills the available width with hashes', () => {
    expect(overflowHashes(40, 8)).toBe('#####')
    expect(overflowHashes(0, 8)).toBe('#')
    expect(overflowHashes(1000, 8)).toBe('#'.repeat(32))
    expect(overflowHashes(40, 0)).toBe('####')
  })

  it('clamps column widths to the minimum', () => {
    expect(clampColWidth(4)).toBe(24)
    expect(clampColWidth(120.4)).toBe(120)
  })
})
