/**
 * Pointing mode: picking a reference off the grid while a formula is open.
 *
 * In Excel you type `=AVERAGE(`, drag across the numbers, and the range
 * appears inside the parentheses. The gesture is so ordinary that a
 * spreadsheet without it does not read as missing a feature — it reads as
 * broken, because the drag does something *else* instead. Here it used to
 * commit the half-written formula, which then failed to parse.
 *
 * The whole decision is "may a reference go in at the caret, and is one
 * already there to replace?", and it is answered from the text alone. Keeping
 * it out of the component means it can be tested as the table of cases it is,
 * rather than through a browser.
 */

/** Where the next pointed reference goes, and what it displaces. */
export interface PointingSlot {
  /** First character of the text the reference replaces. */
  start: number
  /** One past the last, so `start === end` is a pure insertion. */
  end: number
}

/**
 * A reference at the end of a string: an optional sheet qualifier, a cell, and
 * an optional second cell making it a range. Anchored at the end because the
 * only reference that matters is the one the caret is sitting after.
 *
 * The sheet qualifier accepts either a bare name or a quoted one, since
 * `'Q1 Actuals'!A1` is what the app itself writes for a sheet with a space in
 * it. `$` is allowed anywhere Excel allows it so that pointing at a cell twice
 * does not leave the first, absolute, copy behind.
 */
const TRAILING_REF =
  /(?:(?:'[^']*'|[A-Za-z_][A-Za-z0-9_.]*)!)?\$?[A-Za-z]{1,3}\$?[0-9]{1,7}(?::\$?[A-Za-z]{1,3}\$?[0-9]{1,7})?$/

/**
 * Characters after which Excel expects an operand, and therefore accepts a
 * pointed reference. `=` is here so `=` alone is pointable; `(` and `,` are
 * the argument positions; the rest are operators.
 */
const OPERAND_EXPECTED = new Set([
  '=',
  '(',
  ',',
  '+',
  '-',
  '*',
  '/',
  '^',
  '&',
  '<',
  '>',
  ':',
  '%',
  ';',
])

/** True when the caret sits inside an unterminated string literal. */
function insideString(text: string): boolean {
  let open = false
  for (let i = 0; i < text.length; i += 1) {
    if (text[i] !== '"') continue
    if (open && text[i + 1] === '"') {
      i += 1 // an escaped quote, still inside
      continue
    }
    open = !open
  }
  return open
}

/**
 * Where a reference pointed at right now would land, or `null` if this caret
 * position is not expecting one.
 *
 * `null` is what lets clicking away from a *finished* formula still commit and
 * move, which is the other half of feeling like Excel. Only a formula that is
 * visibly mid-expression captures the click.
 */
export function pointingSlot(value: string, caret: number): PointingSlot | null {
  if (!value.startsWith('=')) return null
  const at = Math.max(0, Math.min(caret, value.length))
  const before = value.slice(0, at)
  // A reference typed into the middle of a quoted string is text, not a
  // reference, and replacing part of somebody's message would be worse than
  // doing nothing.
  if (insideString(before)) return null

  const ref = TRAILING_REF.exec(before)
  if (ref) {
    // Only a *reference* is replaceable. `SUM(` matches nothing here, but
    // `LOG10` ends in something shaped like a reference and is a function
    // name, so require that what precedes it expects an operand too.
    const start = at - ref[0].length
    if (expectsOperand(before.slice(0, start))) return { start, end: at }
    return null
  }

  return expectsOperand(before) ? { start: at, end: at } : null
}

/** True when `text` ends somewhere an operand may begin. */
function expectsOperand(text: string): boolean {
  const trimmed = text.trimEnd()
  if (trimmed.length === 0) return false
  return OPERAND_EXPECTED.has(trimmed[trimmed.length - 1])
}

/** The value and caret after pointing `ref` into `slot`. */
export function applyPointing(
  value: string,
  slot: PointingSlot,
  ref: string,
): { value: string; caret: number } {
  return {
    value: value.slice(0, slot.start) + ref + value.slice(slot.end),
    caret: slot.start + ref.length,
  }
}
