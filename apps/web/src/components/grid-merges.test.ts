/**
 * Layout maths added in M5: merged-range lookup, drag autoscroll, A1 parsing
 * and the alignment override.
 *
 * These sit in the same "easy to get subtly wrong, impossible to eyeball"
 * category as the rest of grid-geometry — a merge lookup that is off by one
 * puts the cursor on a cell the user cannot see.
 */

import { describe, expect, it } from 'vitest'
import {
  AUTOSCROLL_MAX_PX,
  MergeMap,
  autoscrollDelta,
  createMetrics,
  parseRangeA1,
  resolvedAlign,
} from './grid-geometry'

const m = createMetrics({ rowCount: 100, colCount: 40 })

describe('parseRangeA1', () => {
  it('reads single cells and ranges', () => {
    expect(parseRangeA1('A1')).toEqual({
      start: { row: 0, col: 0 },
      end: { row: 0, col: 0 },
    })
    expect(parseRangeA1('B2:D4')).toEqual({
      start: { row: 1, col: 1 },
      end: { row: 3, col: 3 },
    })
  })

  it('handles multi-letter columns and absolute markers', () => {
    expect(parseRangeA1('AA10')?.start).toEqual({ row: 9, col: 26 })
    expect(parseRangeA1('$B$3')?.start).toEqual({ row: 2, col: 1 })
  })

  it('normalizes a reversed range rather than producing a negative one', () => {
    expect(parseRangeA1('D4:B2')).toEqual({
      start: { row: 1, col: 1 },
      end: { row: 3, col: 3 },
    })
  })

  it('rejects nonsense instead of guessing', () => {
    expect(parseRangeA1('')).toBeNull()
    expect(parseRangeA1('A0')).toBeNull()
    expect(parseRangeA1('1A')).toBeNull()
    expect(parseRangeA1('A1:')).toBeNull()
  })
})

describe('MergeMap', () => {
  const merges = MergeMap.fromA1(['B2:D4', 'F1:G1'])

  it('finds the merge covering an address', () => {
    expect(merges.at(2, 2)).toEqual({
      start: { row: 1, col: 1 },
      end: { row: 3, col: 3 },
    })
    expect(merges.at(0, 0)).toBeNull()
    // Just outside on each side.
    expect(merges.at(0, 1)).toBeNull()
    expect(merges.at(4, 1)).toBeNull()
    expect(merges.at(1, 0)).toBeNull()
    expect(merges.at(1, 4)).toBeNull()
  })

  it('redirects a click inside a merge to its anchor', () => {
    expect(merges.anchor(3, 3)).toEqual({ row: 1, col: 1 })
    expect(merges.anchor(9, 9)).toEqual({ row: 9, col: 9 })
  })

  it('expands a selection to contain every merge it touches', () => {
    const sel = { start: { row: 3, col: 3 }, end: { row: 5, col: 5 } }
    expect(merges.expand(sel)).toEqual({
      start: { row: 1, col: 1 },
      end: { row: 5, col: 5 },
    })
  })

  it('keeps expanding while growing brings new merges into range', () => {
    // C1:C3 does not touch cell B2, but B2:C2 does — and absorbing it drags
    // the selection into column C, which then touches C1:C3. The merges are
    // listed in the order that makes one pass insufficient: a single sweep
    // considers C1:C3 before the selection has reached column C, and would
    // stop with a merge half-selected.
    const chain = MergeMap.fromA1(['C1:C3', 'B2:C2'])
    const sel = { start: { row: 1, col: 1 }, end: { row: 1, col: 1 } }
    expect(chain.expand(sel)).toEqual({
      start: { row: 0, col: 1 },
      end: { row: 2, col: 2 },
    })
  })

  it('leaves a selection that touches nothing alone', () => {
    const sel = { start: { row: 8, col: 8 }, end: { row: 9, col: 9 } }
    expect(merges.expand(sel)).toEqual(sel)
  })

  it('ignores entries it cannot parse rather than throwing', () => {
    const map = MergeMap.fromA1(['B2:D4', 'not-a-range'])
    expect(map.ranges).toHaveLength(1)
  })

  it('reports emptiness so the painter can skip the whole pass', () => {
    expect(new MergeMap([]).isEmpty).toBe(true)
    expect(merges.isEmpty).toBe(false)
  })
})

describe('autoscrollDelta', () => {
  const W = 400
  const H = 300

  it('is still while the pointer is inside the content box', () => {
    expect(autoscrollDelta(200, 150, W, H, m)).toEqual({ dx: 0, dy: 0 })
  })

  it('scrolls down and right when the pointer leaves past the far edges', () => {
    const d = autoscrollDelta(W + 10, H + 10, W, H, m)
    expect(d.dx).toBeGreaterThan(0)
    expect(d.dy).toBeGreaterThan(0)
  })

  it('scrolls back when the pointer crosses into the headers', () => {
    const d = autoscrollDelta(0, 0, W, H, m)
    expect(d.dx).toBeLessThan(0)
    expect(d.dy).toBeLessThan(0)
  })

  it('ramps with distance and then caps', () => {
    const near = autoscrollDelta(W + 2, 150, W, H, m).dx
    const far = autoscrollDelta(W + 20, 150, W, H, m).dx
    const absurd = autoscrollDelta(W + 5000, 150, W, H, m).dx
    expect(near).toBeLessThan(far)
    expect(absurd).toBe(AUTOSCROLL_MAX_PX)
  })

  it('never returns a zero step for a pointer that is outside', () => {
    // A rounded-down ramp of 0 would look like "outside the box but not
    // scrolling", which reads as a frozen grid.
    expect(autoscrollDelta(W + 0.4, 150, W, H, m).dx).toBeGreaterThanOrEqual(1)
  })

  it('treats the axes independently', () => {
    expect(autoscrollDelta(W + 10, 150, W, H, m).dy).toBe(0)
    expect(autoscrollDelta(200, H + 10, W, H, m).dx).toBe(0)
  })
})

describe('resolvedAlign', () => {
  const KIND_NUMBER = 1
  const KIND_TEXT = 2

  it('falls back to the value type when no format says otherwise', () => {
    expect(resolvedAlign(KIND_NUMBER)).toBe('right')
    expect(resolvedAlign(KIND_TEXT)).toBe('left')
  })

  it('lets an explicit alignment win over the type', () => {
    expect(resolvedAlign(KIND_NUMBER, 'left')).toBe('left')
    expect(resolvedAlign(KIND_TEXT, 'center')).toBe('center')
  })

  it('ignores an alignment it does not understand', () => {
    expect(resolvedAlign(KIND_NUMBER, 'justify')).toBe('right')
  })
})
