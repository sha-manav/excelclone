/**
 * The Routines panel: what the miner noticed, and what it would do about it.
 *
 * The design problem here is trust. A panel that offers to change a
 * spreadsheet has to earn a click, and the only thing that earns it is
 * showing the change first. So every proposal renders its dry run against the
 * *current* selection before Run is available, and the preview comes from the
 * same engine the run will use — not a description of what should happen.
 *
 * Three rules follow:
 *
 *   - A routine that would change nothing here says so, and Run is disabled.
 *     Silently applying a no-op teaches people the button is decorative.
 *   - A routine the engine would refuse shows the refusal, before the click.
 *   - A routine carrying values the log could not recover names them and runs
 *     the rest. "Partial" is a fact about the log, not a defect to hide.
 */

import { useMemo, useState } from 'react'
import type { JSX } from 'react'
import type { EngineHandle, RoutinePreview } from '../engine/bridge'
import type { RoutineRecord } from '../capture/api'
import { a1, type Addr } from '../engine/actions'

export interface RoutinesPanelProps {
  engine: EngineHandle
  routines: RoutineRecord[]
  sheet: string
  anchor: Addr
  /** Bump when the workbook changes, so previews are recomputed. */
  version: number
  loading: boolean
  error: string | null
  onRun(routine: RoutineRecord): void
  onDismiss(routine: RoutineRecord): void
  onRefresh(): void
  onClose(): void
}

const MAX_PREVIEW_ROWS = 8

export function RoutinesPanel(props: RoutinesPanelProps): JSX.Element {
  const {
    engine,
    routines,
    sheet,
    anchor,
    version,
    loading,
    error,
    onRun,
    onDismiss,
    onRefresh,
    onClose,
  } = props

  const proposed = routines.filter((r) => r.status !== 'dismissed')

  return (
    <aside className="drawer routines" role="dialog" aria-label="Routines">
      <header className="drawer__head">
        <h2>Routines ({proposed.length})</h2>
        <div>
          <button onClick={onRefresh} disabled={loading} aria-label="Refresh routines">
            {loading ? '…' : '↻'}
          </button>
          <button aria-label="Close routines" onClick={onClose}>
            ×
          </button>
        </div>
      </header>
      <p className="drawer__intro">
        Work Gridline noticed you repeating. Each one previews against{' '}
        <strong>{a1(anchor)}</strong> before it runs, and runs through the same
        actions you would have typed — so it lands in one undo step.
      </p>

      {error && <p className="routines__error">{error}</p>}
      {!error && proposed.length === 0 && (
        <p className="muted">
          Nothing yet. Routines appear once the same sequence of steps shows up
          often enough to be worth more than a couple of minutes.
        </p>
      )}

      <ul className="drawer__list">
        {proposed.map((r) => (
          <RoutineCard
            key={r.id}
            engine={engine}
            routine={r}
            sheet={sheet}
            anchor={anchor}
            version={version}
            onRun={() => onRun(r)}
            onDismiss={() => onDismiss(r)}
          />
        ))}
      </ul>
    </aside>
  )
}

interface RoutineCardProps {
  engine: EngineHandle
  routine: RoutineRecord
  sheet: string
  anchor: Addr
  version: number
  onRun(): void
  onDismiss(): void
}

function RoutineCard(props: RoutineCardProps): JSX.Element {
  const { engine, routine, sheet, anchor, version, onRun, onDismiss } = props
  const [open, setOpen] = useState(false)

  // Recomputed whenever the selection or the workbook moves: a preview of a
  // stale position is worse than no preview, because it looks current.
  const preview: RoutinePreview | { failed: string } = useMemo(() => {
    void version
    try {
      return engine.previewRoutine(routine.body, sheet, anchor.row, anchor.col)
    } catch (e) {
      return { failed: String(e) }
    }
  }, [engine, routine.body, sheet, anchor.row, anchor.col, version])

  if ('failed' in preview) {
    return (
      <li className="routine">
        <p className="routine__summary">{routine.summary}</p>
        <p className="routines__error">This routine could not be read: {preview.failed}</p>
        <div className="routine__actions">
          <button onClick={onDismiss}>Dismiss</button>
        </div>
      </li>
    )
  }

  const blocked = preview.errors.length > 0
  // Older previews (and any server that has not caught up) carry only
  // `requires`; treating a missing `unmet` as "all of them" keeps the panel
  // honest rather than silently claiming everything is ready.
  const unmet = preview.unmet ?? preview.requires
  // Formatting counts: a routine that only bolds a row still does something,
  // and calling that "no change" would disable Run on a routine that works.
  const all = [...preview.changes, ...preview.format_changes]
  const empty = all.length === 0
  const shown = all.slice(0, MAX_PREVIEW_ROWS)
  const hidden = all.length - shown.length

  return (
    <li className="routine">
      <p className="routine__summary">{routine.summary}</p>
      <p className="routine__meta">
        seen {routine.support}× · saves about{' '}
        {routine.estimated_minutes_saved.toFixed(1)} min
        {routine.status === 'accepted' && ' · used before'}
      </p>

      {unmet.length > 0 && (
        <p className="routine__partial">
          Runs everything except {unmet.length}{' '}
          {unmet.length === 1 ? 'value' : 'values'} it cannot know:{' '}
          {unmet
            .map(
              (q) =>
                `${q.kind} at ${a1({
                  row: anchor.row + q.row_offset,
                  col: anchor.col + q.col_offset,
                })}`,
            )
            .join(', ')}
          . You will still need to type {unmet.length === 1 ? 'it' : 'them'}.
        </p>
      )}

      {unmet.length === 0 && preview.requires.length > 0 && (
        <p className="routine__partial routine__partial--met">
          The {preview.requires.length} values this routine cannot supply are
          already filled in here.
        </p>
      )}

      {blocked && (
        <p className="routines__error">
          Cannot run here: {preview.errors.join('; ')}
        </p>
      )}

      {!blocked && empty && (
        <p className="muted">Nothing would change at {a1(anchor)}.</p>
      )}

      {!blocked && !empty && (
        <>
          <button
            className="linkish"
            aria-expanded={open}
            onClick={() => setOpen((v) => !v)}
          >
            {open ? 'Hide' : 'Preview'} {all.length}{' '}
            {all.length === 1 ? 'change' : 'changes'}
          </button>
          {open && (
            <table className="routine__diff">
              <thead>
                <tr>
                  <th>Cell</th>
                  <th>Now</th>
                  <th>After</th>
                </tr>
              </thead>
              <tbody>
                {/* The index is part of the key because one cell can appear
                    twice — once for its value and once for its formatting. */}
                {shown.map((c, i) => (
                  <tr key={`${c.sheet}!${c.addr}:${i}`}>
                    <th scope="row">{c.addr}</th>
                    <td className="routine__before">{c.before || '—'}</td>
                    <td className="routine__after">{c.after || '—'}</td>
                  </tr>
                ))}
                {hidden > 0 && (
                  <tr>
                    <td colSpan={3} className="muted">
                      …and {hidden} more
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          )}
        </>
      )}

      <div className="routine__actions">
        <button onClick={onDismiss}>Dismiss</button>
        <button
          className="primary"
          disabled={blocked || empty}
          title={
            blocked
              ? 'The engine would refuse this here'
              : empty
                ? 'Nothing would change at this selection'
                : `Apply at ${a1(anchor)}`
          }
          onClick={onRun}
        >
          Run here
        </button>
      </div>
    </li>
  )
}
