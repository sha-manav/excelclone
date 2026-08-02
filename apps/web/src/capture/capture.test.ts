/**
 * Capture pipeline tests.
 *
 * These run against the real wasm redactor when it loads in node — which it
 * does, because `describeAction` is a pure function and needs no DOM. That
 * matters: the privacy assertion here is only worth something if it is testing
 * the code that actually ships. When the module cannot be instantiated the
 * suite falls back to an injected fake and asserts the weaker but still useful
 * property: that this file passes the mode through and never turns raw input
 * into an envelope by itself.
 */

import { afterEach, beforeEach, describe as suite, expect, test, vi } from 'vitest'
import {
  CaptureController,
  RingBuffer,
  ulid,
  type CaptureSink,
  type DescribeFn,
  type EventEnvelope,
} from './capture'
import type { Action } from '../engine/actions'

// --- wasm bootstrap --------------------------------------------------------

/**
 * Instantiate the wasm module for node. The specifier is held in a variable so
 * the type-checker (which is configured for the browser, without node types)
 * does not try to resolve it.
 */
const NODE_FS = 'node:fs'

async function loadRealDescribe(): Promise<DescribeFn | null> {
  try {
    const wasm = await import('gridline-wasm')
    const fs = await import(/* @vite-ignore */ NODE_FS)
    const url = new URL('../../../../crates/wasm/pkg/gridline_wasm_bg.wasm', import.meta.url)
    const bytes = new Uint8Array(fs.readFileSync(url) as ArrayLike<number>)
    wasm.initSync({ module: bytes })
    // Prove it actually works before claiming it does.
    wasm.describeAction(JSON.stringify(cellEdit('1')), 'structural', 's')
    return wasm.describeAction
  } catch {
    return null
  }
}

const realDescribe = await loadRealDescribe()

// --- helpers ---------------------------------------------------------------

function cellEdit(input: string, row = 0, col = 0): Action {
  return { action: 'cell_edit', sheet: 'Sheet1', addr: { row, col }, input }
}

interface FakeSink extends CaptureSink {
  batches: EventEnvelope[][]
  all(): EventEnvelope[]
}

function makeSink(): FakeSink {
  const batches: EventEnvelope[][] = []
  return {
    batches,
    all: () => batches.flat(),
    enqueue(events) {
      batches.push(events)
    },
    async flush() {},
  }
}

interface FakeEngine {
  onEvents(sink: (events: unknown[], action: Action) => void): () => void
  fire(action: Action): void
}

function makeEngine(): FakeEngine {
  let listener: ((events: unknown[], action: Action) => void) | null = null
  return {
    onEvents(sink) {
      listener = sink
      return () => {
        listener = null
      }
    },
    fire(action) {
      listener?.([], action)
    },
  }
}

/** A stand-in redactor that keeps shape and drops content, like the real one. */
const fakeDescribe: DescribeFn = (actionJson, mode) => {
  const action = JSON.parse(actionJson) as { action: string; input?: string }
  return JSON.stringify({
    action: 'cell.edit',
    payload: {
      addr: 'A1',
      mode_seen: mode,
      input: { hash: 'aaaabbbbccccdddd', type: 'number', len: action.input?.length ?? 0 },
    },
  })
}

type ControllerOptions = ConstructorParameters<typeof CaptureController>[0]

function makeController(over: Partial<ControllerOptions> = {}) {
  const sink = makeSink()
  const controller = new CaptureController({
    actorId: 'u_test',
    workbookId: 'wb_test',
    salt: 'test-salt',
    context: () => ({ sheet: 'Sheet1', selection: 'A1' }),
    sink,
    mode: 'structural',
    describe: realDescribe ?? fakeDescribe,
    ...over,
  })
  return { controller, sink }
}

beforeEach(() => {
  vi.useFakeTimers()
})

afterEach(() => {
  vi.useRealTimers()
})

// --- consent and pause gate ------------------------------------------------

suite('nothing is captured without permission', () => {
  test('mode off produces zero envelopes, not even buffered ones', async () => {
    const { controller, sink } = makeController({ mode: 'off' })
    const engine = makeEngine()
    controller.attach(engine)

    for (let i = 0; i < 50; i++) engine.fire(cellEdit('secret'))
    controller.noteSelection('B2')

    expect(controller.state()).toBe('off')
    expect(controller.stats().buffered).toBe(0)
    await controller.flushNow()
    await vi.advanceTimersByTimeAsync(20_000)
    expect(sink.all()).toHaveLength(0)
  })

  test('paused capture produces zero envelopes beyond the pause marker', async () => {
    const { controller, sink } = makeController()
    const engine = makeEngine()
    controller.attach(engine)

    controller.pause()
    expect(controller.state()).toBe('paused')

    for (let i = 0; i < 50; i++) engine.fire(cellEdit('secret'))
    controller.noteSelection('B2')
    await controller.flushNow()

    const names = sink.all().map((e) => e.action)
    // The pause itself is recorded, because a log should explain its own gap.
    expect(names).toEqual(['capture.pause'])
  })

  test('pause and resume bracket the gap, and consent transitions are logged', async () => {
    const { controller, sink } = makeController({ mode: 'off' })
    const engine = makeEngine()
    controller.attach(engine)

    controller.setMode('structural')
    engine.fire(cellEdit('1'))
    controller.pause()
    engine.fire(cellEdit('2'))
    controller.resume()
    engine.fire(cellEdit('3'))
    controller.setMode('off')
    engine.fire(cellEdit('4'))
    await controller.flushNow()

    expect(sink.all().map((e) => e.action)).toEqual([
      'consent.granted',
      'cell.edit',
      'capture.pause',
      'capture.resume',
      'cell.edit',
      'consent.revoked',
    ])
    const granted = sink.all()[0]
    expect(granted.payload.mode).toBe('structural')
    expect(granted.payload.consent_text_version).toBe('1')
  })
})

// --- redaction -------------------------------------------------------------

suite('structural mode never carries the raw typed value', () => {
  test.runIf(realDescribe)('the real wasm redactor hashes literals', async () => {
    const { controller, sink } = makeController({ describe: realDescribe ?? undefined })
    const engine = makeEngine()
    controller.attach(engine)

    engine.fire(cellEdit('48250'))
    await controller.flushNow()

    const [event] = sink.all()
    expect(event.action).toBe('cell.edit')
    // Not just the payload — nowhere in the envelope, context included.
    expect(JSON.stringify(event)).not.toContain('48250')
    const input = event.payload.input as { hash: string; type: string; len: number }
    expect(input.type).toBe('number')
    expect(input.len).toBe(5)
    expect(input.hash).toHaveLength(16)
    expect(event.context.privacy_mode).toBe('structural')
  })

  test.runIf(realDescribe)('formulas survive verbatim, because shape is the point', async () => {
    const { controller, sink } = makeController({ describe: realDescribe ?? undefined })
    const engine = makeEngine()
    controller.attach(engine)

    engine.fire(cellEdit('=SUM(A1:A9)*2'))
    await controller.flushNow()

    expect(sink.all()[0].payload.input).toBe('=SUM(A1:A9)*2')
  })

  test('the mode is passed through and the pipeline never stringifies raw input', async () => {
    const spy = vi.fn(fakeDescribe)
    const { controller, sink } = makeController({ describe: spy, mode: 'structural' })
    const engine = makeEngine()
    controller.attach(engine)

    engine.fire(cellEdit('48250'))
    await controller.flushNow()

    expect(spy).toHaveBeenCalledTimes(1)
    const [actionJson, mode, salt] = spy.mock.calls[0]
    expect(mode).toBe('structural')
    expect(salt).toBe('test-salt')
    // The raw value goes *into* the redactor...
    expect(actionJson).toContain('48250')
    // ...and only what the redactor returned comes out.
    expect(JSON.stringify(sink.all()[0])).not.toContain('48250')
    expect(sink.all()[0].payload.mode_seen).toBe('structural')
  })

  test('full mode is passed through unchanged', async () => {
    const spy = vi.fn(fakeDescribe)
    const { controller } = makeController({ describe: spy, mode: 'full' })
    const engine = makeEngine()
    controller.attach(engine)
    engine.fire(cellEdit('48250'))
    expect(spy.mock.calls[0][1]).toBe('full')
  })
})

// --- envelope --------------------------------------------------------------

suite('the envelope matches docs/EVENTS.md', () => {
  test('every documented field is present and seq is monotonic', async () => {
    const { controller, sink } = makeController()
    const engine = makeEngine()
    controller.attach(engine)

    engine.fire(cellEdit('1'))
    engine.fire(cellEdit('2'))
    await controller.flushNow()

    const events = sink.all()
    expect(events).toHaveLength(2)
    for (const e of events) {
      expect(e.schema_version).toBe(1)
      expect(e.event_id).toHaveLength(26)
      expect(e.session_id).toHaveLength(26)
      expect(e.actor_id).toBe('u_test')
      expect(e.workbook_id).toBe('wb_test')
      expect(typeof e.ts_ms).toBe('number')
      expect(e.client_version).toBe('0.1.0')
      expect(e.context).toEqual({
        sheet: 'Sheet1',
        selection: 'A1',
        privacy_mode: 'structural',
      })
    }
    expect(events[0].seq).toBe(1)
    expect(events[1].seq).toBe(2)
    expect(events[0].session_id).toBe(events[1].session_id)
  })

  test('ten minutes of silence starts a new session and resets seq', async () => {
    const { controller, sink } = makeController()
    const engine = makeEngine()
    controller.attach(engine)

    engine.fire(cellEdit('1'))
    await vi.advanceTimersByTimeAsync(10 * 60 * 1000 + 1)
    engine.fire(cellEdit('2'))
    await controller.flushNow()

    const events = sink.all()
    expect(events[0].session_id).not.toBe(events[events.length - 1].session_id)
    expect(events[events.length - 1].seq).toBe(1)
  })
})

// --- batching --------------------------------------------------------------

suite('delivery cadence', () => {
  test('a batch goes out at 200 events without waiting for the timer', async () => {
    const { controller, sink } = makeController()
    const engine = makeEngine()
    controller.attach(engine)

    for (let i = 0; i < 199; i++) engine.fire(cellEdit(String(i)))
    // Still nothing: the sink does no I/O of its own.
    expect(sink.batches).toHaveLength(0)

    engine.fire(cellEdit('200'))
    // The 200th event does not flush inline either — it schedules.
    expect(sink.batches).toHaveLength(0)

    await vi.advanceTimersByTimeAsync(1)
    expect(sink.batches).toHaveLength(1)
    expect(sink.batches[0]).toHaveLength(200)
    // And well before the five second timer would have fired.
    expect(sink.all()).toHaveLength(200)
  })

  test('a partial batch goes out on the five second timer', async () => {
    const { controller, sink } = makeController()
    const engine = makeEngine()
    controller.attach(engine)

    engine.fire(cellEdit('1'))
    engine.fire(cellEdit('2'))
    engine.fire(cellEdit('3'))

    await vi.advanceTimersByTimeAsync(4_900)
    expect(sink.batches).toHaveLength(0)

    await vi.advanceTimersByTimeAsync(200)
    expect(sink.batches).toHaveLength(1)
    expect(sink.batches[0]).toHaveLength(3)
  })
})

// --- ring buffer -----------------------------------------------------------

suite('the ring buffer', () => {
  test('drops the oldest entries and says how many', () => {
    const ring = new RingBuffer<number>(3)
    for (let i = 0; i < 7; i++) ring.push(i)
    expect(ring.size).toBe(3)
    expect(ring.dropped).toBe(4)
    expect(ring.take(3)).toEqual([4, 5, 6])
    expect(ring.size).toBe(0)
  })

  test('keeps working after a drain', () => {
    const ring = new RingBuffer<number>(3)
    ring.push(1)
    ring.push(2)
    expect(ring.take(1)).toEqual([1])
    ring.push(3)
    ring.push(4)
    ring.push(5)
    expect(ring.take(10)).toEqual([2, 3, 4, 5].slice(-3))
    expect(ring.dropped).toBe(1)
  })

  test('the controller surfaces drops instead of losing events silently', async () => {
    const { controller, sink } = makeController({ capacity: 5, batchSize: 10_000 })
    const engine = makeEngine()
    controller.attach(engine)

    for (let i = 0; i < 8; i++) engine.fire(cellEdit(String(i), i))
    expect(controller.stats().buffered).toBe(5)
    expect(controller.stats().dropped).toBe(3)

    await controller.flushNow()
    const events = sink.all()
    expect(events).toHaveLength(5)
    // The survivors are the newest, and their seq numbers show the gap.
    expect(events.map((e) => e.seq)).toEqual([4, 5, 6, 7, 8])
    expect(controller.stats().dropped).toBe(3)
  })
})

// --- ULID ------------------------------------------------------------------

suite('ULIDs', () => {
  test('are unique and lexicographically monotonic, even within one tick', () => {
    vi.setSystemTime(1_767_225_600_123)
    const ids: string[] = []
    for (let i = 0; i < 5_000; i++) ids.push(ulid())

    expect(new Set(ids).size).toBe(ids.length)
    for (const id of ids) {
      expect(id).toHaveLength(26)
      expect(id).toMatch(/^[0-9A-HJKMNP-TV-Z]{26}$/)
    }
    for (let i = 1; i < ids.length; i++) {
      expect(ids[i] > ids[i - 1]).toBe(true)
    }
  })

  test('stay monotonic when the clock jumps backwards', () => {
    vi.setSystemTime(2_000_000_000_000)
    const a = ulid()
    vi.setSystemTime(1_000_000_000_000)
    const b = ulid()
    expect(b > a).toBe(true)
  })

  test('advance with the clock', () => {
    vi.setSystemTime(1_600_000_000_000)
    const a = ulid()
    vi.setSystemTime(1_600_000_060_000)
    const b = ulid()
    expect(b > a).toBe(true)
    expect(b.slice(0, 10)).not.toBe(a.slice(0, 10))
  })
})

// --- nav.select sampling ---------------------------------------------------

suite('nav.select sampling', () => {
  test('coalesces a burst to two per second and keeps the latest selection', async () => {
    const { controller, sink } = makeController()
    controller.attach(makeEngine())

    for (let i = 1; i <= 20; i++) controller.noteSelection(`A${i}`)
    await controller.flushNow()
    expect(sink.all().map((e) => e.payload.selection)).toEqual(['A1', 'A2'])

    // The trailing event carries where the user actually ended up.
    await vi.advanceTimersByTimeAsync(1_100)
    await controller.flushNow()
    expect(sink.all().map((e) => e.payload.selection)).toEqual(['A1', 'A2', 'A20'])
  })

  test('never exceeds two per second over a long drag', async () => {
    const { controller, sink } = makeController()
    controller.attach(makeEngine())

    // Ten seconds of frantic clicking, twenty times a second.
    for (let tick = 0; tick < 200; tick++) {
      controller.noteSelection(`B${tick}`)
      await vi.advanceTimersByTimeAsync(50)
    }
    await controller.flushNow()

    const times = sink.all().map((e) => e.ts_ms)
    expect(times.length).toBeLessThanOrEqual(2 * 10 + 2)
    for (let i = 0; i < times.length; i++) {
      const inWindow = times.filter((t) => t >= times[i] && t < times[i] + 1000)
      expect(inWindow.length).toBeLessThanOrEqual(2)
    }
  })

  test('a selection change while paused records nothing', async () => {
    const { controller, sink } = makeController()
    controller.attach(makeEngine())
    controller.pause()
    sink.batches.length = 0

    for (let i = 0; i < 10; i++) controller.noteSelection(`C${i}`)
    await vi.advanceTimersByTimeAsync(5_000)
    expect(sink.all().filter((e) => e.action === 'nav.select')).toHaveLength(0)
  })
})

// --- resilience ------------------------------------------------------------

suite('capture never breaks the app', () => {
  test('a redactor that throws loses the event, not the action', async () => {
    const boom: DescribeFn = () => {
      throw new Error('wasm exploded')
    }
    const { controller, sink } = makeController({ describe: boom })
    const engine = makeEngine()
    controller.attach(engine)

    expect(() => engine.fire(cellEdit('1'))).not.toThrow()
    await controller.flushNow()
    expect(sink.all()).toHaveLength(0)
  })

  test('a sink that throws does not propagate into the caller', async () => {
    const controller = new CaptureController({
      actorId: 'u',
      workbookId: 'wb',
      salt: 's',
      context: () => ({ sheet: 'Sheet1', selection: 'A1' }),
      describe: realDescribe ?? fakeDescribe,
      mode: 'structural',
      sink: {
        enqueue() {
          throw new Error('queue exploded')
        },
        async flush() {
          throw new Error('flush exploded')
        },
      },
    })
    const engine = makeEngine()
    controller.attach(engine)
    engine.fire(cellEdit('1'))
    await expect(controller.flushNow()).resolves.toBeUndefined()
  })

  test('detaching stops capture', async () => {
    const { controller, sink } = makeController()
    const engine = makeEngine()
    controller.attach(engine)
    controller.detach()
    engine.fire(cellEdit('1'))
    await controller.flushNow()
    expect(sink.all()).toHaveLength(0)
  })
})
