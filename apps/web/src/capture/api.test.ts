import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { api, setAuthRecovery, setAuthToken, TOKEN_KEY } from './api'

/**
 * The 401 recovery path.
 *
 * A stored token the server rejects is dead, and presenting it again is the
 * one thing that certainly cannot work. Re-seeding a development database used
 * to make that permanent: the browser held a token the new database had never
 * heard of, and no reload, restart or re-seed replaced it — every event was
 * rejected and discarded on a machine whose setup looked correct.
 */

/** A fetch that answers 401 until the request carries `goodToken`. */
function fetchGate(goodToken: string) {
  const seen: (string | undefined)[] = []
  const fn = vi.fn(async (_url: string, init?: { headers?: Record<string, string> }) => {
    const auth = init?.headers?.authorization
    seen.push(auth)
    const ok = auth === `Bearer ${goodToken}`
    return {
      ok,
      status: ok ? 200 : 401,
      text: async () => (ok ? '{"mode":"full"}' : '{"error":"missing or unknown bearer token"}'),
    } as unknown as Response
  })
  return { fn, seen }
}

const storage = new Map<string, string>()

beforeEach(() => {
  storage.clear()
  vi.stubGlobal('localStorage', {
    getItem: (k: string) => storage.get(k) ?? null,
    setItem: (k: string, v: string) => void storage.set(k, v),
    removeItem: (k: string) => void storage.delete(k),
  })
})

afterEach(() => {
  setAuthRecovery(null)
  vi.unstubAllGlobals()
})

describe('401 recovery', () => {
  it('swaps a rejected token for a working one and retries once', async () => {
    const gate = fetchGate('fresh')
    vi.stubGlobal('fetch', gate.fn)
    setAuthToken('stale')
    setAuthRecovery(() => {
      setAuthToken('fresh')
      return true
    })

    const res = await api.getConsent()

    expect(res.ok).toBe(true)
    expect(gate.seen).toEqual(['Bearer stale', 'Bearer fresh'])
    expect(storage.get(TOKEN_KEY)).toBe('fresh')
  })

  it('gives up after one retry rather than looping', async () => {
    // A server that rejects everything must not become an infinite request
    // loop just because the recovery hook keeps saying it changed something.
    const gate = fetchGate('never-offered')
    vi.stubGlobal('fetch', gate.fn)
    setAuthToken('stale')
    let calls = 0
    setAuthRecovery(() => {
      calls += 1
      setAuthToken(`attempt-${calls}`)
      return true
    })

    const res = await api.getConsent()

    expect(res.ok).toBe(false)
    expect(res.status).toBe(401)
    expect(gate.fn).toHaveBeenCalledTimes(2)
    expect(calls).toBe(1)
  })

  it('does not retry when there is nothing better to offer', async () => {
    const gate = fetchGate('fresh')
    vi.stubGlobal('fetch', gate.fn)
    setAuthToken('stale')
    setAuthRecovery(() => false)

    const res = await api.getConsent()

    expect(res.ok).toBe(false)
    expect(gate.fn).toHaveBeenCalledTimes(1)
  })

  it('leaves other failures alone', async () => {
    // Only a 401 says "these credentials are wrong". A 500 is the server's
    // problem and swapping tokens over it would be superstition.
    const fn = vi.fn(
      async () =>
        ({ ok: false, status: 500, text: async () => 'boom' }) as unknown as Response,
    )
    vi.stubGlobal('fetch', fn)
    setAuthToken('stale')
    const recover = vi.fn(() => true)
    setAuthRecovery(recover)

    const res = await api.getConsent()

    expect(res.status).toBe(500)
    expect(res.retryable).toBe(true)
    expect(recover).not.toHaveBeenCalled()
    expect(fn).toHaveBeenCalledTimes(1)
  })
})
