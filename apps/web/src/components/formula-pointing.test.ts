import { describe, expect, it } from 'vitest'
import { applyPointing, bandRef, pointingSlot } from './formula-pointing'

/** `pointingSlot` at the end of the string, which is where a caret usually is. */
const at = (value: string) => pointingSlot(value, value.length)

describe('pointingSlot', () => {
  it('offers an insertion where an operand is expected', () => {
    for (const value of ['=', '=AVERAGE(', '=SUM(A1,', '=1+', '=A1*', '=A1&', '=A1>']) {
      expect(at(value), value).toEqual({ start: value.length, end: value.length })
    }
  })

  it('offers to replace the reference the caret sits after', () => {
    expect(at('=A1')).toEqual({ start: 1, end: 3 })
    expect(at('=AVERAGE(A1:A3')).toEqual({ start: 9, end: 14 })
    expect(at('=SUM(B2)+C3')).toEqual({ start: 9, end: 11 })
  })

  it('replaces a sheet-qualified reference whole', () => {
    // Leaving `Sheet2!` behind and putting the new address after it would
    // silently point at a cell on a sheet the user is no longer looking at.
    expect(at('=Sheet2!B4')).toEqual({ start: 1, end: 10 })
    expect(at("='Q1 Actuals'!B4")).toEqual({ start: 1, end: 16 })
  })

  it('replaces an absolute reference whole', () => {
    expect(at('=$A$1')).toEqual({ start: 1, end: 5 })
  })

  it('refuses when the formula is not expecting an operand', () => {
    // A finished formula: clicking elsewhere means "go there", and this is
    // what lets it still commit.
    expect(at('=SUM(A1:A3)')).toBeNull()
    expect(at('=A1+2')).toBeNull()
  })

  it('refuses inside a text literal', () => {
    expect(at('=CONCAT("total ')).toBeNull()
    expect(at('=CONCAT("a""b ')).toBeNull()
    // Closed again, so the next argument position is pointable.
    expect(at('=CONCAT("total",')).toEqual({ start: 16, end: 16 })
  })

  it('refuses anything that is not a formula', () => {
    expect(at('123')).toBeNull()
    expect(at('A1')).toBeNull()
    expect(at('')).toBeNull()
  })

  it('treats a function name that is also an address as an address', () => {
    // `LOG10` is a real cell — column LOG, row 10 — and Excel has the same
    // ambiguity. It resolves it the same way: the name is a reference until a
    // `(` turns it into a call, so pointing here replaces it.
    expect(at('=LOG10')).toEqual({ start: 1, end: 6 })
    expect(at('=LOG10(')).toEqual({ start: 7, end: 7 })
    // `AVERAGE` is not address-shaped, so there is nothing to replace and
    // nowhere to insert.
    expect(at('=AVERAGE')).toBeNull()
  })

  it('reads the caret rather than the end of the string', () => {
    // Caret just after `SUM(`, with a reference already typed to its right:
    // an insertion, leaving `A1)` alone.
    expect(pointingSlot('=SUM(A1)', 5)).toEqual({ start: 5, end: 5 })
    // Caret in the middle of a reference. Only a *complete* reference is
    // replaceable, and `A` alone is not one, so this declines rather than
    // splicing an address into the middle of another. Reaching it takes a
    // deliberate click into the middle of a reference and then a click on the
    // grid, and the outcome — commit and move — is at least predictable.
    expect(pointingSlot('=SUM(A1)', 6)).toBeNull()
  })

  it('clamps a caret outside the string', () => {
    expect(pointingSlot('=1+', 99)).toEqual({ start: 3, end: 3 })
    expect(pointingSlot('=1+', -1)).toBeNull()
  })
})

describe('applyPointing', () => {
  it('inserts at an empty slot and puts the caret after the reference', () => {
    expect(applyPointing('=AVERAGE(', { start: 9, end: 9 }, 'A1:A3')).toEqual({
      value: '=AVERAGE(A1:A3',
      caret: 14,
    })
  })

  it('replaces without disturbing what follows', () => {
    expect(applyPointing('=SUM(A1)*2', { start: 5, end: 7 }, 'B4:B9')).toEqual({
      value: '=SUM(B4:B9)*2',
      caret: 10,
    })
  })
})

describe('bandRef', () => {
  it('names whole columns and whole rows', () => {
    expect(bandRef('col', 0, 0)).toBe('A:A')
    expect(bandRef('col', 0, 2)).toBe('A:C')
    expect(bandRef('row', 0, 4)).toBe('1:5')
  })

  it('puts the ends in order however the drag went', () => {
    // Dragging right-to-left across headers is ordinary, and `C:A` is not a
    // reference the engine reads.
    expect(bandRef('col', 4, 1)).toBe('B:E')
    expect(bandRef('row', 9, 2)).toBe('3:10')
  })

  it('handles columns past Z', () => {
    expect(bandRef('col', 26, 27)).toBe('AA:AB')
  })
})
