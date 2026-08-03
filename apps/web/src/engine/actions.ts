/**
 * Action constructors — the only vocabulary the UI has for changing anything.
 *
 * These mirror the engine's `Action` enum exactly (serde tags the variant with
 * an `action` field in snake_case). Keeping them in one file means the set of
 * things the UI can do is enumerable by reading a single screen of code, which
 * is also what makes the capture log complete by construction.
 */

export interface Addr {
  row: number
  col: number
}

export interface Range {
  start: Addr
  end: Addr
}

export type PasteMode = 'formulas' | 'values'

export interface SortKey {
  column: number
  ascending: boolean
}

export interface FilterSpec {
  range: Range
  column: number
  allowed: string[]
}

export type Axis = 'row' | 'col'

export type CondOp =
  | 'greater_than'
  | 'less_than'
  | 'greater_or_equal'
  | 'less_or_equal'
  | 'equal'
  | 'not_equal'
  | 'between'
  | 'not_between'

/**
 * What a conditional-formatting rule asks of a cell. Mirrors the engine's
 * `CondTest`, which serde tags with a `test` field.
 */
export type CondTest =
  | { test: 'cell_is'; op: CondOp; operands: string[] }
  | { test: 'text_contains'; needle: string; negate: boolean }
  | { test: 'blank'; negate: boolean }
  | { test: 'duplicate'; unique: boolean }
  | { test: 'formula'; body: string }

export interface CondRule {
  range: Range
  test: CondTest
  /**
   * A *differential* format: only the attributes it sets are applied, and the
   * cell's own formatting shows through the rest.
   */
  format: {
    bold?: boolean
    italic?: boolean
    font_color?: string
    fill_color?: string
    number_format?: string
  }
}

export type HAlign = 'left' | 'center' | 'right'
export type BorderPreset = 'all' | 'outline' | 'none'

/**
 * One presentation attribute. A gesture like "bold and red" sends two
 * patches, so bolding never clears a fill the user set a moment ago, and
 * `{ set: 'fill_color', value: null }` clears rather than being ignored.
 */
export type FormatPatch =
  | { set: 'bold'; value: boolean }
  | { set: 'italic'; value: boolean }
  | { set: 'font_color'; value: string | null }
  | { set: 'fill_color'; value: string | null }
  | { set: 'border'; value: BorderPreset }
  | { set: 'number_format'; value: string | null }
  | { set: 'align'; value: HAlign | null }

export type Action =
  | { action: 'cell_edit'; sheet: string; addr: Addr; input: string }
  | { action: 'cell_clear'; sheet: string; addr: Addr }
  | { action: 'range_clear'; sheet: string; range: Range }
  | {
      action: 'range_paste'
      source_sheet: string
      source: Range
      target_sheet: string
      target: Range
      mode: PasteMode
      cut: boolean
    }
  | { action: 'fill_apply'; sheet: string; source: Range; target: Range }
  | { action: 'row_insert'; sheet: string; at: number; count: number }
  | { action: 'row_delete'; sheet: string; at: number; count: number }
  | { action: 'col_insert'; sheet: string; at: number; count: number }
  | { action: 'col_delete'; sheet: string; at: number; count: number }
  | {
      action: 'sort_apply'
      sheet: string
      range: Range
      keys: SortKey[]
      has_header: boolean
    }
  | { action: 'filter_apply'; sheet: string; spec: FilterSpec }
  | { action: 'filter_clear'; sheet: string }
  | { action: 'merge_apply'; sheet: string; range: Range }
  | { action: 'merge_clear'; sheet: string; range: Range }
  | {
      action: 'format_apply'
      sheet: string
      range: Range
      patches: FormatPatch[]
    }
  | { action: 'format_clear'; sheet: string; range: Range }
  | {
      action: 'find_replace'
      sheet: string
      /** null searches the whole sheet. */
      range: Range | null
      find: string
      replace: string
      match_case: boolean
      whole_cell: boolean
    }
  | { action: 'sheet_add'; name: string }
  | { action: 'sheet_rename'; from: string; to: string }
  | { action: 'sheet_delete'; name: string }
  | {
      action: 'resize'
      sheet: string
      axis: Axis
      at: number
      count: number
      /** Pixels, or null to go back to the default width or height. */
      size: number | null
    }
  | {
      action: 'name_define'
      name: string
      /** An A1 range as xlsx spells it, usually sheet-qualified and absolute. */
      refers_to: string
    }
  | { action: 'name_delete'; name: string }
  | { action: 'freeze_panes'; sheet: string; rows: number; cols: number }
  | { action: 'cond_add'; sheet: string; rule: CondRule }
  | { action: 'cond_clear'; sheet: string; range: Range }
  | { action: 'undo' }
  | { action: 'redo' }

/** Events come back from the engine tagged the same way. */
export interface EngineEvent {
  event: string
  [key: string]: unknown
}

export const addr = (row: number, col: number): Addr => ({ row, col })

export const range = (a: Addr, b: Addr): Range => ({
  start: { row: Math.min(a.row, b.row), col: Math.min(a.col, b.col) },
  end: { row: Math.max(a.row, b.row), col: Math.max(a.col, b.col) },
})

export const singleRange = (a: Addr): Range => ({ start: a, end: a })

/** 0 -> "A", 25 -> "Z", 26 -> "AA". */
export function colLetters(col: number): string {
  let out = ''
  let c = col
  for (;;) {
    out = String.fromCharCode(65 + (c % 26)) + out
    if (c < 26) break
    c = Math.floor(c / 26) - 1
  }
  return out
}

export const a1 = (a: Addr): string => `${colLetters(a.col)}${a.row + 1}`

export const rangeA1 = (r: Range): string =>
  r.start.row === r.end.row && r.start.col === r.end.col
    ? a1(r.start)
    : `${a1(r.start)}:${a1(r.end)}`

export const rangeRows = (r: Range): number => r.end.row - r.start.row + 1
export const rangeCols = (r: Range): number => r.end.col - r.start.col + 1

export const rangeContains = (r: Range, a: Addr): boolean =>
  a.row >= r.start.row &&
  a.row <= r.end.row &&
  a.col >= r.start.col &&
  a.col <= r.end.col
