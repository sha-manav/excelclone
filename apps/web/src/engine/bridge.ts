/**
 * The bridge to the Rust engine.
 *
 * Everything the UI knows about workbook state comes through here, and every
 * change goes out through `apply`. Nothing else in the app is allowed to hold
 * mutable spreadsheet state — that is what keeps the event log a faithful
 * record rather than an approximation of one.
 */

import init, { Gridline } from 'gridline-wasm'
// Vite resolves this to a real asset URL in both dev and build. Without it
// the default loader guesses a path relative to the package inside
// node_modules, which the dev server does not serve.
import wasmUrl from 'gridline-wasm/gridline_wasm_bg.wasm?url'
import type { Action, EngineEvent } from './actions'

export const KIND_EMPTY = 0
export const KIND_NUMBER = 1
export const KIND_TEXT = 2
export const KIND_BOOL = 3
export const KIND_ERROR = 4

export interface Borders {
  top: boolean
  right: boolean
  bottom: boolean
  left: boolean
}

/**
 * A cell's presentation, as the engine resolved it. Absent fields mean the
 * default — the Rust side omits them, so an unformatted cell is `{}`.
 */
export interface CellFormat {
  bold?: boolean
  italic?: boolean
  font_color?: string
  fill_color?: string
  borders?: Borders
  number_format?: string
  align?: 'left' | 'center' | 'right'
}

export const EMPTY_FORMAT: CellFormat = {}

export interface Viewport {
  row0: number
  col0: number
  rows: number
  cols: number
  /** Already run through each cell's number format. */
  values: string[]
  kinds: number[]
  formulas: boolean[]
  /** `rows * cols` indices into `palette`; 0 is the default format. */
  styles: number[]
  palette: CellFormat[]
}

export interface SheetInfo {
  name: string
  /** Extent of the data — where Ctrl+Down stops. */
  used_rows: number
  used_cols: number
  /** Extent of everything that must be drawn, including empty formatted cells. */
  painted_rows: number
  painted_cols: number
  hidden_rows: number[]
  merged: string[]
}

/** One cell a routine would change, as the sandbox reports it. */
export interface CellChange {
  sheet: string
  addr: string
  before: string
  after: string
}

/** A value a routine cannot supply, because the log only has a hash of it. */
export interface RoutineRequirement {
  row_offset: number
  col_offset: number
  kind: string
}

export interface RoutinePreview {
  sheet: string
  anchor: string
  changes: CellChange[]
  /** Cells whose formatting would change, described in words. */
  format_changes: CellChange[]
  /** Actions the engine would refuse, with its reason. */
  errors: string[]
  /** Everything the routine cannot supply, wherever it runs. */
  requires: RoutineRequirement[]
  /**
   * The subset of `requires` whose target cell is empty here. Optional so a
   * preview from an older engine still parses; the panel falls back to
   * `requires`, which over-reports rather than under-reports.
   */
  unmet?: RoutineRequirement[]
}

export interface ImportWarning {
  kind: string
  detail: string
}

export interface ImportOutcome {
  warnings: ImportWarning[]
  sheets: string[]
}

/** Listener for events the engine emitted, used by the capture pipeline. */
export type EventSink = (events: EngineEvent[], action: Action) => void

let initialized: Promise<void> | null = null

async function ensureInit(): Promise<void> {
  if (!initialized) {
    initialized = init({ module_or_path: wasmUrl }).then(() => undefined)
  }
  return initialized
}

export class EngineHandle {
  private inner: Gridline
  private sinks: EventSink[] = []

  private constructor(inner: Gridline) {
    this.inner = inner
    this.inner.setNowMs(Date.now())
  }

  static async create(): Promise<EngineHandle> {
    await ensureInit()
    const handle = new EngineHandle(new Gridline())
    handle.exposeProbe()
    return handle
  }

  /**
   * A read-only window hook for the end-to-end suite.
   *
   * The grid is a canvas, so there is no DOM to assert formatting against.
   * This exposes the same deterministic snapshot the replay tests compare —
   * and nothing else. It is deliberately not a way to mutate anything: the
   * single-mutation-path invariant is what makes the event log trustworthy,
   * and a test-only back door into `apply` would be exactly the side door
   * that invariant exists to forbid. Dev builds only.
   */
  private exposeProbe(): void {
    if (!import.meta.env.DEV) return
    ;(window as unknown as { __gridline__?: unknown }).__gridline__ = {
      stateSnapshot: () => this.stateSnapshot(),
    }
  }

  /** Subscribe to the engine's event stream. Returns an unsubscribe fn. */
  onEvents(sink: EventSink): () => void {
    this.sinks.push(sink)
    return () => {
      this.sinks = this.sinks.filter((s) => s !== sink)
    }
  }

  /**
   * The single mutation path. Throws with the engine's own message when an
   * action is rejected, so the UI surfaces the real reason rather than
   * silently doing nothing.
   */
  apply(action: Action): EngineEvent[] {
    // Refresh the clock so NOW/TODAY advance, while staying an explicit
    // input the log can replay rather than an ambient read.
    this.inner.setNowMs(Date.now())
    const json = this.inner.applyJson(JSON.stringify(action))
    const events = JSON.parse(json) as EngineEvent[]
    for (const sink of this.sinks) sink(events, action)
    return events
  }

  /** Apply several actions as one user-visible step. */
  applyBatch(actions: Action[]): EngineEvent[] {
    this.inner.setNowMs(Date.now())
    const json = this.inner.applyBatchJson(JSON.stringify(actions))
    const events = JSON.parse(json) as EngineEvent[]
    for (const [i, action] of actions.entries()) {
      // Attribute all events to the batch; individual actions still appear
      // in order for the miner.
      if (i === 0) for (const sink of this.sinks) sink(events, action)
    }
    return events
  }

  viewport(
    sheet: string,
    row0: number,
    col0: number,
    rows: number,
    cols: number,
  ): Viewport {
    return this.inner.viewport(sheet, row0, col0, rows, cols) as Viewport
  }

  cellInput(sheet: string, row: number, col: number): string {
    return this.inner.cellInput(sheet, row, col)
  }

  cellValue(sheet: string, row: number, col: number): string {
    return this.inner.cellValue(sheet, row, col)
  }

  sheets(): SheetInfo[] {
    return this.inner.sheets() as SheetInfo[]
  }

  columnValues(sheet: string, rangeA1: string, col: number): string[] {
    return this.inner.columnValues(sheet, rangeA1, col) as string[]
  }

  /** A1 addresses matching a search term, in reading order. */
  findMatches(
    sheet: string,
    find: string,
    matchCase: boolean,
    wholeCell: boolean,
  ): string[] {
    return this.inner.findMatches(sheet, find, matchCase, wholeCell) as string[]
  }

  cellFormat(sheet: string, row: number, col: number): CellFormat {
    return this.inner.cellFormat(sheet, row, col) as CellFormat
  }

  /** What a routine would change here, without changing it. */
  previewRoutine(
    body: unknown,
    sheet: string,
    row: number,
    col: number,
  ): RoutinePreview {
    return JSON.parse(
      this.inner.previewRoutine(JSON.stringify(body), sheet, row, col),
    ) as RoutinePreview
  }

  /**
   * The actions a routine would apply here.
   *
   * Handed back rather than applied inside the engine, so the caller pushes
   * them through the same `applyBatch` every other gesture uses and the
   * capture pipeline sees them without knowing routines exist.
   */
  routineActions(body: unknown, sheet: string, row: number, col: number): Action[] {
    return JSON.parse(
      this.inner.routineActions(JSON.stringify(body), sheet, row, col),
    ) as Action[]
  }

  canUndo(): boolean {
    return this.inner.canUndo()
  }

  canRedo(): boolean {
    return this.inner.canRedo()
  }

  stateSnapshot(): string {
    return this.inner.stateSnapshot()
  }

  importXlsx(bytes: Uint8Array): ImportOutcome {
    return this.inner.importXlsx(bytes) as ImportOutcome
  }

  importCsv(bytes: Uint8Array, sheetName: string): ImportOutcome {
    return this.inner.importCsv(bytes, sheetName) as ImportOutcome
  }

  exportXlsx(): Uint8Array {
    return this.inner.exportXlsx()
  }

  exportCsv(sheet: string): Uint8Array {
    return this.inner.exportCsv(sheet)
  }
}
