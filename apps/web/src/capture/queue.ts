/**
 * The offline queue.
 *
 * Delivery is at-least-once: an envelope stays queued until the server says it
 * has it, and the server deduplicates on `event_id`, so resending is safe and
 * expected. The memory array is the source of truth for what is still owed;
 * IndexedDB is a durability mirror so a reload, a closed laptop or a crashed
 * tab does not lose the backlog.
 *
 * Storage sits behind `QueueStorage` for two reasons. Node has no IndexedDB,
 * and neither does a private-browsing window — the same fallback serves both.
 */

import type { EventEnvelope } from './capture'

export interface SendResult {
  ok: boolean
  /**
   * Whether resending could plausibly succeed. Network failures and 5xx are
   * retryable; a malformed batch is not, and retrying it forever would wedge
   * the queue behind an envelope that will never be accepted.
   */
  retryable?: boolean
  error?: string
}

export interface QueueStorage {
  /** True for a real durable store, false for the memory fallback. */
  readonly durable: boolean
  all(): Promise<EventEnvelope[]>
  put(items: EventEnvelope[]): Promise<void>
  remove(ids: string[]): Promise<void>
  clear(): Promise<void>
}

/** The fallback, and what the tests run against. */
export class MemoryQueueStorage implements QueueStorage {
  readonly durable = false
  private items = new Map<string, EventEnvelope>()

  async all(): Promise<EventEnvelope[]> {
    return [...this.items.values()]
  }

  async put(items: EventEnvelope[]): Promise<void> {
    for (const e of items) this.items.set(e.event_id, e)
  }

  async remove(ids: string[]): Promise<void> {
    for (const id of ids) this.items.delete(id)
  }

  async clear(): Promise<void> {
    this.items.clear()
  }
}

const DB_NAME = 'gridline-capture'
const DB_VERSION = 1
const STORE = 'events'

function request<T>(req: IDBRequest<T>): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    req.onsuccess = () => resolve(req.result)
    req.onerror = () => reject(req.error ?? new Error('indexeddb request failed'))
  })
}

class IndexedDbQueueStorage implements QueueStorage {
  readonly durable = true
  private db: IDBDatabase

  constructor(db: IDBDatabase) {
    this.db = db
  }

  private tx(mode: IDBTransactionMode): IDBObjectStore {
    return this.db.transaction(STORE, mode).objectStore(STORE)
  }

  async all(): Promise<EventEnvelope[]> {
    const rows = await request(this.tx('readonly').getAll())
    return (rows as EventEnvelope[]) ?? []
  }

  async put(items: EventEnvelope[]): Promise<void> {
    const store = this.tx('readwrite')
    await Promise.all(items.map((e) => request(store.put(e))))
  }

  async remove(ids: string[]): Promise<void> {
    const store = this.tx('readwrite')
    await Promise.all(ids.map((id) => request(store.delete(id))))
  }

  async clear(): Promise<void> {
    await request(this.tx('readwrite').clear())
  }
}

/**
 * Open the durable store, falling back to memory whenever IndexedDB is
 * missing, blocked or broken. Private browsing and headless test runners both
 * land here, and in both cases capture keeps working — it just forgets its
 * backlog if the tab dies.
 */
export function openQueueStorage(): Promise<QueueStorage> {
  const idb = (globalThis as { indexedDB?: IDBFactory }).indexedDB
  if (!idb) return Promise.resolve(new MemoryQueueStorage())
  return new Promise<QueueStorage>((resolve) => {
    let settled = false
    const done = (s: QueueStorage) => {
      if (!settled) {
        settled = true
        resolve(s)
      }
    }
    try {
      const open = idb.open(DB_NAME, DB_VERSION)
      open.onupgradeneeded = () => {
        const db = open.result
        if (!db.objectStoreNames.contains(STORE)) {
          db.createObjectStore(STORE, { keyPath: 'event_id' })
        }
      }
      open.onsuccess = () => done(new IndexedDbQueueStorage(open.result))
      open.onerror = () => done(new MemoryQueueStorage())
      open.onblocked = () => done(new MemoryQueueStorage())
    } catch {
      done(new MemoryQueueStorage())
    }
  })
}

export interface QueueState {
  /** Envelopes accepted but not yet acknowledged by the server. */
  pending: number
  /** Consecutive failed delivery attempts. */
  attempt: number
  /** Envelopes the server rejected permanently. */
  discarded: number
  /** Whether the backlog survives a reload. */
  durable: boolean
}

export interface QueueOptions {
  send: (batch: EventEnvelope[]) => Promise<SendResult>
  /** Defaults to memory; pass `await openQueueStorage()` in the browser. */
  storage?: QueueStorage
  batchSize?: number
  baseDelayMs?: number
  maxDelayMs?: number
  /** Injectable so backoff is deterministic under test. */
  random?: () => number
  onChange?: (state: QueueState) => void
}

const QUEUE_DEFAULTS = {
  batchSize: 200,
  baseDelayMs: 1000,
  maxDelayMs: 30_000,
}

export class EventQueue {
  private items: EventEnvelope[] = []
  private ids = new Set<string>()
  private storage: QueueStorage
  private send: (batch: EventEnvelope[]) => Promise<SendResult>
  private batchSize: number
  private baseDelayMs: number
  private maxDelayMs: number
  private random: () => number
  private listeners: ((state: QueueState) => void)[] = []

  private attempt = 0
  private discarded = 0
  private inFlight: Promise<void> | null = null
  private retryTimer: ReturnType<typeof setTimeout> | null = null
  private stopped = false
  /** Resolves once any persisted backlog has been read back in. */
  readonly ready: Promise<void>

  constructor(options: QueueOptions) {
    this.send = options.send
    this.storage = options.storage ?? new MemoryQueueStorage()
    this.batchSize = options.batchSize ?? QUEUE_DEFAULTS.batchSize
    this.baseDelayMs = options.baseDelayMs ?? QUEUE_DEFAULTS.baseDelayMs
    this.maxDelayMs = options.maxDelayMs ?? QUEUE_DEFAULTS.maxDelayMs
    this.random = options.random ?? Math.random
    if (options.onChange) this.listeners.push(options.onChange)
    this.ready = this.hydrate()
  }

  /** Adopt a storage backend discovered asynchronously, keeping the backlog. */
  async useStorage(storage: QueueStorage): Promise<void> {
    this.storage = storage
    const persisted = await storage.all().catch(() => [] as EventEnvelope[])
    this.adopt(persisted)
    if (this.items.length > 0) await storage.put(this.items).catch(() => {})
    this.emit()
  }

  private async hydrate(): Promise<void> {
    const persisted = await this.storage.all().catch(() => [] as EventEnvelope[])
    // Anything already on disk is older than anything enqueued since, so it
    // goes to the front and keeps `seq` order on the wire.
    this.adopt(persisted, true)
    this.emit()
  }

  private adopt(persisted: EventEnvelope[], front = false): void {
    const fresh = persisted.filter((e) => !this.ids.has(e.event_id))
    if (fresh.length === 0) return
    fresh.sort((a, b) => (a.ts_ms - b.ts_ms) || a.seq - b.seq)
    for (const e of fresh) this.ids.add(e.event_id)
    this.items = front ? [...fresh, ...this.items] : [...this.items, ...fresh]
  }

  state(): QueueState {
    return {
      pending: this.items.length,
      attempt: this.attempt,
      discarded: this.discarded,
      durable: this.storage.durable,
    }
  }

  /** Observe pending/backoff/durability changes. Returns an unsubscribe fn. */
  subscribe(listener: (state: QueueState) => void): () => void {
    this.listeners.push(listener)
    return () => {
      this.listeners = this.listeners.filter((l) => l !== listener)
    }
  }

  private emit(): void {
    const state = this.state()
    for (const l of this.listeners) {
      try {
        l(state)
      } catch {
        // Never let an observer break delivery.
      }
    }
  }

  /**
   * Take custody of a batch. Returns immediately: persistence and delivery are
   * both deferred, so the caller (which may be one tick away from a keystroke)
   * is never blocked.
   */
  enqueue(events: EventEnvelope[]): void {
    if (this.stopped || events.length === 0) return
    const fresh = events.filter((e) => !this.ids.has(e.event_id))
    if (fresh.length === 0) return
    for (const e of fresh) this.ids.add(e.event_id)
    this.items.push(...fresh)
    void this.storage.put(fresh).catch(() => {
      // A dead store downgrades durability, not delivery.
    })
    this.emit()
    // Don't stampede a backoff that is already waiting its turn.
    if (this.retryTimer === null) void this.flush()
  }

  /** Attempt delivery now. Resolves when the queue is empty or backing off. */
  flush(): Promise<void> {
    if (this.stopped) return Promise.resolve()
    if (this.inFlight) return this.inFlight
    const run = this.drain().finally(() => {
      this.inFlight = null
    })
    this.inFlight = run
    return run
  }

  private async drain(): Promise<void> {
    while (!this.stopped && this.items.length > 0) {
      const batch = this.items.slice(0, this.batchSize)
      let result: SendResult
      try {
        result = await this.send(batch)
      } catch (e) {
        // The API client is not supposed to throw, but a queue that trusts
        // that and is wrong stops delivering forever.
        result = { ok: false, retryable: true, error: String(e) }
      }
      if (this.stopped) return

      if (result.ok) {
        this.ack(batch)
        this.attempt = 0
        this.emit()
        continue
      }
      if (result.retryable === false) {
        // Permanently rejected: dropping it is the only way past, and the
        // count is surfaced rather than swallowed.
        this.ack(batch)
        this.discarded += batch.length
        this.emit()
        continue
      }
      this.attempt += 1
      this.emit()
      this.scheduleRetry()
      return
    }
  }

  private ack(batch: EventEnvelope[]): void {
    const ids = new Set(batch.map((e) => e.event_id))
    this.items = this.items.filter((e) => !ids.has(e.event_id))
    for (const id of ids) this.ids.delete(id)
    void this.storage.remove([...ids]).catch(() => {})
  }

  /** Exponential backoff with jitter, capped so a long outage still recovers. */
  nextDelayMs(attempt: number): number {
    const exp = Math.min(this.maxDelayMs, this.baseDelayMs * 2 ** Math.max(0, attempt - 1))
    // Half fixed, half jittered: spreads a reconnecting fleet without letting
    // any one client retry instantly forever.
    return Math.round(exp / 2 + this.random() * (exp / 2))
  }

  private scheduleRetry(): void {
    if (this.retryTimer !== null || this.stopped) return
    const delay = this.nextDelayMs(this.attempt)
    this.retryTimer = setTimeout(() => {
      this.retryTimer = null
      void this.flush()
    }, delay)
  }

  /** Stop retrying. The backlog stays in storage for the next session. */
  stop(): void {
    this.stopped = true
    if (this.retryTimer !== null) {
      clearTimeout(this.retryTimer)
      this.retryTimer = null
    }
  }

  /** Test/debug view of what is still owed. */
  peek(): EventEnvelope[] {
    return [...this.items]
  }
}
