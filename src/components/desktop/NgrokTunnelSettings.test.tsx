import { act, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { TunnelSettings } from '@/api/tunnel'
import { NgrokTunnelSettings, TUNNEL_STATUS_REFRESH_MS } from './NgrokTunnelSettings'

const api = vi.hoisted(() => ({ get: vi.fn(), save: vi.fn() }))
vi.mock('@/api/tunnel', () => ({ tunnelApi: api }))

const toast = vi.hoisted(() => ({ success: vi.fn(), error: vi.fn() }))
vi.mock('@/utils/toast', () => ({ showToast: toast }))

function settings(over: Partial<TunnelSettings> = {}): TunnelSettings {
  return {
    enabled: true,
    authtoken_saved: true,
    domain: 'desk.ngrok-free.app',
    tunnel: {
      state: 'online',
      url: 'https://desk.ngrok-free.app',
      message: 'Online at https://desk.ngrok-free.app',
    },
    ...over,
  }
}

beforeEach(() => {
  api.get.mockResolvedValue(settings())
})

afterEach(() => {
  vi.useRealTimers()
  vi.clearAllMocks()
})

describe('NgrokTunnelSettings', () => {
  it('shows the status in words and never the saved authtoken', async () => {
    render(<NgrokTunnelSettings />)
    expect(await screen.findByTestId('tunnel-message')).toHaveTextContent(
      'Online at https://desk.ngrok-free.app'
    )
    expect(screen.getByTestId('tunnel-state')).toHaveTextContent('Online')
    const token = screen.getByLabelText('ngrok authtoken') as HTMLInputElement
    expect(token.value).toBe('')
    expect(token.type).toBe('password')
    expect(token.placeholder).toMatch(/^Saved\./)
    expect(screen.getByText('An authtoken is saved.')).toBeInTheDocument()
    expect((screen.getByLabelText('Reserved domain (optional)') as HTMLInputElement).value).toBe(
      'desk.ngrok-free.app'
    )
    expect(screen.getByText(/your broker's automatic redirect address/)).toBeInTheDocument()
  })

  it('sends only what the trader changed, then clears the authtoken field', async () => {
    api.save.mockResolvedValue(
      settings({ tunnel: { state: 'connecting', url: null, message: 'Connecting to ngrok.' } })
    )
    render(<NgrokTunnelSettings />)
    const save = await screen.findByRole('button', { name: 'Save authtoken and domain' })
    expect(save).toBeDisabled()
    const token = screen.getByLabelText('ngrok authtoken') as HTMLInputElement
    fireEvent.change(token, { target: { value: ' 2abcTOKEN_x ' } })
    fireEvent.click(save)
    await waitFor(() => expect(api.save).toHaveBeenCalledWith({ authtoken: '2abcTOKEN_x' }))
    await waitFor(() => expect(token.value).toBe(''))
    expect(screen.getByTestId('tunnel-message')).toHaveTextContent('Connecting to ngrok.')

    fireEvent.change(screen.getByLabelText('Reserved domain (optional)'), {
      target: { value: '' },
    })
    fireEvent.click(save)
    await waitFor(() => expect(api.save).toHaveBeenLastCalledWith({ domain: '' }))
  })

  it('removes the saved authtoken on request', async () => {
    api.save.mockResolvedValue(settings({ authtoken_saved: false }))
    render(<NgrokTunnelSettings />)
    fireEvent.click(await screen.findByRole('button', { name: 'Remove the saved authtoken' }))
    await waitFor(() => expect(api.save).toHaveBeenCalledWith({ forget_authtoken: true }))
    await waitFor(() => expect(screen.queryByText('An authtoken is saved.')).toBeNull())
  })

  it("tells the server's reason when a save is refused", async () => {
    api.save.mockRejectedValue(new Error('That does not look like an ngrok authtoken.'))
    render(<NgrokTunnelSettings />)
    fireEvent.change(await screen.findByLabelText('ngrok authtoken'), {
      target: { value: 'bad token' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Save authtoken and domain' }))
    await waitFor(() =>
      expect(toast.error).toHaveBeenCalledWith(
        'That does not look like an ngrok authtoken.',
        'admin'
      )
    )
  })

  it('reads the status again while open, and keeps the last one when a read fails', async () => {
    vi.useFakeTimers()
    api.get.mockResolvedValueOnce(
      settings({ tunnel: { state: 'connecting', url: null, message: 'Connecting to ngrok.' } })
    )
    render(<NgrokTunnelSettings />)
    await act(async () => {})
    expect(screen.getByTestId('tunnel-message')).toHaveTextContent('Connecting to ngrok.')
    await act(async () => {
      vi.advanceTimersByTime(TUNNEL_STATUS_REFRESH_MS)
    })
    expect(screen.getByTestId('tunnel-message')).toHaveTextContent('Online at')
    api.get.mockRejectedValueOnce(new Error('offline'))
    await act(async () => {
      vi.advanceTimersByTime(TUNNEL_STATUS_REFRESH_MS)
    })
    expect(screen.getByTestId('tunnel-message')).toHaveTextContent(
      'Online at https://desk.ngrok-free.app (Could not read the latest status'
    )
  })
})
