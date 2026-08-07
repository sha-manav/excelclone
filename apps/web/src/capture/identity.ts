/**
 * Getting a credential, for a visitor who has never been here before.
 *
 * On a hosted deployment there is no sign-in and no seeding script: the app
 * asks the server for an anonymous account the first time it needs one, and
 * that is the entire identity story. `POST /v1/register` returns an opaque id
 * and a token — no email, no password, no profile.
 *
 * Registering is *not* consent. A fresh account has no consent record, so the
 * notice still appears and nothing is captured until it is answered. All this
 * buys is the ability to speak to the server at all.
 *
 * The reason this is a module rather than three lines in `useCapture`: it has
 * to be exactly-once across concurrent callers and StrictMode double-mounts,
 * and it has to distinguish "cannot reach the server yet" from "this server
 * will never register me". Getting the second one wrong is how this project
 * previously spent a day discarding every event while reporting good health.
 */

import { api, authToken, setAuthToken } from './api'
import { isStandalone } from '../standalone'

export type Identity =
  /** Usable credentials. `actorId` is null when the token was already stored. */
  | { kind: 'ready'; actorId: string | null }
  /** No credentials, but the reason might not last. Worth retrying. */
  | { kind: 'pending'; reason: string }
  /** No credentials and no prospect of any. Must be shown to the user. */
  | { kind: 'refused'; reason: string }

/**
 * How many times a dead token may be swapped for a fresh registration before
 * the app concludes the problem is not the token. Without a bound, a server
 * that issues tokens and then rejects them is an infinite registration loop.
 */
const MAX_REREGISTRATIONS = 3

let inFlight: Promise<Identity> | null = null
let settled: Identity | null = null
let reregistrations = 0

/** Test seam. Nothing in the app calls this. */
export function resetIdentity(): void {
  inFlight = null
  settled = null
  reregistrations = 0
}

export function ensureIdentity(): Promise<Identity> {
  // A standalone build has no server to register with, and asking would be a
  // network call the page promises not to make.
  if (isStandalone()) {
    return Promise.resolve({ kind: 'refused', reason: 'this build has no server' })
  }
  if (authToken()) return Promise.resolve({ kind: 'ready', actorId: null })
  // A refusal is remembered; a pending failure is not, so the next attempt
  // actually tries again instead of returning a stale disappointment.
  if (settled?.kind === 'refused') return Promise.resolve(settled)
  // Concurrent callers — the queue flushing while the consent effect runs —
  // share one request rather than racing to create two accounts.
  inFlight ??= registerOnce().finally(() => {
    inFlight = null
  })
  return inFlight
}

async function registerOnce(): Promise<Identity> {
  const res = await api.register()
  if (res.ok && res.data?.token) {
    setAuthToken(res.data.token)
    settled = { kind: 'ready', actorId: res.data.actor_id ?? null }
    return settled
  }
  // Offline, DNS, 5xx, 429. The server may well register us in a minute.
  if (res.retryable || res.status === 0) {
    return { kind: 'pending', reason: res.error ?? 'the server is not reachable' }
  }
  // 403 (registration closed), 404 (an API too old to have the endpoint), or
  // anything else final. Remembered, so the app stops asking and starts
  // telling the user.
  settled = {
    kind: 'refused',
    reason:
      res.status === 404
        ? 'this server is too old to register new users'
        : (res.data?.error ?? res.error ?? `HTTP ${res.status}`),
  }
  return settled
}

/**
 * Called when the server rejects the stored token outright.
 *
 * A 401 means the credential is dead, and re-presenting it is the one thing
 * guaranteed not to work — a database rebuilt underneath a browser that had
 * been talking to it happily. Dropping it and registering again recovers,
 * where the previous behaviour was to present the corpse forever.
 *
 * Returns whether the caller should treat its failed request as retryable. It
 * says no once the swap has been tried enough times to conclude the token was
 * never the problem, which is what stops an unauthenticated loop.
 */
export function invalidateIdentity(): boolean {
  if (reregistrations >= MAX_REREGISTRATIONS) {
    settled = {
      kind: 'refused',
      reason: 'the server keeps rejecting freshly issued tokens',
    }
    return false
  }
  reregistrations += 1
  setAuthToken(null)
  settled = null
  inFlight = null
  return true
}
