/**
 * The Gridline API client.
 *
 * The only network destination in this application. There is no analytics SDK,
 * no error reporter, no third-party anything — `docs/PRIVACY.md` promises that
 * and this file is where it is kept.
 *
 * Nothing here throws. A capture pipeline that can raise into a React render
 * because a laptop lid was closed is worse than one that quietly retries, so
 * every failure comes back as a value.
 */

import type { EventEnvelope, PrivacyMode } from './capture'
import type { SendResult } from './queue'

export const TOKEN_KEY = 'gridline.token'

export interface ApiResponse<T> {
  ok: boolean
  status: number
  data: T | null
  error: string | null
  /** Whether the caller should try the same request again later. */
  retryable: boolean
}

export interface ConsentRecord {
  mode: PrivacyMode
  consent_text_version: string
  granted_at_ms: number
  /** Per-workbook hashing salt. Issued by the server; see `docs/PRIVACY.md`. */
  salt?: string
}

export interface IngestAck {
  accepted: number
  duplicates: number
}

/** A routine as the server stores it. `body` is the engine-shaped macro. */
export interface RoutineRecord {
  id: string
  workbook_id: string
  summary: string
  body: unknown
  estimated_minutes_saved: number
  support: number
  status: 'proposed' | 'accepted' | 'dismissed'
  created_at: string
}

export function apiBaseUrl(): string {
  const env = import.meta.env as { VITE_API_URL?: string } | undefined
  return env?.VITE_API_URL ?? 'http://localhost:8787'
}

export function authToken(): string | null {
  try {
    return globalThis.localStorage?.getItem(TOKEN_KEY) ?? null
  } catch {
    // Storage can be disabled outright; that is not an error worth surfacing.
    return null
  }
}

export function setAuthToken(token: string | null): void {
  try {
    if (token === null) globalThis.localStorage?.removeItem(TOKEN_KEY)
    else globalThis.localStorage?.setItem(TOKEN_KEY, token)
  } catch {
    /* ignore */
  }
}

/** 5xx, 408 and 429 are worth another attempt; the rest of 4xx is not. */
function statusIsRetryable(status: number): boolean {
  if (status === 408 || status === 429) return true
  return status >= 500
}

/**
 * Asked once when a request comes back 401, to see whether better credentials
 * are available. Returns true if it changed the token, in which case the
 * request is tried again.
 *
 * A 401 means the stored token is *dead*, and continuing to present it is the
 * one thing that certainly will not work. Re-seeding a development database
 * used to make that permanent: the browser held a token the new database had
 * never heard of, nothing replaced it, and no reload or restart recovered —
 * every event was rejected and discarded, forever, on a machine whose setup
 * looked correct.
 *
 * The recovery itself lives in `useCapture`, because only it knows what a
 * development build is allowed to substitute.
 */
let recoverAuth: (() => boolean) | null = null

export function setAuthRecovery(fn: (() => boolean) | null): void {
  recoverAuth = fn
}

async function call<T>(
  path: string,
  init: { method: string; body?: unknown },
  retried = false,
): Promise<ApiResponse<T>> {
  const headers: Record<string, string> = { 'content-type': 'application/json' }
  const token = authToken()
  if (token) headers.authorization = `Bearer ${token}`

  let res: Response
  try {
    res = await fetch(`${apiBaseUrl()}${path}`, {
      method: init.method,
      headers,
      body: init.body === undefined ? undefined : JSON.stringify(init.body),
    })
  } catch (e) {
    // Offline, DNS failure, server down. All of these deserve a retry.
    return { ok: false, status: 0, data: null, error: String(e), retryable: true }
  }

  let data: T | null = null
  let text = ''
  try {
    text = await res.text()
    if (text) data = JSON.parse(text) as T
  } catch {
    data = null
  }

  if (!res.ok) {
    // Exactly one retry, and only when the credentials actually changed —
    // otherwise a server that rejects everything becomes an infinite loop.
    if (res.status === 401 && !retried && recoverAuth?.()) {
      return call<T>(path, init, true)
    }
    return {
      ok: false,
      status: res.status,
      data,
      error: text || `HTTP ${res.status}`,
      retryable: statusIsRetryable(res.status),
    }
  }
  return { ok: true, status: res.status, data, error: null, retryable: false }
}

export class GridlineApi {
  /** `POST /v1/events`. Idempotent server-side on `event_id`. */
  async postEvents(events: EventEnvelope[]): Promise<ApiResponse<IngestAck>> {
    return call<IngestAck>('/v1/events', { method: 'POST', body: { events } })
  }

  /** `POST /v1/consent`. */
  async postConsent(record: ConsentRecord): Promise<ApiResponse<ConsentRecord>> {
    return call<ConsentRecord>('/v1/consent', { method: 'POST', body: record })
  }

  /**
   * `GET /v1/events/recent` — the caller's own most recent events.
   *
   * Not an admin endpoint: it is the user's own record, and a page that can
   * only describe what *would* be captured is half a promise.
   */
  async recentEvents(limit = 50): Promise<ApiResponse<EventEnvelope[]>> {
    return call<EventEnvelope[]>(`/v1/events/recent?limit=${limit}`, { method: 'GET' })
  }

  /** `GET /v1/consent/me`. */
  async getConsent(): Promise<ApiResponse<ConsentRecord>> {
    return call<ConsentRecord>('/v1/consent/me', { method: 'GET' })
  }

  /** `GET /v1/routines?workbook=…`. */
  async getRoutines(workbookId: string): Promise<ApiResponse<RoutineRecord[]>> {
    return call<RoutineRecord[]>(
      `/v1/routines?workbook=${encodeURIComponent(workbookId)}`,
      { method: 'GET' },
    )
  }

  /** `POST /v1/routines/:id/feedback`. */
  async postRoutineFeedback(
    id: string,
    status: 'accepted' | 'dismissed',
  ): Promise<ApiResponse<{ id: string; status: string }>> {
    return call<{ id: string; status: string }>(
      `/v1/routines/${encodeURIComponent(id)}/feedback`,
      { method: 'POST', body: { status } },
    )
  }

  /** Adapter for `EventQueue`, which only cares whether to retry. */
  sender(): (batch: EventEnvelope[]) => Promise<SendResult> {
    return async (batch) => {
      const res = await this.postEvents(batch)
      return { ok: res.ok, retryable: res.retryable, error: res.error ?? undefined }
    }
  }
}

export const api = new GridlineApi()
