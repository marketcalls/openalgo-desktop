import { useEffect } from 'react'
import { useSocketContext } from '@/components/socket/SocketProvider'
import { useThemeStore } from '@/stores/themeStore'

/** Desktop Socket.IO event: the analyzer mode the server now stores. */
export const MODE_CHANGED_EVENT = 'analyzer_mode_changed'

/**
 * Desktop departure from the web (UI-01), to be offered upstream. The web
 * reads the Live/Analyze mode once when the app starts and keeps a stored
 * copy. On the desktop an SDK script, an AI client (MCP) or Telegram can
 * switch the server while a page is open, so a page could show Sandbox while
 * the next order went Live. The page therefore follows the server: it takes
 * the mode the server announces on every change (`analyzer_mode_changed`,
 * desktop only; the web emits nothing), and reads it again whenever an
 * announcement could have been missed: when the Socket.IO connection comes
 * back and when the window is shown again. Mounted by the layouts, inside
 * SocketProvider; themeStore stays identical to the web's.
 */
export function ModeSync() {
  const { socket } = useSocketContext()
  const setAppMode = useThemeStore((s) => s.setAppMode)
  const syncAppMode = useThemeStore((s) => s.syncAppMode)

  useEffect(() => {
    const resync = () => {
      void syncAppMode()
    }
    const onChanged = (payload: { analyze_mode?: unknown } | undefined) => {
      if (typeof payload?.analyze_mode === 'boolean') {
        setAppMode(payload.analyze_mode ? 'analyzer' : 'live')
      } else {
        resync()
      }
    }
    const onVisible = () => {
      if (document.visibilityState === 'visible') resync()
    }
    socket?.on(MODE_CHANGED_EVENT, onChanged)
    socket?.on('connect', resync)
    window.addEventListener('focus', resync)
    document.addEventListener('visibilitychange', onVisible)
    return () => {
      socket?.off(MODE_CHANGED_EVENT, onChanged)
      socket?.off('connect', resync)
      window.removeEventListener('focus', resync)
      document.removeEventListener('visibilitychange', onVisible)
    }
  }, [socket, setAppMode, syncAppMode])

  return null
}
