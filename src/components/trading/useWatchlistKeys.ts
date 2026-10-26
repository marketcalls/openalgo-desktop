import { type KeyboardEvent, type MouseEvent, useCallback, useRef } from 'react'

/**
 * Desktop (issue #198): Up and Down move through the /trading watchlist and
 * chart the row they land on, so a trader who clicked the first symbol can
 * walk the list without the mouse. The web's panel has no arrow keys; this
 * hook keeps the logic out of the web-carried WatchlistPanel, which only
 * spreads the handlers on its list and marks each row's chart button with
 * `data-watchlist-row` (its index).
 *
 * - Only a key pressed on a row moves: focus elsewhere in the panel, or in
 *   a field, keeps its own arrows. A modifier means something else.
 * - Focus moves to the next row's button, so the focus ring shows where the
 *   trader is and a screen reader reads its name ("Chart SBIN on NSE").
 * - The key is the list's: it does not scroll the panel, pan the chart or
 *   nudge a selected drawing, which also listen for arrows.
 * - A held key moves focus row by row and charts the row where it is let
 *   go, instead of loading every chart on the way.
 * - The macOS window (WebKit) does not focus a button on click, so a click
 *   on a row focuses it: the arrows then start from the row clicked.
 */
export const WATCHLIST_ROW = 'data-watchlist-row'

function rowOf(target: EventTarget): HTMLElement | null {
  return (target as Element).closest?.<HTMLElement>(`[${WATCHLIST_ROW}]`) ?? null
}

function step(key: string): number {
  if (key === 'ArrowDown') return 1
  if (key === 'ArrowUp') return -1
  return 0
}

export function useWatchlistKeys<T>(items: readonly T[], pick: (item: T) => void) {
  // A held key moved focus without charting; its release charts the row.
  const held = useRef(false)

  const onKeyDown = useCallback(
    (e: KeyboardEvent<HTMLElement>) => {
      const by = step(e.key)
      if (!by || e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return
      const row = rowOf(e.target)
      if (!row) return
      e.preventDefault()
      e.stopPropagation()
      const next = Number(row.getAttribute(WATCHLIST_ROW)) + by
      if (!(next >= 0 && next < items.length)) return
      const nextRow = e.currentTarget.querySelector<HTMLElement>(`[${WATCHLIST_ROW}="${next}"]`)
      if (!nextRow) return
      nextRow.focus()
      nextRow.scrollIntoView?.({ block: 'nearest' })
      if (e.repeat) {
        held.current = true
        return
      }
      pick(items[next])
    },
    [items, pick]
  )

  const onKeyUp = useCallback(
    (e: KeyboardEvent<HTMLElement>) => {
      if (!held.current || !step(e.key)) return
      held.current = false
      const row = rowOf(e.target)
      const item = row ? items[Number(row.getAttribute(WATCHLIST_ROW))] : undefined
      if (item !== undefined) pick(item)
    },
    [items, pick]
  )

  const onClick = useCallback((e: MouseEvent<HTMLElement>) => {
    const row = rowOf(e.target)
    if (row && row !== document.activeElement) row.focus({ preventScroll: true })
  }, [])

  return { onKeyDown, onKeyUp, onClick }
}
