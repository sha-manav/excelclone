/**
 * What has actually been captured, read back from the server.
 *
 * The rest of the transparency page describes the *rules* — which actions
 * exist, what each mode records, what is never sent. All of that is true and
 * none of it answers "so what do you have on me?". This does, from the same
 * rows an exporter would receive, so the page cannot describe one thing while
 * the database holds another.
 *
 * It is also the check that would have caught a capture pipeline silently
 * dropping everything: an empty list here is a fact, where a green status chip
 * was only a claim.
 */

import { useCallback, useEffect, useState } from 'react'
import type { JSX } from 'react'
import { api } from '../capture/api'
import type { EventEnvelope } from '../capture/capture'

/** How many rows to ask for. The server clamps its own maximum. */
const LIMIT = 50

interface Props {
  /** Bumped by the caller when capture state changes, to re-read. */
  refreshKey?: number
}

type Load =
  | { kind: 'loading' }
  | { kind: 'ok'; events: EventEnvelope[] }
  | { kind: 'error'; message: string }

export function CapturedLog({ refreshKey = 0 }: Props): JSX.Element {
  const [load, setLoad] = useState<Load>({ kind: 'loading' })

  const read = useCallback(async () => {
    setLoad({ kind: 'loading' })
    const res = await api.recentEvents(LIMIT)
    if (!res.ok) {
      setLoad({
        kind: 'error',
        message:
          res.status === 0
            ? 'The API server is not reachable, so nothing can be read back.'
            : res.status === 401
              ? 'Not signed in to the API server, so nothing can be read back.'
              : (res.error ?? `HTTP ${res.status}`),
      })
      return
    }
    setLoad({ kind: 'ok', events: res.data ?? [] })
  }, [])

  useEffect(() => {
    void read()
  }, [read, refreshKey])

  return (
    <section data-testid="captured-log">
      <h2>What has been captured</h2>
      <p className="transparency__muted">
        Your own most recent events, newest first, exactly as they are stored —
        the same rows an export would contain.{' '}
        <button type="button" className="link-button" onClick={() => void read()}>
          Refresh
        </button>
      </p>

      {load.kind === 'loading' && <p className="transparency__muted">Reading…</p>}

      {load.kind === 'error' && (
        <p className="transparency__alert" data-testid="captured-log-error">
          {load.message}
        </p>
      )}

      {load.kind === 'ok' && load.events.length === 0 && (
        <p className="transparency__muted" data-testid="captured-log-empty">
          Nothing has been captured. That is what you should see with capture off — and also
          what a broken connection to the API server looks like, so check the status above.
        </p>
      )}

      {load.kind === 'ok' && load.events.length > 0 && (
        <table className="captured-log">
          <thead>
            <tr>
              <th>When</th>
              <th>Action</th>
              <th>Where</th>
              <th>What was recorded</th>
            </tr>
          </thead>
          <tbody>
            {load.events.map((e) => (
              <tr key={e.event_id} data-testid="captured-log-row">
                <td className="captured-log__when">{clockTime(e.ts_ms)}</td>
                <td>
                  <code>{e.action}</code>
                </td>
                <td className="captured-log__where">{e.context?.selection ?? ''}</td>
                <td className="captured-log__payload">
                  <code>{summarize(e.payload)}</code>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </section>
  )
}

function clockTime(ms: number): string {
  const d = new Date(ms)
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`
}

/**
 * The payload as one line.
 *
 * Rendered from the stored object rather than re-described, because the whole
 * point is to show what is *there*. Under structural mode a hashed value
 * arrives as `{hash, len, type}` and comes out looking like it — which is the
 * most convincing possible demonstration that the literal was not kept.
 */
export function summarize(payload: unknown): string {
  if (payload === null || typeof payload !== 'object') return String(payload ?? '')
  const parts: string[] = []
  for (const [key, value] of Object.entries(payload as Record<string, unknown>)) {
    parts.push(`${key}=${scalar(value)}`)
  }
  return parts.join(' ') || '—'
}

function scalar(value: unknown): string {
  if (value === null || value === undefined) return '—'
  if (typeof value === 'string') return value
  if (typeof value === 'number' || typeof value === 'boolean') return String(value)
  if (Array.isArray(value)) return `[${value.length}]`
  const obj = value as Record<string, unknown>
  // A redacted literal: show that it is a hash and what shape it stood for,
  // never a reconstruction of the value.
  if (typeof obj.hash === 'string') {
    return `⟨${obj.type ?? 'value'}:${obj.len ?? '?'} ${String(obj.hash).slice(0, 8)}…⟩`
  }
  return '{…}'
}
