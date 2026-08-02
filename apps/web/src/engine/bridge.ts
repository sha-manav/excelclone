/**
 * The bridge to the Rust engine.
 *
 * Everything the UI knows about workbook state comes through here, and every
 * change goes out through `apply`. Nothing else in the app is allowed to hold
 * mutable spreadsheet state — that is what keeps the event log a faithful
 * record rather than an approximation of one.
 */

import init, { Gridline } from 'gridline-wasm'
import type { Action, EngineEvent } from './actions'

export const KIND_EMPTY = 0
export const KIND_NUMBER = 1
export const KIND_TEXT = 2
export const KIND_BOOL = 3
export const KIND_ERROR = 4

export interface Viewport {
  row0: number
  col0: number
  rows: number
  cols: number
  values: string[]
  kinds: number[]
  formulas: boolean[]
}

export interface SheetInfo {
  name: string
  used_rows: number
  used_cols: number
  hidden_rows: number[]
  merged: string[]
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
  if (!initialized) initialized = init().then(() => undefined)
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
    return new EngineHandle(new Gridline())
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
