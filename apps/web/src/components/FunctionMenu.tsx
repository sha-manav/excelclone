/**
 * The function-name menu, and the keyboard behaviour that goes with it.
 *
 * Both editing surfaces need this — the cell editor and the formula bar edit
 * the same value and a user typing `=AVE` in either one expects the same help
 * — so the state, the key handling and the list live together here and each
 * surface supplies only its own anchor.
 *
 * The hook returns a `handleKeyDown` that reports whether it consumed the key,
 * rather than taking over the event. Enter, Tab and Escape all mean something
 * to the editor underneath, and which one wins depends on whether the menu is
 * open — so the caller has to stay in charge of the ones the menu declines.
 */

import { useCallback, useMemo, useState } from 'react'
import type { JSX, KeyboardEvent as ReactKeyboardEvent, RefObject } from 'react'
import { acceptCompletion, completionAt, describe } from './formula-complete'
import type { Completion } from './formula-complete'
import { signature } from './function-help'

export interface CompletionApi {
  completion: Completion | null
  /** Index into `completion.names` the keyboard is on. */
  active: number
  setActive(index: number): void
  /** Re-read the value and caret; call after anything that moves either. */
  refresh(value: string, caret: number): void
  /** Put the highlighted name into the formula. */
  accept(index?: number): void
  close(): void
  /** True when the key belonged to the menu and the caller should stop. */
  handleKeyDown(e: ReactKeyboardEvent<HTMLInputElement>): boolean
}

export function useCompletion(
  inputRef: RefObject<HTMLInputElement | null>,
  names: readonly string[],
  onChange: (value: string) => void,
): CompletionApi {
  const [completion, setCompletion] = useState<Completion | null>(null)
  const [active, setActive] = useState(0)

  const close = useCallback(() => setCompletion(null), [])

  const refresh = useCallback(
    (value: string, caret: number) => {
      const next = completionAt(value, caret, names)
      setCompletion(next)
      // The highlight goes back to the top whenever the list changes, because
      // holding position by index would mean "the second match" jumping to a
      // different function as the user types.
      setActive(0)
    },
    [names],
  )

  const accept = useCallback(
    (index?: number) => {
      const input = inputRef.current
      if (!completion || !input) return
      const name = completion.names[index ?? active]
      if (!name) return
      const next = acceptCompletion(input.value, completion, name)
      setCompletion(null)
      onChange(next.value)
      // The value has not reached the DOM yet, so write it here as well: this
      // is the same caret problem pointing has, and the same fix — without it
      // the caret lands after the closing text rather than inside the call.
      input.value = next.value
      input.setSelectionRange(next.caret, next.caret)
      input.focus()
    },
    [active, completion, inputRef, onChange],
  )

  const handleKeyDown = useCallback(
    (e: ReactKeyboardEvent<HTMLInputElement>): boolean => {
      if (!completion) return false
      switch (e.key) {
        case 'ArrowDown':
          e.preventDefault()
          setActive((i) => (i + 1) % completion.names.length)
          return true
        case 'ArrowUp':
          e.preventDefault()
          setActive((i) => (i - 1 + completion.names.length) % completion.names.length)
          return true
        case 'Tab':
        case 'Enter':
          e.preventDefault()
          accept()
          return true
        case 'Escape':
          // Dismisses the menu and nothing else. Escaping out of the whole
          // edit takes a second press, which is what Excel does and what
          // anyone who has ever dismissed a menu by reflex expects.
          e.preventDefault()
          setCompletion(null)
          return true
        default:
          return false
      }
    },
    [accept, completion],
  )

  return useMemo(
    () => ({ completion, active, setActive, refresh, accept, close, handleKeyDown }),
    [accept, active, close, completion, handleKeyDown, refresh],
  )
}

export interface FunctionMenuProps {
  api: CompletionApi
  /** Absolute placement within the nearest positioned ancestor. */
  style: { left: number | string; top: number | string; minWidth?: number }
}

export function FunctionMenu({ api, style }: FunctionMenuProps): JSX.Element | null {
  const { completion, active } = api
  if (!completion) return null
  return (
    <ul
      className="fn-menu"
      data-testid="function-menu"
      // The menu is a hint, not a control: taking focus would end the edit it
      // exists to help with, so the press is swallowed before it can.
      onMouseDown={(e) => e.preventDefault()}
      style={{ ...style, position: 'absolute' }}
    >
      {completion.names.map((name, i) => (
        <li
          key={name}
          className={i === active ? 'fn-menu__item fn-menu__item--active' : 'fn-menu__item'}
          data-testid={`function-menu-item-${name}`}
          onMouseEnter={() => api.setActive(i)}
          onClick={() => api.accept(i)}
        >
          <span className="fn-menu__sig">{signature(name)}</span>
          <span className="fn-menu__about">{describe(name)}</span>
        </li>
      ))}
    </ul>
  )
}
