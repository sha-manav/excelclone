/**
 * The consent notice, shown once before anything is captured.
 *
 * The wording here is `docs/PRIVACY.md`, not a friendlier paraphrase of it. A
 * consent screen that promises something the code does not do is worse than no
 * consent screen, so the two are kept identical on purpose.
 */

import { useEffect, useRef, useState } from 'react'
import type { PrivacyMode } from '../capture/capture'

interface Props {
  onChoose: (mode: PrivacyMode) => void
  onOpenTransparency: () => void
}

const FOCUSABLE =
  'button, [href], input:not([disabled]), select, textarea, [tabindex]:not([tabindex="-1"])'

export function ConsentModal({ onChoose, onOpenTransparency }: Props) {
  // `structural` is pre-selected: the default is the private one.
  const [mode, setMode] = useState<PrivacyMode>('structural')
  const dialogRef = useRef<HTMLDivElement>(null)
  const chooseRef = useRef(onChoose)
  chooseRef.current = onChoose

  useEffect(() => {
    const el = dialogRef.current
    if (!el) return
    const restoreTo = document.activeElement as HTMLElement | null
    const focusables = () => Array.from(el.querySelectorAll<HTMLElement>(FOCUSABLE))
    focusables()[0]?.focus()

    const onKeyDown = (e: KeyboardEvent) => {
      // The dialog is modal, so the app's own shortcuts must not see these
      // keys — Delete would otherwise clear the selection behind the overlay.
      e.stopPropagation()
      if (e.key === 'Escape') {
        // Escape is a refusal, never an accidental acceptance.
        e.preventDefault()
        chooseRef.current('off')
        return
      }
      if (e.key !== 'Tab') return
      const list = focusables()
      if (list.length === 0) return
      const first = list[0]
      const last = list[list.length - 1]
      const active = document.activeElement
      if (e.shiftKey && (active === first || !el.contains(active))) {
        e.preventDefault()
        last.focus()
      } else if (!e.shiftKey && (active === last || !el.contains(active))) {
        e.preventDefault()
        first.focus()
      }
    }

    document.addEventListener('keydown', onKeyDown, true)
    return () => {
      document.removeEventListener('keydown', onKeyDown, true)
      restoreTo?.focus()
    }
  }, [])

  return (
    <div className="consent-backdrop" data-testid="consent-backdrop">
      <div
        className="consent"
        role="dialog"
        aria-modal="true"
        aria-labelledby="consent-title"
        aria-describedby="consent-intro"
        data-testid="consent-modal"
        ref={dialogRef}
      >
        <h2 id="consent-title">Before Gridline records anything</h2>

        <p id="consent-intro">
          Gridline records what you do in the app so it can find repetitive work
          and offer to automate it. Capture is <strong>off until you turn it
          on</strong>. Nothing is recorded or sent before you accept this
          notice.
        </p>

        <p className="consent__note">
          Gridline records <em>actions</em>, not input: which cells, what kind
          of operation, when. It does <strong>not</strong> record keystrokes,
          key timings, mouse movement, scroll position, screenshots, or the
          contents of your screen. Selection changes are sampled to at most two
          events per second. Data goes only to the Gridline server — there are
          no third-party analytics or telemetry SDKs in this application, of
          any kind.
        </p>

        <fieldset className="consent__modes">
          <legend>Choose what is captured</legend>

          <label className="consent__mode">
            <input
              type="radio"
              name="privacy-mode"
              value="structural"
              checked={mode === 'structural'}
              onChange={() => setMode('structural')}
              data-testid="consent-mode-structural"
            />
            <span>
              <strong>Structural</strong> <span className="consent__badge">default</span>
              <br />
              Formulas are recorded verbatim, because a formula&rsquo;s
              structure is the entire point of finding repeated work — and
              verbatim includes what is written inside them, so the words in{' '}
              <code>=IF(A1&gt;0,&quot;paid&quot;,&quot;due&quot;)</code> and the
              sheet name in <code>=VLOOKUP(B2,Rates!A:B,2,0)</code> are both
              recorded. Sheet names are hashed everywhere else. Literal
              values are <strong>not</strong> recorded — in their place Gridline
              stores a salted SHA-256 hash truncated to 16 hex characters, the
              type (<code>number</code>, <code>text</code>, or <code>bool</code>),
              and the length. Typing <code>48250</code> records that a 5-digit
              number was entered, and a hash that matches other cells containing
              the same number: enough to notice you copied the same value twice,
              not enough to recover the value. The salt is per workbook and
              never leaves the server, so hashes cannot be compared across
              workbooks.
            </span>
          </label>

          <label className="consent__mode">
            <input
              type="radio"
              name="privacy-mode"
              value="full"
              checked={mode === 'full'}
              onChange={() => setMode('full')}
              data-testid="consent-mode-full"
            />
            <span>
              <strong>Full</strong>
              <br />
              Formulas and the values you type are recorded verbatim. Choose
              this only for workbooks whose contents are not sensitive.
            </span>
          </label>
        </fieldset>

        <p className="consent__note">
          You can change the mode or revoke consent at any time. Capture state
          is always visible in the toolbar, and clicking it pauses capture
          instantly. Revoking stops capture immediately and excludes you from
          every dataset export from that moment on.
        </p>

        <p className="consent__note">
          <button className="linklike" onClick={onOpenTransparency} type="button">
            See exactly what is captured
          </button>{' '}
          — the transparency page lists the live action vocabulary, generated
          from the same code that does the capturing.
        </p>

        <div className="consent__actions">
          <button
            className="button--primary"
            onClick={() => onChoose(mode)}
            data-testid="consent-accept"
            type="button"
          >
            Accept {mode} capture
          </button>
          <button
            onClick={() => onChoose('off')}
            data-testid="consent-decline"
            type="button"
          >
            No thanks — capture off
          </button>
        </div>

        <p className="consent__fineprint">
          Declining sets the mode to <code>off</code>: nothing is captured and
          nothing is transmitted. The status chip will read &ldquo;capture
          off&rdquo;.
        </p>
      </div>
    </div>
  )
}
