/**
 * The client capture pipeline.
 *
 * One rule governs the shape of this file: capture must never be something a
 * user can feel. The engine calls our sink synchronously inside `apply`, on
 * the same tick as a keystroke, so the sink does the smallest amount of work
 * that is still correct — redact through wasm, stamp an envelope, push it into
 * a fixed-size ring — and hands everything else (JSON, IndexedDB, network) to
 * a timer. Nothing in here throws into the caller; a broken capture pipeline
 * must degrade to "no events", never to "no spreadsheet".
 *
 * Redaction itself is deliberately not implemented here. `describeAction` is
 * the same Rust the engine tests cover, compiled to wasm. A second
 * implementation of a privacy guarantee is a second chance to get it wrong.
 *
 * The envelope, the vocabulary and the delivery cadence are specified in
 * `docs/EVENTS.md`; the promises they keep are in `docs/PRIVACY.md`.
 */

import {
  actionVocabulary as wasmActionVocabulary,
  describeAction as wasmDescribeAction,
  redactLabel as wasmRedactLabel,
} from 'gridline-wasm'
import type { Action } from '../engine/actions'

/** Envelope version. Bumped only for breaking changes. */
export const SCHEMA_VERSION = 1

/** Version of the web app that produced the event. */
export const CLIENT_VERSION = '0.1.0'

/**
 * Version of the consent text the user actually read. Bump this whenever the
 * wording in `docs/PRIVACY.md` or the modal changes materially, so an old
 * acceptance can be told apart from a new one.
 */
export const CONSENT_TEXT_VERSION = '1'

export type PrivacyMode = 'full' | 'structural' | 'off'

/** What the toolbar chip shows. */
export type CaptureState = 'capturing' | 'paused' | 'off'

export interface EventContext {
  sheet: string
  selection: string
  privacy_mode: PrivacyMode
}

/** The wire format. Field order matches `docs/EVENTS.md`. */
export interface EventEnvelope {
  schema_version: number
  event_id: string
  session_id: string
  actor_id: string
  workbook_id: string
  seq: number
  ts_ms: number
  action: string
  payload: Record<string, unknown>
  context: EventContext
  client_version: string
}

/** Where flushed batches go. `EventQueue` in `queue.ts` implements this. */
export interface CaptureSink {
  enqueue(events: EventEnvelope[]): void
  flush(): Promise<void>
}

/**
 * The slice of `EngineHandle` capture needs. Declared structurally so tests
 * can attach a three-line fake instead of a real wasm engine.
 */
export interface AttachableEngine {
  onEvents(sink: (events: unknown[], action: Action) => void): () => void
}

/** `describeAction`'s signature, injectable so tests can run without wasm. */
export type DescribeFn = (actionJson: string, mode: string, salt: string) => string

/** `redactLabel`'s signature, injectable for the same reason. */
export type RedactLabelFn = (text: string, mode: string, salt: string) => string

export interface CaptureOptions {
  actorId: string
  workbookId: string
  /**
   * Per-workbook hashing salt. Real deployments take this from the server,
   * which is the only place it is meant to live; the client holds it just long
   * enough to redact with it.
   */
  salt: string
  /** Where the user is, read lazily so the caller never has to push updates. */
  context: () => { sheet: string; selection: string }
  sink: CaptureSink
  mode?: PrivacyMode
  describe?: DescribeFn
  redactLabel?: RedactLabelFn
  clientVersion?: string
  /** Hard cap on buffered events before the oldest are dropped. */
  capacity?: number
  /** Flush at this many buffered events. */
  batchSize?: number
  /** Flush at least this often. */
  flushIntervalMs?: number
  /** A gap this long starts a new session. */
  sessionIdleMs?: number
  /** Ceiling on `nav.select` events per second. */
  navPerSecond?: number
}

export interface CaptureStats {
  state: CaptureState
  mode: PrivacyMode
  /** Events built but not yet handed to the queue. */
  buffered: number
  /** Events the ring buffer discarded because delivery fell too far behind. */
  dropped: number
  sessionId: string
  seq: number
}

const DEFAULTS = {
  capacity: 5000,
  batchSize: 200,
  flushIntervalMs: 5000,
  sessionIdleMs: 10 * 60 * 1000,
  navPerSecond: 2,
}

// --- ULID ------------------------------------------------------------------
//
// 48 bits of millisecond timestamp then 80 bits of randomness, Crockford
// base32, so ids sort by creation time as plain strings. Within a millisecond
// the random field is incremented rather than redrawn, which keeps a burst of
// events from one action strictly ordered. A dependency for 30 lines of this
// would be a dependency to audit.

const ULID_ALPHABET = '0123456789ABCDEFGHJKMNPQRSTVWXYZ'
const ULID_TIME_LEN = 10
const ULID_RAND_LEN = 16

let ulidLastTime = -1
const ulidLastRandom: number[] = new Array<number>(ULID_RAND_LEN).fill(0)

function randomBase32(out: number[]): void {
  const c = (globalThis as { crypto?: Crypto }).crypto
  if (c && typeof c.getRandomValues === 'function') {
    const buf = new Uint8Array(out.length)
    c.getRandomValues(buf)
    // 256 is a multiple of 32, so the modulo stays uniform.
    for (let i = 0; i < out.length; i++) out[i] = buf[i] % 32
    return
  }
  for (let i = 0; i < out.length; i++) out[i] = Math.floor(Math.random() * 32)
}

function encodeUlidTime(ms: number): string {
  let out = ''
  let t = Math.max(0, Math.floor(ms))
  for (let i = 0; i < ULID_TIME_LEN; i++) {
    out = ULID_ALPHABET[t % 32] + out
    t = Math.floor(t / 32)
  }
  return out
}

/** A monotonic ULID. Unique and strictly increasing, even within one tick. */
export function ulid(nowMs: number = Date.now()): string {
  if (nowMs === ulidLastTime) {
    let i = ULID_RAND_LEN - 1
    for (; i >= 0; i--) {
      if (ulidLastRandom[i] < 31) {
        ulidLastRandom[i] += 1
        break
      }
      ulidLastRandom[i] = 0
    }
    if (i < 0) {
      // 80 bits exhausted inside one millisecond. Borrow from the next one
      // rather than emit a duplicate.
      ulidLastTime += 1
      randomBase32(ulidLastRandom)
    }
  } else {
    // A clock that jumps backwards must not make ids go backwards either.
    ulidLastTime = nowMs > ulidLastTime ? nowMs : ulidLastTime + 1
    randomBase32(ulidLastRandom)
  }
  let out = encodeUlidTime(ulidLastTime)
  for (let i = 0; i < ULID_RAND_LEN; i++) out += ULID_ALPHABET[ulidLastRandom[i]]
  return out
}

// --- Ring buffer -----------------------------------------------------------

/**
 * Fixed-capacity FIFO. When it is full the oldest event is discarded and
 * counted: losing the tail of a long offline session is survivable, growing
 * without bound in a tab someone left open for a week is not. The count is
 * surfaced in the chip's tooltip and on the transparency page, because a
 * silent loss is indistinguishable from a bug.
 */
export class RingBuffer<T> {
  private items: (T | undefined)[]
  private start = 0
  private count = 0
  private droppedCount = 0
  readonly capacity: number

  constructor(capacity: number) {
    this.capacity = Math.max(1, capacity)
    this.items = new Array<T | undefined>(this.capacity)
  }

  get size(): number {
    return this.count
  }

  get dropped(): number {
    return this.droppedCount
  }

  push(item: T): void {
    if (this.count === this.capacity) {
      this.items[this.start] = undefined
      this.start = (this.start + 1) % this.capacity
      this.count -= 1
      this.droppedCount += 1
    }
    this.items[(this.start + this.count) % this.capacity] = item
    this.count += 1
  }

  /** Remove and return up to `n` items, oldest first. */
  take(n: number): T[] {
    const out: T[] = []
    const limit = Math.min(n, this.count)
    for (let i = 0; i < limit; i++) {
      const idx = (this.start + i) % this.capacity
      out.push(this.items[idx] as T)
      this.items[idx] = undefined
    }
    this.start = (this.start + limit) % this.capacity
    this.count -= limit
    return out
  }
}

// --- Controller ------------------------------------------------------------

/** The documented vocabulary, straight from the engine. Never a copy. */
export function captureVocabulary(): string[] {
  try {
    return wasmActionVocabulary()
  } catch {
    // The transparency page must render even if wasm has not finished loading.
    return []
  }
}

export class CaptureController {
  private opts: Required<
    Pick<
      CaptureOptions,
      'capacity' | 'batchSize' | 'flushIntervalMs' | 'sessionIdleMs' | 'navPerSecond'
    >
  >
  private actorId: string
  private workbookId: string
  private salt: string
  private clientVersion: string
  private readContext: () => { sheet: string; selection: string }
  private sink: CaptureSink
  private describe: DescribeFn
  private redactLabel: RedactLabelFn

  private mode: PrivacyMode
  private paused = false

  private buffer: RingBuffer<EventEnvelope>
  private sessionId: string
  private seq = 0
  private lastActivityMs = 0

  private timer: ReturnType<typeof setInterval> | null = null
  private soon: ReturnType<typeof setTimeout> | null = null
  private detachEngine: (() => void) | null = null
  private listeners: ((s: CaptureStats) => void)[] = []

  // nav.select sampling state.
  private navEmits: number[] = []
  private navPending: string | null = null
  private navTimer: ReturnType<typeof setTimeout> | null = null

  constructor(options: CaptureOptions) {
    this.actorId = options.actorId
    this.workbookId = options.workbookId
    this.salt = options.salt
    this.clientVersion = options.clientVersion ?? CLIENT_VERSION
    this.readContext = options.context
    this.sink = options.sink
    this.describe = options.describe ?? wasmDescribeAction
    this.redactLabel = options.redactLabel ?? wasmRedactLabel
    this.mode = options.mode ?? 'off'
    this.opts = {
      capacity: options.capacity ?? DEFAULTS.capacity,
      batchSize: options.batchSize ?? DEFAULTS.batchSize,
      flushIntervalMs: options.flushIntervalMs ?? DEFAULTS.flushIntervalMs,
      sessionIdleMs: options.sessionIdleMs ?? DEFAULTS.sessionIdleMs,
      navPerSecond: options.navPerSecond ?? DEFAULTS.navPerSecond,
    }
    this.buffer = new RingBuffer<EventEnvelope>(this.opts.capacity)
    this.sessionId = ulid()
    this.lastActivityMs = Date.now()
    this.startTimer()
  }

  // -- state ---------------------------------------------------------------

  state(): CaptureState {
    if (this.mode === 'off') return 'off'
    return this.paused ? 'paused' : 'capturing'
  }

  currentMode(): PrivacyMode {
    return this.mode
  }

  stats(): CaptureStats {
    return {
      state: this.state(),
      mode: this.mode,
      buffered: this.buffer.size,
      dropped: this.buffer.dropped,
      sessionId: this.sessionId,
      seq: this.seq,
    }
  }

  /** Subscribe to state and counter changes. Returns an unsubscribe fn. */
  subscribe(listener: (s: CaptureStats) => void): () => void {
    this.listeners.push(listener)
    return () => {
      this.listeners = this.listeners.filter((l) => l !== listener)
    }
  }

  private notify(): void {
    const s = this.stats()
    for (const l of this.listeners) {
      try {
        l(s)
      } catch {
        // A listener that throws is the listener's problem, not capture's.
      }
    }
  }

  setSalt(salt: string): void {
    this.salt = salt
  }

  /**
   * Change the privacy mode, recording the consent transition it represents.
   *
   * Revocation is recorded *before* the mode changes, because an event emitted
   * after capture stopped would be an event emitted without consent.
   */
  setMode(mode: PrivacyMode, consentTextVersion: string = CONSENT_TEXT_VERSION): void {
    if (mode === this.mode) return
    const wasCapturing = this.mode !== 'off'
    if (mode === 'off') {
      this.record('consent.revoked', {})
      this.mode = 'off'
      // Nothing may sit in a buffer waiting once consent is gone: push what
      // was legitimately captured, then stop.
      void this.flushNow()
      this.notify()
      return
    }
    this.mode = mode
    // Resuming capture by picking a mode also clears a stale pause, so the
    // chip cannot read "paused" when the user just said yes.
    if (!wasCapturing) this.paused = false
    this.record('consent.granted', {
      mode,
      consent_text_version: consentTextVersion,
    })
    this.notify()
  }

  pause(): void {
    if (this.state() !== 'capturing') return
    // Recorded while capture is still on, so the log explains its own gap.
    this.record('capture.pause', {})
    this.paused = true
    void this.flushNow()
    this.notify()
  }

  resume(): void {
    if (this.mode === 'off' || !this.paused) return
    this.paused = false
    this.record('capture.resume', {})
    this.notify()
  }

  /** Pause/resume in one click, for the toolbar chip. A no-op when off. */
  toggle(): void {
    if (this.mode === 'off') return
    if (this.paused) this.resume()
    else this.pause()
  }

  // -- attachment ----------------------------------------------------------

  /** Subscribe to the engine's applied-action stream. */
  attach(engine: AttachableEngine): () => void {
    this.detach()
    this.detachEngine = engine.onEvents((_events, action) => {
      this.onAction(action)
    })
    return () => this.detach()
  }

  detach(): void {
    if (this.detachEngine) {
      this.detachEngine()
      this.detachEngine = null
    }
  }

  /** Stop timers and hand everything buffered to the queue. */
  async shutdown(): Promise<void> {
    this.detach()
    if (this.timer !== null) {
      clearInterval(this.timer)
      this.timer = null
    }
    if (this.soon !== null) {
      clearTimeout(this.soon)
      this.soon = null
    }
    if (this.navTimer !== null) {
      clearTimeout(this.navTimer)
      this.navTimer = null
    }
    await this.flushNow()
  }

  // -- recording -----------------------------------------------------------

  /**
   * The engine's sink. Everything here is O(1) and allocation-light; the only
   * non-trivial call is the wasm redaction, which is a few microseconds of
   * Rust. Any failure is swallowed so a capture bug can never break `apply`.
   */
  private onAction(action: Action): void {
    if (this.state() !== 'capturing') return
    try {
      const described = this.describe(JSON.stringify(action), this.mode, this.salt) as string
      const parsed = JSON.parse(described) as {
        action: string
        payload: Record<string, unknown>
      }
      this.record(parsed.action, parsed.payload)
    } catch {
      // A vocabulary gap or a wasm hiccup loses one event, not the session.
    }
  }

  /**
   * Which workbook this session is about. The routines panel needs it to ask
   * the server for suggestions, and the id is derived here already.
   */
  currentWorkbookId(): string {
    return this.workbookId
  }

  /**
   * Record a shell-level action — the ones the engine cannot know about, like
   * `file.open` or `routine.run`. The payload must already be redaction-safe.
   */
  recordShellAction(name: string, payload: Record<string, unknown> = {}): void {
    if (this.state() !== 'capturing') return
    this.record(name, payload)
  }

  /**
   * Note a selection change. Bursts are coalesced to at most `navPerSecond`
   * events, keeping the most recent selection, because dragging a cursor is
   * noise and the log should carry intent.
   */
  noteSelection(selection: string): void {
    if (this.state() !== 'capturing') return
    this.navPending = selection
    this.drainNav()
  }

  private drainNav(): void {
    const now = Date.now()
    const windowStart = now - 1000
    this.navEmits = this.navEmits.filter((t) => t > windowStart)
    if (this.navPending === null) return

    if (this.navEmits.length < this.opts.navPerSecond) {
      const selection = this.navPending
      this.navPending = null
      this.navEmits.push(now)
      this.record('nav.select', { selection })
      return
    }
    if (this.navTimer !== null) return
    // Wake up exactly when the oldest emission leaves the window.
    const wait = Math.max(1, this.navEmits[0] + 1000 - now + 1)
    this.navTimer = setTimeout(() => {
      this.navTimer = null
      if (this.state() !== 'capturing') {
        this.navPending = null
        return
      }
      this.drainNav()
    }, wait)
  }

  /** Build one envelope and buffer it. Never performs I/O. */
  private record(action: string, payload: Record<string, unknown>): void {
    if (this.mode === 'off') return
    const now = Date.now()
    // A long gap client-side starts a new session, matching the server's own
    // 10-minute rule so the two agree about where a session ended.
    if (now - this.lastActivityMs > this.opts.sessionIdleMs) {
      this.sessionId = ulid(now)
      this.seq = 0
    }
    this.lastActivityMs = now

    let ctx: { sheet: string; selection: string }
    try {
      ctx = this.readContext()
    } catch {
      ctx = { sheet: '', selection: '' }
    }

    this.seq += 1
    this.buffer.push({
      schema_version: SCHEMA_VERSION,
      event_id: ulid(now),
      session_id: this.sessionId,
      actor_id: this.actorId,
      workbook_id: this.workbookId,
      seq: this.seq,
      ts_ms: now,
      action,
      payload,
      context: {
        // The sheet name rides on every event, so it is redacted exactly as
        // payload sheet names are. Leaving it clear would leak the name and
        // expose a matched hash/plaintext pair for this workbook's salt.
        sheet: this.redactSheet(ctx.sheet),
        selection: ctx.selection,
        privacy_mode: this.mode,
      },
      client_version: this.clientVersion,
    })

    if (this.buffer.size >= this.opts.batchSize) this.scheduleImmediateFlush()
  }

  /**
   * Adopt the actor id the server authenticated us as.
   *
   * The client mints a local id so capture works before the first response,
   * but the server rejects any envelope whose `actor_id` is not the
   * authenticated one — that check is what stops a client forging another
   * user's log, so the client has to converge on the server's answer rather
   * than the other way round.
   */
  setActorId(actorId: string): void {
    if (!actorId || actorId === this.actorId) return
    this.actorId = actorId
  }

  /** The id envelopes are currently stamped with, for tests and diagnostics. */
  currentActorId(): string {
    return this.actorId
  }

  /** Redact the context sheet name, failing closed if the redactor throws. */
  private redactSheet(name: string): string {
    if (name === '' || this.mode === 'full') return name
    try {
      return this.redactLabel(name, this.mode, this.salt)
    } catch {
      // Never transmit the raw name because redaction failed.
      return ''
    }
  }

  // -- delivery ------------------------------------------------------------

  private startTimer(): void {
    if (this.timer !== null) return
    this.timer = setInterval(() => {
      void this.flushNow()
    }, this.opts.flushIntervalMs)
  }

  /**
   * Reaching the batch size flushes on the next macrotask rather than inline:
   * the 200th event is usually produced inside a keystroke handler, and that
   * is not where a serialise-and-store belongs.
   */
  private scheduleImmediateFlush(): void {
    if (this.soon !== null) return
    this.soon = setTimeout(() => {
      this.soon = null
      void this.flushNow()
    }, 0)
  }

  /**
   * Hand everything buffered to the queue. Resolves when the queue has taken
   * it; queue failures are the queue's business and never surface here.
   */
  async flushNow(): Promise<void> {
    const pending = this.buffer.take(this.buffer.size)
    if (pending.length > 0) {
      // Stamp identity on the way out, not on the way in. The server's
      // actor id arrives asynchronously, and anything buffered before it
      // lands would otherwise carry the client's placeholder and be
      // rejected as a mismatched actor — permanently, since a rejected
      // event is retried unchanged.
      for (const envelope of pending) envelope.actor_id = this.actorId
      try {
        this.sink.enqueue(pending)
      } catch {
        // The queue is the durability layer; if even enqueueing fails there is
        // nowhere left to put these.
      }
      this.notify()
    }
    try {
      await this.sink.flush()
    } catch {
      // Delivery failures retry on the queue's own schedule.
    }
  }
}
