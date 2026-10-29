import { useCallback, useEffect } from 'react'
import { type NavigateOptions, type To, useNavigate } from 'react-router'
import { create } from 'zustand'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { useFlowWorkflowStore } from '@/stores/flowWorkflowStore'

/**
 * Desktop departure from the web's Flow editor (parity F-170, lead decision
 * 14): the web lets the back arrow, the ? key, the Dashboard link, the
 * profile menu and the Keyboard Shortcuts item leave the editor with unsaved
 * edits, and the store reset on unmount discards them without a word. Here a
 * trader with unsaved changes is asked first.
 *
 * The app uses a declarative router, where react-router's useBlocker does not
 * work, so the guard covers the ways the editor itself leaves: its navigate
 * calls (useGuardedNavigate), any in-app link clicked inside the window, and
 * a reload or close of the window (the browser's own prompt).
 */

interface PendingLeave {
  /** Where the trader asked to go, run once they choose to leave. */
  leave: (() => void) | null
  ask: (leave: () => void) => void
  clear: () => void
}

const usePendingLeave = create<PendingLeave>((set) => ({
  leave: null,
  ask: (leave) => set({ leave }),
  clear: () => set({ leave: null }),
}))

const unsaved = () => useFlowWorkflowStore.getState().isModified

/**
 * The editor's navigate, asking before it leaves unsaved changes. Signing out
 * is let through: it has its own confirmation, and the session is already
 * gone when it navigates.
 */
export function useGuardedNavigate() {
  const navigate = useNavigate()
  return useCallback(
    (to: To, options?: NavigateOptions) => {
      if (!unsaved() || to === '/login') {
        navigate(to, options)
        return
      }
      usePendingLeave.getState().ask(() => navigate(to, options))
    },
    [navigate]
  )
}

/** An in-app link's path, or null for anything that does not leave the page in place. */
function inAppTarget(event: MouseEvent): string | null {
  if (event.defaultPrevented || event.button !== 0) return null
  if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return null
  const anchor = (event.target as Element | null)?.closest?.('a[href]') as HTMLAnchorElement | null
  if (!anchor || (anchor.target && anchor.target !== '_self') || anchor.hasAttribute('download')) {
    return null
  }
  const url = new URL(anchor.href, window.location.href)
  if (url.origin !== window.location.origin) return null
  const here = window.location.pathname + window.location.search
  const there = url.pathname + url.search
  return there === here ? null : there + url.hash
}

/** The prompt, the link interception and the window-close prompt. Render once in the editor. */
export function FlowLeaveGuard() {
  const navigate = useNavigate()
  const leave = usePendingLeave((s) => s.leave)
  const isModified = useFlowWorkflowStore((s) => s.isModified)

  useEffect(() => {
    if (!isModified) return
    const onBeforeUnload = (event: BeforeUnloadEvent) => {
      event.preventDefault()
      // Chromium-based webviews still need returnValue set to show the prompt.
      event.returnValue = ''
    }
    // Capture, so it runs before the router's own link handler, which then
    // sees the click as handled and stays put.
    const onClick = (event: MouseEvent) => {
      const target = inAppTarget(event)
      if (!target || !unsaved()) return
      event.preventDefault()
      usePendingLeave.getState().ask(() => navigate(target))
    }
    window.addEventListener('beforeunload', onBeforeUnload)
    document.addEventListener('click', onClick, true)
    return () => {
      window.removeEventListener('beforeunload', onBeforeUnload)
      document.removeEventListener('click', onClick, true)
    }
  }, [isModified, navigate])

  // A prompt left open by an editor that has gone away must not fire later.
  useEffect(() => () => usePendingLeave.getState().clear(), [])

  return (
    <AlertDialog
      open={leave !== null}
      onOpenChange={(open) => {
        if (!open) usePendingLeave.getState().clear()
      }}
    >
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>Leave without saving?</AlertDialogTitle>
          <AlertDialogDescription>
            This workflow has changes that are not saved. If you leave the editor now, they are
            lost. Stay and press Save to keep them.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>Stay</AlertDialogCancel>
          <AlertDialogAction
            onClick={() => {
              const go = usePendingLeave.getState().leave
              usePendingLeave.getState().clear()
              go?.()
            }}
            className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
          >
            Leave without saving
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}
