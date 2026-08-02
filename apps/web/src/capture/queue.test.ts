/**
 * Offline queue tests.
 *
 * The property that matters is that a failure never loses an envelope. Storage
 * is injected, so these run in node with no IndexedDB at all — which is also
 * the private-browsing case the real client has to survive.
 */

import { afterEach, beforeEach, describe as suite, expect, test, vi } from 'vitest'
import {
  EventQueue,
  MemoryQueueStorage,
  openQueueStorage,
  type SendResult,
} from './queue'
import type { EventEnvelope } from './capture'

function envelope(seq: number): EventEnvelope {
  return {
    schema_version: 1,
    event_id: `EVT${String(seq).padStart(23, '0')}`,
    session_id: 'SESSION',
    actor_id: 'u_test',
    workbook_id: 'wb_test',
    seq,
    ts_ms: 1_767_225_600_000 + seq,
    action: 'cell.edit',
    payload: { addr: 'A1' },
    context: { sheet: 'Sheet1', selection: 'A1', privacy_mode: 'structural' },
    client_version: '0.1.0',
  }
}

function envelopes(n: number, from = 1): EventEnvelope[] {
  return Array.from({ length: n }, (_, i) => envelope(from + i))
}

/** A server that can be told to fail, recording everything it was offered. */
function makeServer() {
  const attempts: EventEnvelope[][] = []
  const accepted: EventEnvelope[] = []
  let failures = 0
  let result: SendResult = { ok: false, retryable: true }
  return {
    attempts,
    accepted,
    failNext(n: number, r: SendResult = { ok: false, retryable: true }) {
      failures = n
      result = r
    },
    send: async (batch: EventEnvelope[]): Promise<SendResult> => {
      attempts.push(batch)
      if (failures > 0) {
        failures -= 1
        return result
      }
      accepted.push(...batch)
      return { ok: true }
    },
  }
}

beforeEach(() => {
  vi.useFakeTimers()
})

afterEach(() => {
  vi.useRealTimers()
})

suite('delivery', () => {
  test('delivers a batch and empties the queue', async () => {
    const server = makeServer()
    const queue = new EventQueue({ send: server.send, storage: new MemoryQueueStorage() })
    await queue.ready

    queue.enqueue(envelopes(3))
    await vi.advanceTimersByTimeAsync(0)

    expect(server.accepted.map((e) => e.seq)).toEqual([1, 2, 3])
    expect(queue.state().pending).toBe(0)
  })

  test('splits oversized backlogs into batches', async () => {
    const server = makeServer()
    const queue = new EventQueue({
      send: server.send,
      storage: new MemoryQueueStorage(),
      batchSize: 200,
    })
    await queue.ready

    queue.enqueue(envelopes(450))
    await vi.advanceTimersByTimeAsync(0)

    expect(server.attempts.map((b) => b.length)).toEqual([200, 200, 50])
    expect(queue.state().pending).toBe(0)
  })

  test('deduplicates on event_id, so a re-enqueue is harmless', async () => {
    const server = makeServer()
    const queue = new EventQueue({ send: server.send, storage: new MemoryQueueStorage() })
    await queue.ready
    server.failNext(1)

    queue.enqueue(envelopes(2))
    await vi.advanceTimersByTimeAsync(0)
    queue.enqueue(envelopes(2)) // the same two envelopes again
    expect(queue.state().pending).toBe(2)
  })
})

suite('retry with backoff', () => {
  test('keeps every event across a simulated outage', async () => {
    const server = makeServer()
    const queue = new EventQueue({
      send: server.send,
      storage: new MemoryQueueStorage(),
      baseDelayMs: 1_000,
      random: () => 1, // no jitter, so the schedule is exact
    })
    await queue.ready
    server.failNext(3)

    queue.enqueue(envelopes(5))
    await vi.advanceTimersByTimeAsync(0)
    expect(server.attempts).toHaveLength(1)
    expect(queue.state().pending).toBe(5)
    expect(queue.state().attempt).toBe(1)

    // Nothing happens early...
    await vi.advanceTimersByTimeAsync(900)
    expect(server.attempts).toHaveLength(1)

    // ...then the first retry, at one second.
    await vi.advanceTimersByTimeAsync(200)
    expect(server.attempts).toHaveLength(2)
    expect(queue.state().attempt).toBe(2)

    // The second retry waits twice as long.
    await vi.advanceTimersByTimeAsync(1_800)
    expect(server.attempts).toHaveLength(2)
    await vi.advanceTimersByTimeAsync(200)
    expect(server.attempts).toHaveLength(3)

    // The fourth attempt succeeds and nothing was lost or reordered.
    await vi.advanceTimersByTimeAsync(4_100)
    expect(server.accepted.map((e) => e.seq)).toEqual([1, 2, 3, 4, 5])
    expect(queue.state().pending).toBe(0)
    expect(queue.state().attempt).toBe(0)
  })

  test('backoff grows exponentially, is jittered, and is capped', () => {
    const lo = new EventQueue({
      send: async () => ({ ok: true }),
      storage: new MemoryQueueStorage(),
      baseDelayMs: 1_000,
      maxDelayMs: 30_000,
      random: () => 0,
    })
    const hi = new EventQueue({
      send: async () => ({ ok: true }),
      storage: new MemoryQueueStorage(),
      baseDelayMs: 1_000,
      maxDelayMs: 30_000,
      random: () => 1,
    })

    expect(hi.nextDelayMs(1)).toBe(1_000)
    expect(hi.nextDelayMs(2)).toBe(2_000)
    expect(hi.nextDelayMs(3)).toBe(4_000)
    // Half the delay is jitter, so two clients never retry in lockstep.
    expect(lo.nextDelayMs(3)).toBe(2_000)
    // And the whole thing is capped.
    for (const attempt of [6, 10, 40, 1_000]) {
      expect(hi.nextDelayMs(attempt)).toBeLessThanOrEqual(30_000)
      expect(lo.nextDelayMs(attempt)).toBeGreaterThan(0)
    }
    expect(hi.nextDelayMs(1_000)).toBe(30_000)
  })

  test('a send that throws is treated as a retryable failure, not a lost batch', async () => {
    let calls = 0
    const queue = new EventQueue({
      send: async () => {
        calls += 1
        if (calls === 1) throw new Error('fetch blew up')
        return { ok: true }
      },
      storage: new MemoryQueueStorage(),
      baseDelayMs: 1_000,
      random: () => 1,
    })
    await queue.ready

    queue.enqueue(envelopes(2))
    await vi.advanceTimersByTimeAsync(0)
    expect(queue.state().pending).toBe(2)

    await vi.advanceTimersByTimeAsync(1_000)
    expect(queue.state().pending).toBe(0)
  })

  test('a permanently rejected batch is dropped and counted, not retried forever', async () => {
    const server = makeServer()
    const queue = new EventQueue({ send: server.send, storage: new MemoryQueueStorage() })
    await queue.ready
    server.failNext(1, { ok: false, retryable: false, error: 'HTTP 400' })

    queue.enqueue(envelopes(2))
    await vi.advanceTimersByTimeAsync(0)

    expect(queue.state().pending).toBe(0)
    expect(queue.state().discarded).toBe(2)
  })
})

suite('durability', () => {
  test('an undelivered backlog survives a reload', async () => {
    const storage = new MemoryQueueStorage()
    const first = makeServer()
    const queueA = new EventQueue({
      send: first.send,
      storage,
      baseDelayMs: 1_000,
      random: () => 1,
    })
    await queueA.ready
    first.failNext(99)

    queueA.enqueue(envelopes(4))
    await vi.advanceTimersByTimeAsync(0)
    expect(first.accepted).toHaveLength(0)
    // The tab closes mid-outage.
    queueA.stop()

    expect(await storage.all()).toHaveLength(4)

    // A new session picks the backlog straight back up.
    const second = makeServer()
    const queueB = new EventQueue({ send: second.send, storage })
    await queueB.ready
    await queueB.flush()

    expect(second.accepted.map((e) => e.seq)).toEqual([1, 2, 3, 4])
    expect(await storage.all()).toHaveLength(0)
  })

  test('acknowledged events are removed from storage', async () => {
    const storage = new MemoryQueueStorage()
    const server = makeServer()
    const queue = new EventQueue({ send: server.send, storage })
    await queue.ready

    queue.enqueue(envelopes(3))
    await vi.advanceTimersByTimeAsync(0)
    expect(await storage.all()).toHaveLength(0)
  })

  test('a persisted backlog is delivered ahead of new events', async () => {
    const storage = new MemoryQueueStorage()
    await storage.put(envelopes(2, 1))
    const server = makeServer()
    const queue = new EventQueue({ send: server.send, storage, batchSize: 100 })
    await queue.ready

    queue.enqueue(envelopes(2, 3))
    await vi.advanceTimersByTimeAsync(0)
    expect(server.accepted.map((e) => e.seq)).toEqual([1, 2, 3, 4])
  })
})

suite('graceful degradation', () => {
  test('falls back to memory when IndexedDB is missing', async () => {
    const original = (globalThis as { indexedDB?: IDBFactory }).indexedDB
    delete (globalThis as { indexedDB?: IDBFactory }).indexedDB

    const storage = await openQueueStorage()
    expect(storage.durable).toBe(false)

    // And it still behaves like a store, so nothing downstream changes.
    await storage.put(envelopes(2))
    expect(await storage.all()).toHaveLength(2)
    await storage.remove([envelope(1).event_id])
    expect(await storage.all()).toHaveLength(1)

    if (original) (globalThis as { indexedDB?: IDBFactory }).indexedDB = original
  })

  test('a store that throws downgrades durability, not delivery', async () => {
    const broken = {
      durable: true,
      all: async () => {
        throw new Error('quota')
      },
      put: async () => {
        throw new Error('quota')
      },
      remove: async () => {
        throw new Error('quota')
      },
      clear: async () => {
        throw new Error('quota')
      },
    }
    const server = makeServer()
    const queue = new EventQueue({ send: server.send, storage: broken })
    await queue.ready

    queue.enqueue(envelopes(2))
    await vi.advanceTimersByTimeAsync(0)
    expect(server.accepted).toHaveLength(2)
  })

  test('stop() halts retries and leaves the backlog for next time', async () => {
    const storage = new MemoryQueueStorage()
    const server = makeServer()
    const queue = new EventQueue({ send: server.send, storage, random: () => 1 })
    await queue.ready
    server.failNext(99)

    queue.enqueue(envelopes(2))
    await vi.advanceTimersByTimeAsync(0)
    const attemptsBefore = server.attempts.length
    queue.stop()

    await vi.advanceTimersByTimeAsync(120_000)
    expect(server.attempts).toHaveLength(attemptsBefore)
    expect(await storage.all()).toHaveLength(2)
  })
})
