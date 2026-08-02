/**
 * The transparency page, at `/transparency`.
 *
 * The vocabulary below is not a list someone maintained by hand — it is
 * `actionVocabulary()`, read out of the same wasm module that redacts and
 * describes every event. If the engine learns a new action, this page shows it
 * on the next reload, because the alternative is a disclosure document that
 * quietly drifts away from the code it is supposed to describe.
 */

import { useMemo } from 'react'
import { captureVocabulary, type CaptureState, type PrivacyMode } from '../capture/capture'

interface Props {
  mode: PrivacyMode
  state: CaptureState
  dropped: number
  pending: number
  durable: boolean
  onChangeMode: (mode: PrivacyMode) => void
  onBack: () => void
}

/** Grouped exactly as `docs/EVENTS.md` groups them. */
const GROUPS: { title: string; prefixes: string[] }[] = [
  { title: 'Cell and range editing', prefixes: ['cell.', 'range.', 'fill.'] },
  {
    title: 'Structure',
    prefixes: ['row.', 'col.', 'sort.', 'filter.', 'sheet.', 'format.', 'find.'],
  },
  { title: 'Files', prefixes: ['file.'] },
  { title: 'Navigation and history', prefixes: ['nav.', 'undo', 'redo'] },
  { title: 'Automation and capture control', prefixes: ['routine.', 'capture.', 'consent.'] },
]

const NOTES: Record<string, string> = {
  'cell.edit': 'Literal values are hashed under structural; formulas are kept verbatim.',
  'nav.select': 'Sampled — bursts are coalesced to at most 2 events per second.',
  'filter.apply': 'Allowed-value lists are hashed under structural.',
  'sheet.add': 'Sheet names are hashed under structural.',
  'find.replace': 'Search terms are hashed under structural.',
  'file.import': 'Warning categories only — never file contents or names under structural.',
  'routine.run': 'Tagged so mined behaviour is never confused with human behaviour.',
  'consent.revoked': 'Also stops any further capture immediately.',
}

export function TransparencyPage({
  mode,
  state,
  dropped,
  pending,
  durable,
  onChangeMode,
  onBack,
}: Props) {
  const vocabulary = useMemo(() => captureVocabulary(), [])
  const grouped = useMemo(() => {
    const seen = new Set<string>()
    const out = GROUPS.map((g) => {
      const names = vocabulary.filter((n) => {
        if (seen.has(n)) return false
        const match = g.prefixes.some((p) => (p.endsWith('.') ? n.startsWith(p) : n === p))
        if (match) seen.add(n)
        return match
      })
      return { title: g.title, names }
    }).filter((g) => g.names.length > 0)
    // Anything the engine added that this page has not been taught to group
    // still gets listed. Disclosure is not allowed to depend on a lookup table.
    const rest = vocabulary.filter((n) => !seen.has(n))
    if (rest.length > 0) out.push({ title: 'Other', names: rest })
    return out
  }, [vocabulary])

  return (
    <div className="transparency">
      <header className="transparency__header">
        <button type="button" onClick={onBack} data-testid="transparency-back">
          ← Back to the grid
        </button>
        <h1>What Gridline captures</h1>
      </header>

      <section className="transparency__status" data-testid="transparency-status">
        <p>
          Your current mode is <strong data-testid="transparency-mode">{mode}</strong> and capture
          is <strong data-testid="transparency-state">{state}</strong>.
        </p>
        <p className="transparency__muted">
          {pending} event{pending === 1 ? '' : 's'} waiting to send
          {durable ? ' (queued on disk)' : ' (queued in memory only)'}
          {dropped > 0
            ? ` · ${dropped} event${dropped === 1 ? '' : 's'} dropped because delivery fell behind`
            : ''}
          .
        </p>
        <div className="transparency__modes">
          {(['full', 'structural', 'off'] as PrivacyMode[]).map((m) => (
            <button
              key={m}
              type="button"
              className={m === mode ? 'is-current' : undefined}
              aria-pressed={m === mode}
              data-testid={`transparency-set-${m}`}
              onClick={() => onChangeMode(m)}
            >
              {m}
            </button>
          ))}
        </div>
      </section>

      <section>
        <h2>The three modes</h2>
        <dl className="transparency__modes-list">
          <dt>
            <code>full</code>
          </dt>
          <dd>
            Formulas and the values you type are recorded verbatim. Choose this only for workbooks
            whose contents are not sensitive.
          </dd>
          <dt>
            <code>structural</code> — the default
          </dt>
          <dd>
            Formulas are recorded verbatim, because a formula&rsquo;s structure is the entire point
            of finding repeated work. Literal values are <strong>not</strong> recorded. In their
            place Gridline stores a salted SHA-256 hash truncated to 16 hex characters, the type (
            <code>number</code>, <code>text</code>, or <code>bool</code>), and the length. So typing{' '}
            <code>48250</code> records that a 5-digit number was entered, and a hash that matches
            other cells containing the same number — enough to notice you copied the same value
            twice, not enough to recover the value. The salt is per workbook, is generated on the
            server, and never leaves it.
          </dd>
          <dt>
            <code>off</code>
          </dt>
          <dd>
            Nothing is captured and nothing is transmitted. The status chip reads &ldquo;capture
            off&rdquo;.
          </dd>
        </dl>
      </section>

      <section>
        <h2>The action vocabulary</h2>
        <p className="transparency__muted">
          Read live from the engine ({vocabulary.length} actions). Every variant of the engine&rsquo;s
          action type maps to exactly one name here, and the engine is the only thing that can
          mutate a workbook, so this list is complete by construction.
        </p>
        {grouped.map((group) => (
          <div key={group.title} className="transparency__group">
            <h3>{group.title}</h3>
            <ul className="transparency__vocab" data-testid="transparency-vocabulary">
              {group.names.map((name) => (
                <li key={name}>
                  <code>{name}</code>
                  {NOTES[name] && <span className="transparency__muted"> — {NOTES[name]}</span>}
                </li>
              ))}
            </ul>
          </div>
        ))}
      </section>

      <section>
        <h2>What is never captured</h2>
        <ul className="transparency__never">
          <li>Keystrokes, key timings, mouse positions, scroll offsets, or screenshots.</li>
          <li>Clipboard contents from outside Gridline.</li>
          <li>Anything at all while the mode is <code>off</code> or capture is paused.</li>
          <li>
            Data from any user who has not accepted the consent notice. Exports refuse to include
            an actor whose consent is currently <code>off</code> or revoked.
          </li>
        </ul>
      </section>

      <section>
        <h2>Delivery</h2>
        <p>
          The client keeps a ring buffer and flushes every 5 seconds or 200 events, whichever comes
          first, to <code>POST /v1/events</code>. Undelivered batches queue in memory and IndexedDB
          so a closed laptop or a dropped connection does not lose events: delivery is at-least-once
          and the server deduplicates on <code>event_id</code>. Capture never blocks or slows the
          grid.
        </p>
      </section>
    </div>
  )
}
