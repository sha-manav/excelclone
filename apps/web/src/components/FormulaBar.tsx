/**
 * Formula bar: shows the active cell's address and its input text, with
 * light syntax highlighting of references while editing.
 */

import { useEffect, useMemo, useRef, useState } from 'react'
import { a1, type Addr } from '../engine/actions'
import { FunctionMenu, useCompletion } from './FunctionMenu'

export interface FormulaBarProps {
  active: Addr
  /** Text to show when not editing (the cell's stored input). */
  cellInput: string
  editing: boolean
  editValue: string
  /** Function names to complete against, from the engine. */
  functionNames: readonly string[]
  onChange(value: string): void
  onCommit(): void
  onCancel(): void
  onBeginEdit(): void
  /**
   * The name box was submitted. Excel's rule: an address or an existing name
   * navigates, anything else defines a new name over the current selection.
   * The caller decides which, because only it knows what names exist.
   */
  onNameBox(text: string): void
}

/** Reference-like tokens, for highlighting. Deliberately permissive: the
 *  engine is the authority on what parses, this is only a reading aid. */
const REF_PATTERN =
  /((?:'[^']+'|[A-Za-z_][A-Za-z0-9_.]*)!)?\$?[A-Za-z]{1,3}\$?[0-9]{1,7}(?::\$?[A-Za-z]{1,3}\$?[0-9]{1,7})?/g

interface Token {
  text: string
  kind: 'ref' | 'string' | 'plain'
}

export function tokenizeFormula(src: string): Token[] {
  if (!src.startsWith('=')) return [{ text: src, kind: 'plain' }]
  const tokens: Token[] = []
  let i = 0
  let plainStart = 0

  const pushPlain = (end: number) => {
    if (end > plainStart) {
      // Highlight references inside the plain run.
      const chunk = src.slice(plainStart, end)
      let last = 0
      for (const m of chunk.matchAll(REF_PATTERN)) {
        const at = m.index ?? 0
        if (at > last) tokens.push({ text: chunk.slice(last, at), kind: 'plain' })
        tokens.push({ text: m[0], kind: 'ref' })
        last = at + m[0].length
      }
      if (last < chunk.length) tokens.push({ text: chunk.slice(last), kind: 'plain' })
    }
  }

  while (i < src.length) {
    if (src[i] === '"') {
      pushPlain(i)
      let j = i + 1
      while (j < src.length) {
        if (src[j] === '"') {
          if (src[j + 1] === '"') j += 2
          else break
        } else j += 1
      }
      tokens.push({ text: src.slice(i, Math.min(j + 1, src.length)), kind: 'string' })
      i = j + 1
      plainStart = i
    } else {
      i += 1
    }
  }
  pushPlain(src.length)
  return tokens
}

export function FormulaBar({
  active,
  cellInput,
  editing,
  editValue,
  functionNames,
  onChange,
  onCommit,
  onCancel,
  onBeginEdit,
  onNameBox,
}: FormulaBarProps) {
  const inputRef = useRef<HTMLInputElement>(null)
  const completion = useCompletion(inputRef, functionNames, onChange)
  // The name box shows the address until the user types in it, and goes back
  // to showing the address as soon as the selection moves — otherwise a
  // half-typed name would sit there looking like where you are.
  const [nameBox, setNameBox] = useState<string | null>(null)
  const address = a1(active)
  useEffect(() => setNameBox(null), [address])
  const shown = editing ? editValue : cellInput
  const tokens = useMemo(() => tokenizeFormula(shown), [shown])

  return (
    <div className="formula-bar">
      <input
        className="formula-bar__address"
        aria-label="Name box"
        data-testid="name-box"
        value={nameBox ?? address}
        onChange={(e) => setNameBox(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter') {
            e.preventDefault()
            const text = (nameBox ?? '').trim()
            setNameBox(null)
            if (text) onNameBox(text)
          }
          if (e.key === 'Escape') setNameBox(null)
        }}
        onBlur={() => setNameBox(null)}
      />
      <div className="formula-bar__divider" />
      <div className="formula-bar__field">
        {/* The highlight layer sits behind a transparent input so the caret
            and selection stay native while the text is coloured. */}
        <div className="formula-bar__highlight" aria-hidden="true">
          {tokens.map((t, i) => (
            <span key={i} className={`tok tok--${t.kind}`}>
              {t.text}
            </span>
          ))}
        </div>
        <input
          ref={inputRef}
          className="formula-bar__input"
          value={shown}
          spellCheck={false}
          aria-label="Formula"
          onFocus={() => {
            if (!editing) onBeginEdit()
          }}
          onChange={(e) => {
            onChange(e.target.value)
            completion.refresh(e.target.value, e.target.selectionStart ?? e.target.value.length)
          }}
          onSelect={(e) => {
            const el = e.currentTarget
            completion.refresh(el.value, el.selectionStart ?? el.value.length)
          }}
          onBlur={completion.close}
          onKeyDown={(e) => {
            // While the menu is open Enter picks a function rather than
            // committing the cell, which is the whole point of the menu.
            if (completion.handleKeyDown(e)) return
            if (e.key === 'Enter') {
              e.preventDefault()
              onCommit()
            } else if (e.key === 'Escape') {
              e.preventDefault()
              onCancel()
            }
          }}
        />
        <FunctionMenu api={completion} style={{ left: 0, top: '100%', minWidth: 320 }} />
      </div>
    </div>
  )
}
