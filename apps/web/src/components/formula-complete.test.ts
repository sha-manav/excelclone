// The engine's own source, imported as text. Reading it through Vite rather
// than `node:fs` keeps the app's TypeScript free of Node types — and makes the
// dependency visible in the import graph instead of hidden in a path string.
import engineFunctions from '../../../../crates/engine/src/functions/mod.rs?raw'
import { describe, expect, it } from 'vitest'
import { acceptCompletion, completionAt, describe as about } from './formula-complete'
import { FUNCTION_HELP, signature } from './function-help'

/** A slice of the real list, enough to exercise ordering and prefixes. */
const NAMES = [
  'SUM',
  'SUMIF',
  'SUMIFS',
  'SUMPRODUCT',
  'AVERAGE',
  'AVERAGEIF',
  'ABS',
  'IF',
  'IFS',
  'IFERROR',
  'COUNT',
]

const at = (value: string) => completionAt(value, value.length, NAMES)

describe('completionAt', () => {
  it('offers matches for a prefix', () => {
    expect(at('=AVE')).toEqual({
      prefix: 'AVE',
      start: 1,
      names: ['AVERAGE', 'AVERAGEIF'],
    })
  })

  it('is case insensitive and reports the prefix uppercased', () => {
    expect(at('=ave')?.names).toEqual(['AVERAGE', 'AVERAGEIF'])
  })

  it('puts the shorter name first', () => {
    // `SUM` is asked for far more often than `SUMPRODUCT`; a menu that leads
    // with the rarer one is a menu people learn to dismiss.
    expect(at('=SU')?.names).toEqual(['SUM', 'SUMIF', 'SUMIFS', 'SUMPRODUCT'])
  })

  it('offers inside a nested call', () => {
    expect(at('=SUM(IF')?.names).toEqual(['IF', 'IFS', 'IFERROR'])
    expect(at('=SUM(A1, AB')?.names).toEqual(['ABS'])
    expect(at('=1+AB')?.names).toEqual(['ABS'])
  })

  it('keeps offering longer names once one matches exactly', () => {
    // Typing `SUM` should still show `SUMIF`, or the menu vanishes at the
    // moment it is most useful.
    expect(at('=SUM')?.names).toEqual(['SUM', 'SUMIF', 'SUMIFS', 'SUMPRODUCT'])
  })

  it('says nothing when only the exact name is left', () => {
    expect(at('=COUNT')).toBeNull()
  })

  it('says nothing for a name that follows a value', () => {
    // `=A1andB2` is not a call, and completing it would be worse than silence.
    expect(at('=A1AB')).toBeNull()
    expect(at('=2SU')).toBeNull()
  })

  it('says nothing without a prefix, or for a non-formula', () => {
    expect(at('=')).toBeNull()
    expect(at('=SUM(')).toBeNull()
    expect(at('SUM')).toBeNull()
    expect(at('')).toBeNull()
  })

  it('waits for a second letter', () => {
    // A single letter after `=` is far more often a reference, and the menu
    // would land on top of the very cells about to be pointed at.
    expect(at('=A')).toBeNull()
    expect(at('=AB')?.names).toEqual(['ABS'])
  })

  it('says nothing for a prefix nothing matches', () => {
    expect(at('=ZZZ')).toBeNull()
  })

  it('reads the caret rather than the end of the string', () => {
    // Caret after `AV`, with a stale `ERAGE(` to its right.
    expect(completionAt('=AVERAGE(', 3, NAMES)).toEqual({
      prefix: 'AV',
      start: 1,
      names: ['AVERAGE', 'AVERAGEIF'],
    })
  })
})

describe('acceptCompletion', () => {
  it('writes the name with its bracket and puts the caret inside', () => {
    const c = completionAt('=AVE', 4, NAMES)!
    expect(acceptCompletion('=AVE', c, 'AVERAGE')).toEqual({
      value: '=AVERAGE(',
      caret: 9,
    })
  })

  it('keeps whatever followed the prefix', () => {
    const c = completionAt('=SU+1', 3, NAMES)!
    expect(acceptCompletion('=SU+1', c, 'SUM')).toEqual({
      value: '=SUM(+1',
      caret: 5,
    })
  })

  it('does not double a bracket the user already typed', () => {
    // Caret back at the end of the name, with the bracket already to its
    // right — what happens when somebody types the call, goes back to change
    // the name, and accepts a suggestion.
    const c = completionAt('=AVERAGE(', 8, NAMES)!
    expect(acceptCompletion('=AVERAGE(', c, 'AVERAGE')).toEqual({
      value: '=AVERAGE(',
      caret: 9,
    })
  })
})

describe('function help', () => {
  it('renders a signature', () => {
    expect(signature('AVERAGE')).toBe('AVERAGE(number1, [number2], …)')
    expect(signature('TODAY')).toBe('TODAY()')
  })

  it('falls back to the bare name for a function it has no entry for', () => {
    // A new engine function completes on the day it lands, hint or no hint.
    expect(signature('NEWTHING')).toBe('NEWTHING')
    expect(about('NEWTHING')).toBe('')
  })

  it('describes every function it claims to know', () => {
    for (const [name, help] of Object.entries(FUNCTION_HELP)) {
      expect(help.about, name).not.toBe('')
      // Lower case and no full stop: these sit beside the name, not as prose.
      // A spreadsheet literal — TRUE, FALSE, #N/A — is allowed to lead,
      // because writing "true when all are true" would be describing a
      // different value from the one the function returns.
      const first = help.about.split(' ')[0]
      expect(first === first.toUpperCase() || first === first.toLowerCase(), name).toBe(true)
      expect(help.about.endsWith('.'), name).toBe(false)
    }
  })

  it('covers exactly the functions the engine implements', () => {
    // The engine owns which functions exist and this file owns how to explain
    // them, so the two have to be checked against each other somewhere. Doing
    // it here means a function added in Rust fails a web test until it has a
    // hint — which is a cheap way to keep the menu honest, and much better
    // than discovering the gap by hovering a blank tooltip.
    const engine = new Set(implementedInRust())
    const helped = new Set(Object.keys(FUNCTION_HELP))
    expect([...helped].filter((n) => !engine.has(n))).toEqual([])
    expect([...engine].filter((n) => !helped.has(n))).toEqual([])
  })
})

/** The engine's `IMPLEMENTED` list, read from the source that defines it. */
function implementedInRust(): string[] {
  const block = /pub const IMPLEMENTED: &\[&str\] = &\[([\s\S]*?)\n\];/.exec(engineFunctions)
  if (!block) throw new Error('could not find IMPLEMENTED in the engine source')
  return [...block[1].matchAll(/"([A-Z0-9.]+)"/g)].map((m) => m[1])
}
