/**
 * The capture status chip.
 *
 * `docs/PRIVACY.md` promises that capture state is always visible and that one
 * click pauses it. This is that promise: a toolbar control that is never
 * hidden behind a menu and never lies about what is happening.
 */

import type { CaptureState, PrivacyMode } from '../capture/capture'

interface Props {
  state: CaptureState
  mode: PrivacyMode
  /** Events the ring buffer discarded. Shown so a loss is never silent. */
  dropped: number
  /** Envelopes not yet acknowledged by the server. */
  pending: number
  /** Envelopes the server refused. Above zero means capture is not working. */
  rejected: number
  /**
   * Why this browser has no account on the server, or null when it has one.
   *
   * Outranks `rejected` because it comes first: with no credential there is
   * nothing to reject, so the count stays at zero while nothing works.
   */
  registration: string | null
  onToggle: () => void
}

const GLYPH: Record<CaptureState, string> = {
  capturing: '●', // ●
  paused: '‖', // ‖
  off: '○', // ○
}

const LABEL: Record<CaptureState, string> = {
  capturing: 'capturing',
  paused: 'paused',
  off: 'capture off',
}

export function CaptureChip({
  state,
  mode,
  dropped,
  pending,
  rejected,
  registration,
  onToggle,
}: Props) {
  // A rejection outranks the nominal state. Saying "capturing" while the
  // server is refusing every batch is the one thing this chip exists not to
  // do — and it is precisely what a blank auth token looks like: no backlog,
  // no error, and nothing arriving.
  //
  // A failed registration outranks both, and is checked first because it sits
  // upstream of them: no account means no request to reject, so `rejected`
  // stays at zero and the healthy-looking case is the broken one again.
  const broken = registration !== null || (rejected > 0 && state === 'capturing')

  const parts = [`Privacy mode: ${mode}.`]
  parts.push(
    registration !== null
      ? `This browser has no account on the server, so nothing can be recorded: ${registration}`
      : state === 'off'
        ? 'Nothing is captured and nothing is transmitted.'
        : broken
          ? 'The server is refusing these events, so nothing is being recorded. Check the API server and the auth token.'
          : state === 'paused'
            ? 'Capture is paused. Click to resume.'
            : 'Click to pause capture instantly.',
  )
  if (pending > 0) parts.push(`${pending} event${pending === 1 ? '' : 's'} waiting to send.`)
  if (rejected > 0) parts.push(`${rejected} event${rejected === 1 ? '' : 's'} rejected by the server.`)
  if (dropped > 0) parts.push(`${dropped} event${dropped === 1 ? '' : 's'} dropped from the buffer.`)

  return (
    <button
      type="button"
      className="capture-chip"
      data-testid="capture-chip"
      data-state={broken ? 'rejected' : state}
      data-mode={mode}
      title={parts.join(' ')}
      aria-label={
        registration !== null
          ? `Capture unavailable, no account on the server, privacy mode ${mode}`
          : broken
            ? `Capture failing, ${rejected} events rejected, privacy mode ${mode}`
            : `Capture ${LABEL[state]}, privacy mode ${mode}`
      }
      aria-pressed={state === 'paused'}
      onClick={onToggle}
    >
      <span className="capture-chip__glyph" aria-hidden="true">
        {broken ? '!' : GLYPH[state]}
      </span>
      <span className="capture-chip__label">{broken ? 'not recording' : LABEL[state]}</span>
      {registration !== null && (
        <span className="capture-chip__warn" data-testid="capture-unregistered">
          no account
        </span>
      )}
      {rejected > 0 && (
        <span className="capture-chip__warn" data-testid="capture-rejected">
          {rejected} rejected
        </span>
      )}
      {dropped > 0 && (
        <span className="capture-chip__warn" data-testid="capture-dropped">
          {dropped} dropped
        </span>
      )}
    </button>
  )
}
