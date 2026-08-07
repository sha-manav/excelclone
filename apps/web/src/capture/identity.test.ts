import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { api, setAuthToken, TOKEN_KEY } from './api'
import { ensureIdentity, invalidateIdentity, resetIdentity } from './identity'

/**
 * Getting, keeping and replacing an anonymous account.
 *
 * The failure this file is really about is not "registration broke" — it is
 * "registration broke and the app carried on saying it was recording". Every
 * test below asserts on which of `ready` / `pending` / `refused` came back,
 * because that word is what decides whether a batch is held, retried or
 * discarded, and whether the user is told.
 */

const storage = new Map<string, string>()

function mockLocalStorage() {
  vi.stubGlobal('localStorage', {
    getItem: (k: string) => storage.get(k) ?? null,
    setItem: (k: string, v: string) => void storage.set(k, v),
    removeItem: (k: string) => void storage.delete(k),
  })
}

/** A `fetch` answering `POST /v1/register` with a given status and body. */
function registerReplying(status: number, body: unknown) {
  return vi.fn(async (_url: string, _init?: unknown) => {
    const text = JSON.stringify(body)
    return {
      ok: status >= 200 && status < 300,
      status,
      text: async () => text,
    } as unknown as Response
  })
}

beforeEach(() => {
  storage.clear()
  mockLocalStorage()
  resetIdentity()
})

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('ensureIdentity', () => {
  it('registers when there is no token and stores what comes back', async () => {
    const fetchMock = registerReplying(200, { token: 'gl_new', actor_id: 'u_abc' })
    vi.stubGlobal('fetch', fetchMock)

    const identity = await ensureIdentity()

    expect(identity).toEqual({ kind: 'ready', actorId: 'u_abc' })
    expect(storage.get(TOKEN_KEY)).toBe('gl_new')
    expect(fetchMock).toHaveBeenCalledTimes(1)
    expect(String(fetchMock.mock.calls[0][0])).toContain('/v1/register')
  })

  it('does not register when a token is already stored', async () => {
    const fetchMock = registerReplying(200, { token: 'gl_new', actor_id: 'u_abc' })
    vi.stubGlobal('fetch', fetchMock)
    setAuthToken('gl_existing')

    const identity = await ensureIdentity()

    expect(identity.kind).toBe('ready')
    expect(fetchMock).not.toHaveBeenCalled()
    expect(storage.get(TOKEN_KEY)).toBe('gl_existing')
  })

  it('creates one account when several callers ask at once', async () => {
    // The queue flushing while the consent effect runs, or StrictMode
    // double-mounting. Two accounts would split one person's events in half
    // and neither half would be a session.
    const fetchMock = registerReplying(200, { token: 'gl_new', actor_id: 'u_abc' })
    vi.stubGlobal('fetch', fetchMock)

    const all = await Promise.all([ensureIdentity(), ensureIdentity(), ensureIdentity()])

    expect(fetchMock).toHaveBeenCalledTimes(1)
    expect(all.every((i) => i.kind === 'ready')).toBe(true)
  })

  it('reports an unreachable server as pending, and tries again next time', async () => {
    const failing = vi.fn(async () => {
      throw new Error('network down')
    })
    vi.stubGlobal('fetch', failing)

    const first = await ensureIdentity()
    expect(first.kind).toBe('pending')

    // The server comes back. Nothing should have been cached that stops the
    // app from ever noticing.
    vi.stubGlobal('fetch', registerReplying(200, { token: 'gl_new', actor_id: 'u_abc' }))
    const second = await ensureIdentity()
    expect(second).toEqual({ kind: 'ready', actorId: 'u_abc' })
  })

  it('treats a 429 as pending rather than final', async () => {
    vi.stubGlobal('fetch', registerReplying(429, { error: 'too many registrations' }))
    expect((await ensureIdentity()).kind).toBe('pending')
  })

  it('remembers a refusal instead of asking forever', async () => {
    const fetchMock = registerReplying(403, {
      error: 'this server does not accept new registrations',
    })
    vi.stubGlobal('fetch', fetchMock)

    const first = await ensureIdentity()
    const second = await ensureIdentity()

    expect(first.kind).toBe('refused')
    expect(second.kind).toBe('refused')
    if (first.kind === 'refused') {
      expect(first.reason).toContain('does not accept new registrations')
    }
    expect(fetchMock).toHaveBeenCalledTimes(1)
  })

  it('explains a 404 as an out-of-date server rather than echoing the status', async () => {
    // The likely misconfiguration: a Pages build pointed at an API deployed
    // before the endpoint existed. "HTTP 404" sends someone hunting a typo.
    vi.stubGlobal('fetch', registerReplying(404, {}))
    const identity = await ensureIdentity()
    expect(identity.kind).toBe('refused')
    if (identity.kind === 'refused') expect(identity.reason).toContain('too old')
  })
})

describe('invalidateIdentity', () => {
  it('drops a dead token so the next attempt registers again', async () => {
    vi.stubGlobal('fetch', registerReplying(200, { token: 'gl_first', actor_id: 'u_1' }))
    await ensureIdentity()
    expect(storage.get(TOKEN_KEY)).toBe('gl_first')

    expect(invalidateIdentity()).toBe(true)
    expect(storage.get(TOKEN_KEY)).toBeUndefined()

    vi.stubGlobal('fetch', registerReplying(200, { token: 'gl_second', actor_id: 'u_2' }))
    expect(await ensureIdentity()).toEqual({ kind: 'ready', actorId: 'u_2' })
  })

  it('stops swapping once it is clear the token was never the problem', async () => {
    // A server that issues tokens and then rejects them would otherwise be an
    // unbounded registration loop — an account created per batch, forever.
    vi.stubGlobal('fetch', registerReplying(200, { token: 'gl_x', actor_id: 'u_x' }))
    await ensureIdentity()

    expect(invalidateIdentity()).toBe(true)
    expect(invalidateIdentity()).toBe(true)
    expect(invalidateIdentity()).toBe(true)
    expect(invalidateIdentity()).toBe(false)

    const identity = await ensureIdentity()
    expect(identity.kind).toBe('refused')
    if (identity.kind === 'refused') expect(identity.reason).toContain('keeps rejecting')
  })
})

describe('the queue sender', () => {
  const batch = [{ event_id: 'ev_1' }] as never

  it('holds a batch while registration is only temporarily failing', async () => {
    // Retryable means the queue keeps it. Getting this wrong discards the
    // backlog every time a laptop lid closes.
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        throw new Error('offline')
      }),
    )
    const result = await api.sender()(batch)
    expect(result.ok).toBe(false)
    expect(result.retryable).toBe(true)
  })

  it('gives up on a batch when the server will never register this browser', async () => {
    // Not retryable, so it is counted as discarded — which is what puts "not
    // recording" on the chip. Holding it forever would look healthy instead.
    vi.stubGlobal('fetch', registerReplying(403, { error: 'closed' }))
    const result = await api.sender()(batch)
    expect(result.ok).toBe(false)
    expect(result.retryable).toBe(false)
  })

  it('keeps a batch when the token turns out to be dead', async () => {
    // The database was rebuilt under a browser that had been talking to it.
    // The events are fine; only the credential is stale.
    setAuthToken('gl_dead')
    const fetchMock = vi.fn(
      async () =>
        ({
          ok: false,
          status: 401,
          text: async () => '{"error":"missing or unknown bearer token"}',
        }) as unknown as Response,
    )
    vi.stubGlobal('fetch', fetchMock)

    const result = await api.sender()(batch)

    expect(result.ok).toBe(false)
    expect(result.retryable).toBe(true)
    expect(storage.get(TOKEN_KEY)).toBeUndefined()
  })

  it('sends normally once there is a working token', async () => {
    setAuthToken('gl_good')
    vi.stubGlobal(
      'fetch',
      vi.fn(
        async () =>
          ({
            ok: true,
            status: 200,
            text: async () => '{"accepted":1,"duplicates":0}',
          }) as unknown as Response,
      ),
    )
    const result = await api.sender()(batch)
    expect(result.ok).toBe(true)
  })
})
