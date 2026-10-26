import { act, renderHook, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { useHostServer } from './useHostServer'

// docs-37: the desktop's pages are served from 127.0.0.1, so a webhook
// address built from window.location.origin is useless to TradingView. The
// address comes from the Host Server URL (or the tunnel's while online).

afterEach(() => vi.unstubAllGlobals())

function serve(response: Response | Promise<Response>) {
  const fetchMock = vi.fn(async () => response)
  vi.stubGlobal('fetch', fetchMock)
  return fetchMock
}

describe('useHostServer', () => {
  it('answers the Host Server URL, not the page address', async () => {
    const fetchMock = serve(
      new Response(
        JSON.stringify({ host_server: 'https://desk.ngrok-free.app/', is_localhost: false })
      )
    )
    const { result } = renderHook(() => useHostServer())
    expect(result.current).toBe(window.location.origin)
    await waitFor(() => expect(result.current).toBe('https://desk.ngrok-free.app'))
    expect(fetchMock).toHaveBeenCalledWith('/api/config/host', { credentials: 'include' })
  })

  it('keeps the page address when the setting cannot be read', async () => {
    const fetchMock = serve(new Response('{}', { status: 500 }))
    const { result } = renderHook(() => useHostServer())
    await waitFor(() => expect(fetchMock).toHaveBeenCalled())
    // Let the answer's promise chain settle (microtasks only).
    await act(async () => {
      await fetchMock.mock.results[0]?.value
    })
    expect(result.current).toBe(window.location.origin)
  })
})
