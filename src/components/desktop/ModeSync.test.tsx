/**
 * UI-01: the Live/Analyze mode on screen follows the server's, on every page
 * the layouts render. An SDK script or an AI client can switch the server
 * while the window shows the other mode; the page takes the announced mode
 * at once, and reads it again on a reconnect and when the window is shown.
 */
import { act, render } from '@testing-library/react'
import type { ReactNode } from 'react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { FullWidthLayout } from '@/components/layout/FullWidthLayout'
import { Layout } from '@/components/layout/Layout'
import { useThemeStore } from '@/stores/themeStore'

const socket = vi.hoisted(() => {
  const handlers = new Map<string, (payload?: unknown) => void>()
  return {
    handlers,
    on: vi.fn((event: string, fn: (payload?: unknown) => void) => handlers.set(event, fn)),
    off: vi.fn((event: string) => handlers.delete(event)),
    emit(event: string, payload?: unknown) {
      handlers.get(event)?.(payload)
    },
  }
})
vi.mock('@/components/socket/SocketProvider', () => ({
  SocketProvider: ({ children }: { children: ReactNode }) => children,
  useSocketContext: () => ({ socket, playAlertSound: () => {} }),
}))
vi.mock('@/stores/authStore', () => ({
  useAuthStore: () => ({
    isAuthenticated: true,
    user: { username: 'trader', broker: 'zerodha' },
  }),
}))
vi.mock('@/components/layout/Navbar', () => ({ Navbar: () => null }))
vi.mock('@/components/layout/Footer', () => ({ Footer: () => null }))
vi.mock('@/components/layout/MobileBottomNav', () => ({ MobileBottomNav: () => null }))
vi.mock('@/components/desktop/UpdateNotice', () => ({ UpdateNotice: () => null }))

function serverMode(analyze: boolean) {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => ({
      ok: true,
      json: async () => ({
        status: 'success',
        data: { mode: analyze ? 'analyze' : 'live', analyze_mode: analyze },
      }),
    }))
  )
}

const layouts = [
  ['Layout', <Layout key="l" />],
  ['FullWidthLayout', <FullWidthLayout key="f" />],
] as const

function mount(layout: ReactNode) {
  return render(
    <MemoryRouter initialEntries={['/scalping']}>
      <Routes>
        <Route element={layout}>
          <Route path="/scalping" element={<p>page</p>} />
        </Route>
      </Routes>
    </MemoryRouter>
  )
}

const appMode = () => useThemeStore.getState().appMode

describe.each(layouts)('%s keeps the mode equal to the server', (_name, layout) => {
  beforeEach(() => {
    socket.handlers.clear()
    useThemeStore.setState({ appMode: 'analyzer' })
  })
  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it('takes the mode the server announces', () => {
    mount(layout)
    act(() => socket.emit('analyzer_mode_changed', { analyze_mode: false, mode: 'live' }))
    expect(appMode()).toBe('live')
    act(() => socket.emit('analyzer_mode_changed', { analyze_mode: true, mode: 'analyze' }))
    expect(appMode()).toBe('analyzer')
  })

  it('reads the mode again when the connection comes back', async () => {
    serverMode(false)
    mount(layout)
    await act(async () => socket.emit('connect'))
    expect(fetch).toHaveBeenCalledWith('/auth/analyzer-mode', expect.anything())
    expect(appMode()).toBe('live')
  })

  it('reads the mode again when the window is shown', async () => {
    serverMode(false)
    mount(layout)
    await act(async () => {
      window.dispatchEvent(new Event('focus'))
    })
    expect(appMode()).toBe('live')
  })

  it('stops listening when the layout goes away', () => {
    const { unmount } = mount(layout)
    unmount()
    expect(socket.handlers.has('analyzer_mode_changed')).toBe(false)
    expect(socket.handlers.has('connect')).toBe(false)
  })
})
