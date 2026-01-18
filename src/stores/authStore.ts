/**
 * Auth Store for OpenAlgo Desktop
 *
 * Uses Zustand for state management with Tauri IPC for authentication.
 * Session state is primarily managed by the Rust backend.
 */

import { create } from 'zustand'
import { persist } from 'zustand/middleware'
import { authApi } from '../api/auth'
import type { BrokerCredentials } from '../api/client'

interface User {
  username: string
  broker: string | null
  brokerId: string | null
  isLoggedIn: boolean
  loginTime: string | null
}

interface AuthStore {
  user: User | null
  apiKey: string | null // Kept for compatibility with pages
  isAuthenticated: boolean
  brokerConnected: boolean
  isLoading: boolean
  error: string | null

  // Actions
  setUser: (user: User) => void
  setApiKey: (apiKey: string | null) => void
  login: (username: string, password: string) => Promise<boolean>
  logout: () => Promise<void>
  brokerLogin: (brokerId: string, credentials: BrokerCredentials) => Promise<boolean>
  brokerLogout: () => Promise<void>
  checkSession: () => Promise<boolean>
  refreshSession: () => Promise<void>
  clearError: () => void
}

export const useAuthStore = create<AuthStore>()(
  persist(
    (set, get) => ({
      user: null,
      apiKey: null,
      isAuthenticated: false,
      brokerConnected: false,
      isLoading: false,
      error: null,

      setUser: (user) =>
        set({
          user,
          isAuthenticated: user.isLoggedIn,
          brokerConnected: !!user.broker,
        }),

      setApiKey: (apiKey) => set({ apiKey }),

      login: async (username, password) => {
        set({ isLoading: true, error: null })
        try {
          const response = await authApi.login({ username, password })

          if (response.success) {
            const user: User = {
              username: response.username,
              broker: null,
              brokerId: null,
              isLoggedIn: true,
              loginTime: new Date().toISOString(),
            }
            set({
              user,
              isAuthenticated: true,
              isLoading: false,
            })
            return true
          } else {
            set({
              isLoading: false,
              error: 'Login failed',
            })
            return false
          }
        } catch (error) {
          set({
            isLoading: false,
            error: error instanceof Error ? error.message : 'Login failed',
          })
          return false
        }
      },

      logout: async () => {
        set({ isLoading: true })
        try {
          await authApi.logout()
        } catch (error) {
          console.error('Logout error:', error)
        } finally {
          set({
            user: null,
            apiKey: null,
            isAuthenticated: false,
            brokerConnected: false,
            isLoading: false,
            error: null,
          })
        }
      },

      brokerLogin: async (brokerId, credentials) => {
        set({ isLoading: true, error: null })
        try {
          const response = await authApi.brokerLogin(brokerId, credentials)

          if (response.success) {
            const currentUser = get().user
            if (currentUser) {
              set({
                user: {
                  ...currentUser,
                  broker: response.user_name || response.user_id,
                  brokerId: response.broker_id,
                },
                brokerConnected: true,
                isLoading: false,
              })
            }
            return true
          } else {
            set({
              isLoading: false,
              error: 'Broker login failed',
            })
            return false
          }
        } catch (error) {
          set({
            isLoading: false,
            error: error instanceof Error ? error.message : 'Broker login failed',
          })
          return false
        }
      },

      brokerLogout: async () => {
        set({ isLoading: true })
        try {
          await authApi.brokerLogout()
          const currentUser = get().user
          if (currentUser) {
            set({
              user: {
                ...currentUser,
                broker: null,
                brokerId: null,
              },
              brokerConnected: false,
              isLoading: false,
            })
          }
        } catch (error) {
          console.error('Broker logout error:', error)
          set({ isLoading: false })
        }
      },

      checkSession: async () => {
        try {
          const session = await authApi.getSession()

          if (session.authenticated && session.user) {
            const user: User = {
              username: session.user.username,
              broker: session.broker?.user_id || null,
              brokerId: session.broker?.broker_id || null,
              isLoggedIn: true,
              loginTime: session.user.authenticated_at,
            }
            set({
              user,
              isAuthenticated: true,
              brokerConnected: session.broker?.connected || false,
            })
            return true
          } else {
            // Session expired or not authenticated
            set({
              user: null,
              isAuthenticated: false,
              brokerConnected: false,
            })
            return false
          }
        } catch (error) {
          console.error('Session check error:', error)
          return false
        }
      },

      refreshSession: async () => {
        const isValid = await get().checkSession()
        if (!isValid) {
          // Session is invalid, clear local state
          set({
            user: null,
            isAuthenticated: false,
            brokerConnected: false,
          })
        }
      },

      clearError: () => set({ error: null }),
    }),
    {
      name: 'openalgo-desktop-auth',
      partialize: (state) => ({
        // Only persist minimal info - actual auth state is in Rust backend
        user: state.user,
        isAuthenticated: state.isAuthenticated,
        brokerConnected: state.brokerConnected,
      }),
    }
  )
)

// Hook to check session on app startup
export const initializeAuth = async () => {
  const store = useAuthStore.getState()
  await store.checkSession()
}

export default useAuthStore
