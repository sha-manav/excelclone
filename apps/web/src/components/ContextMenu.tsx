/**
 * The right-click menu.
 *
 * Items are data rather than markup so the same list can be asserted against
 * in tests, and so a disabled item can explain itself instead of silently
 * doing nothing.
 */

import { useEffect, useLayoutEffect, useRef, useState } from 'react'
import type { JSX } from 'react'

export interface MenuItem {
  /** A separator when `label` is absent. */
  label?: string
  onSelect?: () => void
  disabled?: boolean
  title?: string
}

export interface ContextMenuProps {
  x: number
  y: number
  items: MenuItem[]
  onClose(): void
}

export function ContextMenu(props: ContextMenuProps): JSX.Element {
  const { x, y, items, onClose } = props
  const ref = useRef<HTMLUListElement>(null)
  const [pos, setPos] = useState({ left: x, top: y })

  // Flip the menu back inside the window when it would open off the edge —
  // a menu whose bottom half is unreachable is worse than no menu.
  useLayoutEffect(() => {
    const el = ref.current
    if (!el) return
    const r = el.getBoundingClientRect()
    setPos({
      left: Math.max(4, Math.min(x, window.innerWidth - r.width - 4)),
      top: Math.max(4, Math.min(y, window.innerHeight - r.height - 4)),
    })
  }, [x, y, items.length])

  useEffect(() => {
    const close = () => onClose()
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose()
    }
    // `click` fires after the mouseup that opened us, so listen on the next
    // tick or the menu would close the instant it appeared.
    const id = window.setTimeout(() => {
      window.addEventListener('click', close)
      window.addEventListener('contextmenu', close)
    }, 0)
    window.addEventListener('keydown', onKey)
    return () => {
      window.clearTimeout(id)
      window.removeEventListener('click', close)
      window.removeEventListener('contextmenu', close)
      window.removeEventListener('keydown', onKey)
    }
  }, [onClose])

  return (
    <ul
      ref={ref}
      className="context-menu"
      role="menu"
      style={{ left: pos.left, top: pos.top }}
    >
      {items.map((item, i) =>
        item.label === undefined ? (
          <li key={i} className="context-menu__sep" role="separator" />
        ) : (
          <li
            key={i}
            role="menuitem"
            aria-disabled={item.disabled}
            className={item.disabled ? 'is-disabled' : undefined}
            title={item.title}
            onClick={() => {
              if (item.disabled) return
              item.onSelect?.()
              onClose()
            }}
          >
            {item.label}
          </li>
        ),
      )}
    </ul>
  )
}
