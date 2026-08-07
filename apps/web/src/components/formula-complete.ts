/**
 * Function-name completion for a formula being typed.
 *
 * Deciding *what* to offer is a question about a string and a caret, so it
 * lives here rather than in the component: the interesting cases — a name
 * inside a nested call, a name that is really a cell reference, a caret that
 * has moved back into the middle of the text — are much easier to state as a
 * table than to click through.
 */

import { FUNCTION_HELP } from './function-help'

/** How many names the menu will show at once. */
export const MAX_SUGGESTIONS = 8

/**
 * How many letters before the menu appears.
 *
 * Excel opens on the first one. Here it waits for two, because a single letter
 * after `=` is far more often the start of a reference than of a function
 * name, and the menu drops over exactly the cells the user is about to point
 * at — `=A` would put a list of ABS, AND and AVERAGE on top of column A every
 * time somebody started `=A1+A2`. Two letters costs almost nothing and keeps
 * the two features out of each other's way.
 */
export const MIN_PREFIX = 2

export interface Completion {
  /** Uppercased prefix the user has typed so far. */
  prefix: string
  /** Where in the value the prefix starts, so accepting can replace it. */
  start: number
  /** Matching function names, best first. */
  names: string[]
}

/**
 * Only a bare run of letters immediately before the caret can be a function
 * name being typed. Digits are excluded deliberately: `A1` is a reference and
 * `LOG10` is only a function once the `(` arrives, and offering completions
 * over a half-typed address would put a menu in front of the grid every time
 * somebody typed `=A`.
 */
const TRAILING_WORD = /[A-Za-z][A-Za-z.]*$/

/**
 * What to offer at `caret`, or `null` when nothing should be offered.
 *
 * `names` is ordered by how likely the user means it: exact prefix matches on
 * a short name first, because `SUM` is asked for far more often than
 * `SUMPRODUCT` and a menu that puts the rarer one first is a menu people
 * learn to dismiss.
 */
export function completionAt(
  value: string,
  caret: number,
  available: readonly string[],
): Completion | null {
  if (!value.startsWith('=')) return null
  const at = Math.max(0, Math.min(caret, value.length))
  const before = value.slice(0, at)
  const word = TRAILING_WORD.exec(before)
  if (!word || word[0].length < MIN_PREFIX) return null

  const start = at - word[0].length
  // A name that follows something other than an operator or a bracket is not
  // a function call — `=A1andB2` is nonsense, and completing it is worse.
  const preceding = before.slice(0, start).trimEnd()
  const last = preceding[preceding.length - 1]
  if (last !== undefined && !'=(,+-*/^&<>:% ;'.includes(last)) return null

  const prefix = word[0].toUpperCase()
  const names = available
    .filter((n) => n.startsWith(prefix))
    // Never offer only the thing already typed in full: a one-entry menu
    // showing exactly what is on screen is noise the user has to dismiss.
    .filter((n) => n !== prefix || available.some((o) => o !== n && o.startsWith(prefix)))
    .sort((a, b) => a.length - b.length || a.localeCompare(b))
    .slice(0, MAX_SUGGESTIONS)

  return names.length > 0 ? { prefix, start, names } : null
}

/**
 * The value and caret after accepting `name`.
 *
 * The `(` comes with it, because a function name without one is not something
 * anybody wanted, and the caret lands inside — which is exactly where pointing
 * mode then takes over.
 */
export function acceptCompletion(
  value: string,
  completion: Completion,
  name: string,
): { value: string; caret: number } {
  const head = value.slice(0, completion.start)
  const tail = value.slice(completion.start + completion.prefix.length)
  // Do not double the bracket when the user typed one and then went back.
  const open = tail.startsWith('(') ? '' : '('
  return {
    value: `${head}${name}${open}${tail}`,
    caret: completion.start + name.length + 1,
  }
}

/** A function with no help entry still completes; it just completes bare. */
export function describe(name: string): string {
  return FUNCTION_HELP[name]?.about ?? ''
}
