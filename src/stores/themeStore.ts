/**
 * Theme Store for OpenAlgo Desktop
 *
 * Manages light/dark mode, theme colors, and app mode (live/analyzer).
 * App mode is persisted via Tauri settings for desktop.
 */

import { create } from 'zustand'
import { persist } from 'zustand/middleware'
import { settingsCommands } from '@/api/client'

export type ThemeMode = 'light' | 'dark'
export type AppMode = 'live' | 'analyzer' | 'sandbox'
export type ThemeColor =
  | 'zinc'
  | 'slate'
  | 'stone'
  | 'gray'
  | 'neutral'
  | 'red'
  | 'rose'
  | 'orange'
  | 'green'
  | 'blue'
  | 'yellow'
  | 'violet'

// Event emitter for mode changes
type ModeChangeCallback = (newMode: AppMode) => void
const modeChangeListeners: Set<ModeChangeCallback> = new Set()

export const onModeChange = (callback: ModeChangeCallback): (() => void) => {
  modeChangeListeners.add(callback)
  return () => {
    modeChangeListeners.delete(callback)
  }
}

const notifyModeChange = (newMode: AppMode) => {
  modeChangeListeners.forEach((cb) => cb(newMode))
}

interface ThemeStore {
  mode: ThemeMode
  color: ThemeColor
  appMode: AppMode
  isTogglingMode: boolean

  setMode: (mode: ThemeMode) => void
  setColor: (color: ThemeColor) => void
  setAppMode: (appMode: AppMode) => void
  toggleMode: () => void
  toggleAppMode: () => Promise<{ success: boolean; message?: string }>
  syncAppMode: () => Promise<void>
}

export const useThemeStore = create<ThemeStore>()(
  persist(
    (set, get) => ({
      mode: 'light',
      color: 'zinc',
      appMode: 'live',
      isTogglingMode: false,

      setMode: (mode) => {
        // Only allow theme change in live mode
        if (get().appMode !== 'live') return

        set({ mode })
        if (typeof document !== 'undefined') {
          document.documentElement.classList.toggle('dark', mode === 'dark')
        }

        // Persist to Tauri settings
        settingsCommands.updateSettings({ theme: mode }).catch(console.error)
      },

      setColor: (color) => {
        // Only allow color change in live mode
        if (get().appMode !== 'live') return

        set({ color })
        if (typeof document !== 'undefined') {
          document.documentElement.setAttribute('data-theme', color)
        }
      },

      setAppMode: (appMode) => {
        const previousMode = get().appMode
        set({ appMode })
        if (typeof document !== 'undefined') {
          // Remove all mode classes first
          document.documentElement.classList.remove('analyzer', 'sandbox', 'dark')

          if (appMode === 'live') {
            // Restore the saved light/dark mode when returning to live
            const savedMode = get().mode
            document.documentElement.classList.toggle('dark', savedMode === 'dark')
          } else if (appMode === 'analyzer') {
            // Analyzer mode uses its own dark purple theme (like dracula)
            document.documentElement.classList.add('analyzer')
          } else if (appMode === 'sandbox') {
            // Sandbox mode uses amber/yellow theme
            document.documentElement.classList.add('sandbox')
          }
        }
        // Notify listeners if mode changed
        if (previousMode !== appMode) {
          notifyModeChange(appMode)
        }
      },

      toggleMode: () => {
        // Only allow toggle in live mode
        if (get().appMode !== 'live') return

        const newMode = get().mode === 'light' ? 'dark' : 'light'
        set({ mode: newMode })
        if (typeof document !== 'undefined') {
          document.documentElement.classList.toggle('dark', newMode === 'dark')
        }

        // Persist to Tauri settings
        settingsCommands.updateSettings({ theme: newMode }).catch(console.error)
      },

      // Toggle app mode via Tauri settings
      toggleAppMode: async (): Promise<{ success: boolean; message?: string }> => {
        if (get().isTogglingMode) return { success: false, message: 'Already toggling' }

        set({ isTogglingMode: true })
        try {
          const currentMode = get().appMode
          let newMode: AppMode

          // Cycle through modes: live -> analyzer -> live
          // (sandbox is accessed separately via sandbox page)
          if (currentMode === 'live') {
            newMode = 'analyzer'
          } else {
            newMode = 'live'
          }

          get().setAppMode(newMode)

          const modeMessage =
            newMode === 'analyzer'
              ? 'Switched to Analyzer mode. All trades will be paper traded.'
              : 'Switched to Live mode. All trades will be executed with real money.'

          return { success: true, message: modeMessage }
        } catch (error) {
          console.error('Failed to toggle app mode:', error)
          return { success: false, message: 'Failed to toggle mode' }
        } finally {
          set({ isTogglingMode: false })
        }
      },

      // Sync app mode from Tauri settings
      syncAppMode: async () => {
        try {
          const settings = await settingsCommands.getSettings()

          // Theme sync - apply saved theme from settings
          if (settings.theme) {
            const savedMode = settings.theme as ThemeMode
            if (savedMode === 'light' || savedMode === 'dark') {
              set({ mode: savedMode })
              if (typeof document !== 'undefined' && get().appMode === 'live') {
                document.documentElement.classList.toggle('dark', savedMode === 'dark')
              }
            }
          }
        } catch (error) {
          console.error('Failed to sync app mode:', error)
        }
      },
    }),
    {
      name: 'openalgo-desktop-theme',
      partialize: (state) => ({
        mode: state.mode,
        color: state.color,
        appMode: state.appMode,
      }),
      onRehydrateStorage: () => (state) => {
        // Apply theme on rehydration
        if (state && typeof document !== 'undefined') {
          document.documentElement.classList.remove('analyzer', 'sandbox', 'dark')

          if (state.appMode === 'live') {
            document.documentElement.classList.toggle('dark', state.mode === 'dark')
          } else if (state.appMode === 'analyzer') {
            document.documentElement.classList.add('analyzer')
          } else if (state.appMode === 'sandbox') {
            document.documentElement.classList.add('sandbox')
          }

          document.documentElement.setAttribute('data-theme', state.color)
        }
      },
    }
  )
)
