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

export function CaptureChip({ state, mode, dropped, pending, onToggle }: Props) {
  const parts = [`Privacy mode: ${mode}.`]
  parts.push(
    state === 'off'
      ? 'Nothing is captured and nothing is transmitted.'
      : state === 'paused'
        ? 'Capture is paused. Click to resume.'
        : 'Click to pause capture instantly.',
  )
  if (pending > 0) parts.push(`${pending} event${pending === 1 ? '' : 's'} waiting to send.`)
  if (dropped > 0) parts.push(`${dropped} event${dropped === 1 ? '' : 's'} dropped from the buffer.`)

  return (
    <button
      type="button"
      className="capture-chip"
      data-testid="capture-chip"
      data-state={state}
      data-mode={mode}
      title={parts.join(' ')}
      aria-label={`Capture ${LABEL[state]}, privacy mode ${mode}`}
      aria-pressed={state === 'paused'}
      onClick={onToggle}
    >
      <span className="capture-chip__glyph" aria-hidden="true">
        {GLYPH[state]}
      </span>
      <span className="capture-chip__label">{LABEL[state]}</span>
      {dropped > 0 && (
        <span className="capture-chip__warn" data-testid="capture-dropped">
          {dropped} dropped
        </span>
      )}
    </button>
  )
}
