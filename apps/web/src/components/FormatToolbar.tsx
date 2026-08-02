/**
 * The formatting strip.
 *
 * Every control emits a `format_apply` action carrying one or more patches;
 * nothing here holds formatting state of its own. The pressed states come
 * from the engine's view of the anchor cell, so the toolbar can only ever be
 * out of date by one render rather than drifting from the truth.
 *
 * Toggles read the anchor and send the opposite. That is Excel's rule too: a
 * mixed selection whose anchor is bold un-bolds, rather than every cell
 * flipping independently.
 */

import { useState } from 'react'
import type { JSX } from 'react'
import type { CellFormat } from '../engine/bridge'
import type { BorderPreset, FormatPatch, HAlign } from '../engine/actions'

export interface FormatToolbarProps {
  /** The anchor cell's format, which drives the pressed states. */
  active: CellFormat
  /** True when the selection covers more than one cell. */
  hasRange: boolean
  merged: boolean
  onPatch(patches: FormatPatch[]): void
  onClearFormatting(): void
  onMergeToggle(): void
}

/** Number formats offered by name, so the user never types a format code. */
export const NUMBER_FORMATS: ReadonlyArray<{ label: string; code: string | null }> = [
  { label: 'General', code: null },
  { label: 'Number', code: '#,##0.00' },
  { label: 'Currency', code: '$#,##0.00' },
  { label: 'Percent', code: '0.00%' },
  { label: 'Date', code: 'yyyy-mm-dd' },
  { label: 'Text', code: '@' },
]

/** A small, deliberately boring palette; the picker also takes any hex. */
const SWATCHES = [
  '#1a1a1a',
  '#b3261e',
  '#c77700',
  '#1e7e45',
  '#1a5fb4',
  '#7048a8',
  '#6b6b6b',
  '#ffffff',
]

export function FormatToolbar(props: FormatToolbarProps): JSX.Element {
  const { active, hasRange, merged, onPatch, onClearFormatting, onMergeToggle } = props
  const [open, setOpen] = useState<'font' | 'fill' | null>(null)

  const align = (value: HAlign) =>
    onPatch([{ set: 'align', value: active.align === value ? null : value }])

  const border = (value: BorderPreset) => onPatch([{ set: 'border', value }])

  return (
    <div className="toolbar__group format-toolbar" data-testid="format-toolbar">
      <button
        aria-label="Bold"
        aria-pressed={!!active.bold}
        className={active.bold ? 'is-on' : undefined}
        title="Bold"
        onClick={() => onPatch([{ set: 'bold', value: !active.bold }])}
      >
        <b>B</b>
      </button>
      <button
        aria-label="Italic"
        aria-pressed={!!active.italic}
        className={active.italic ? 'is-on' : undefined}
        title="Italic"
        onClick={() => onPatch([{ set: 'italic', value: !active.italic }])}
      >
        <i>I</i>
      </button>

      <ColorButton
        label="Text colour"
        glyph="A"
        swatch={active.font_color}
        isOpen={open === 'font'}
        onToggle={() => setOpen(open === 'font' ? null : 'font')}
        onPick={(value) => {
          setOpen(null)
          onPatch([{ set: 'font_color', value }])
        }}
      />
      <ColorButton
        label="Fill colour"
        glyph="▦"
        swatch={active.fill_color}
        isOpen={open === 'fill'}
        onToggle={() => setOpen(open === 'fill' ? null : 'fill')}
        onPick={(value) => {
          setOpen(null)
          onPatch([{ set: 'fill_color', value }])
        }}
      />

      <select
        aria-label="Number format"
        value={active.number_format ?? ''}
        onChange={(e) => {
          const code = e.target.value === '' ? null : e.target.value
          onPatch([{ set: 'number_format', value: code }])
        }}
      >
        {NUMBER_FORMATS.map((f) => (
          <option key={f.label} value={f.code ?? ''}>
            {f.label}
          </option>
        ))}
        {/* A code from an imported workbook that is not one of ours still has
            to be shown, or the dropdown would silently claim "General". */}
        {active.number_format &&
          !NUMBER_FORMATS.some((f) => f.code === active.number_format) && (
            <option value={active.number_format}>{active.number_format}</option>
          )}
      </select>

      <div className="toolbar__seg" role="group" aria-label="Alignment">
        {(['left', 'center', 'right'] as const).map((a) => (
          <button
            key={a}
            aria-label={`Align ${a}`}
            aria-pressed={active.align === a}
            className={active.align === a ? 'is-on' : undefined}
            title={`Align ${a}`}
            onClick={() => align(a)}
          >
            {a === 'left' ? '⯇' : a === 'center' ? '≡' : '⯈'}
          </button>
        ))}
      </div>

      <select
        aria-label="Borders"
        value=""
        onChange={(e) => {
          if (e.target.value) border(e.target.value as BorderPreset)
          e.target.value = ''
        }}
      >
        <option value="">Borders…</option>
        <option value="outline">Outline</option>
        <option value="all">All</option>
        <option value="none">None</option>
      </select>

      <button
        aria-label={merged ? 'Unmerge cells' : 'Merge cells'}
        aria-pressed={merged}
        className={merged ? 'is-on' : undefined}
        disabled={!merged && !hasRange}
        title={merged ? 'Unmerge cells' : 'Merge the selected cells'}
        onClick={onMergeToggle}
      >
        Merge
      </button>
      <button
        aria-label="Clear formatting"
        title="Clear formatting, keeping the contents"
        onClick={onClearFormatting}
      >
        Clear
      </button>
    </div>
  )
}

interface ColorButtonProps {
  label: string
  glyph: string
  swatch?: string
  isOpen: boolean
  onToggle(): void
  onPick(value: string | null): void
}

function ColorButton(props: ColorButtonProps): JSX.Element {
  const { label, glyph, swatch, isOpen, onToggle, onPick } = props
  return (
    <span className="color-button">
      <button aria-label={label} aria-expanded={isOpen} title={label} onClick={onToggle}>
        <span className="color-button__glyph">{glyph}</span>
        <span
          className="color-button__swatch"
          style={{ background: swatch ?? 'transparent' }}
        />
      </button>
      {isOpen && (
        <div className="color-menu" role="menu" aria-label={`${label} choices`}>
          {SWATCHES.map((c) => (
            <button
              key={c}
              aria-label={c}
              className="color-menu__swatch"
              style={{ background: c }}
              onClick={() => onPick(c)}
            />
          ))}
          <button className="color-menu__none" onClick={() => onPick(null)}>
            None
          </button>
        </div>
      )}
    </span>
  )
}
