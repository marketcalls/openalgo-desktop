import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen, waitFor } from '@/test/test-utils'

// Desktop: the server applies broker settings without a restart and says so
// (`restart_required`), so the Broker tab only asks for one when told to.

const client = vi.hoisted(() => ({
  get: vi.fn(),
  post: vi.fn(),
}))

vi.mock('@/api/client', () => ({ webClient: client }))

import Profile from './Profile'

const credentials = {
  broker_api_key: 'kiteap********',
  broker_api_secret: 'kite********',
  redirect_url: 'http://127.0.0.1:5000/zerodha/callback',
  current_broker: 'zerodha',
  valid_brokers: ['zerodha', 'angel'],
  client_id_brokers: [],
  client_id: null,
  ngrok_allow: false,
  host_server: 'http://127.0.0.1:5000',
  websocket_url: 'ws://127.0.0.1:8765',
  server_status: {
    flask: { host: '127.0.0.1', port: '5000' },
    websocket: { host: '127.0.0.1', port: '8765' },
    zmq: { host: '127.0.0.1', port: '5555' },
  },
}

beforeEach(() => {
  window.history.pushState({}, '', '/profile?tab=broker')
  client.get.mockImplementation(async (url: string) => {
    if (url === '/api/broker/credentials') return { data: { status: 'success', data: credentials } }
    if (url === '/auth/profile-data') {
      return { data: { status: 'success', data: { username: 'trader', smtp_settings: null } } }
    }
    return { data: { status: 'success', data: {} } }
  })
})

afterEach(() => {
  window.history.pushState({}, '', '/')
  vi.clearAllMocks()
})

async function saveServerConfiguration(restartRequired: boolean) {
  client.post.mockResolvedValue({
    data: { status: 'success', message: 'Saved', restart_required: restartRequired },
  })
  render(<Profile />)
  const ws = await screen.findByPlaceholderText('ws://127.0.0.1:8765')
  fireEvent.change(ws, { target: { value: 'ws://127.0.0.1:8766' } })
  fireEvent.click(screen.getByRole('button', { name: /save ngrok settings/i }))
  await waitFor(() => expect(client.post).toHaveBeenCalled())
}

describe('Profile broker tab and restarts', () => {
  it('says changes apply on save, not after a restart', async () => {
    render(<Profile />)
    expect(await screen.findByText(/changes apply as soon as you save/i)).toBeInTheDocument()
    expect(screen.queryByText(/require an application restart/i)).toBeNull()
  })

  it('shows no restart notice when the server does not ask for one', async () => {
    await saveServerConfiguration(false)
    await new Promise((r) => setTimeout(r, 50))
    expect(screen.queryByText('Restart Required')).toBeNull()
  })

  it('shows the restart notice when the server asks for one', async () => {
    await saveServerConfiguration(true)
    expect(await screen.findByText('Restart Required')).toBeInTheDocument()
  })
})

function serveCredentials(saved: Record<string, unknown>) {
  client.get.mockImplementation(async (url: string) => {
    if (url === '/api/broker/credentials') return { data: { status: 'success', data: saved } }
    if (url === '/auth/profile-data') {
      return { data: { status: 'success', data: { username: 'trader', smtp_settings: null } } }
    }
    return { data: { status: 'success', data: {} } }
  })
}

// CFG-01: the tunnel switch alone never saves the automatic addresses shown in
// the fields as fixed ones, and a saved address can go back to automatic.
describe('Profile server addresses', () => {
  it('sends only the tunnel switch when only the switch changed', async () => {
    client.post.mockResolvedValue({ data: { status: 'success', message: 'Saved' } })
    render(<Profile />)
    fireEvent.click(await screen.findByRole('checkbox', { name: /enable ngrok tunnel/i }))
    fireEvent.click(screen.getByRole('button', { name: /save ngrok settings/i }))
    await waitFor(() => expect(client.post).toHaveBeenCalled())
    expect(client.post).toHaveBeenCalledWith('/api/broker/credentials', { ngrok_allow: 'TRUE' })
  })

  it('goes back to the automatic address on request', async () => {
    serveCredentials({
      ...credentials,
      websocket_url: 'wss://x.ngrok.app/ws',
      websocket_url_is_default: false,
      host_server_is_default: true,
    })
    client.post.mockResolvedValue({ data: { status: 'success', message: 'Saved' } })
    render(<Profile />)
    fireEvent.click(
      await screen.findByRole('button', { name: 'Use the automatic WebSocket address' })
    )
    expect(
      screen.queryByRole('button', { name: 'Use the automatic host server address' })
    ).toBeNull()
    fireEvent.click(screen.getByRole('button', { name: /save ngrok settings/i }))
    await waitFor(() => expect(client.post).toHaveBeenCalled())
    expect(client.post).toHaveBeenCalledWith('/api/broker/credentials', {
      ngrok_allow: 'FALSE',
      websocket_url: '',
    })
  })
})

// FLOW-03: saving another broker as the active one ends the live session, so
// the trader confirms first.
describe('Profile broker switch', () => {
  afterEach(() => vi.unstubAllGlobals())

  async function chooseAngelAndSave() {
    render(<Profile />)
    await screen.findByText(/changes apply as soon as you save/i)
    const select = screen.getAllByRole('combobox').find((c) => c.textContent === 'Zerodha')
    if (!select) throw new Error('broker list not found')
    fireEvent.keyDown(select, { key: 'ArrowDown' })
    fireEvent.click(await screen.findByRole('option', { name: 'Angel' }))
    fireEvent.click(screen.getByRole('button', { name: /save broker credentials/i }))
  }

  it('asks before ending the live session, and Cancel saves nothing', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response(JSON.stringify({ logged_in: true, broker: 'zerodha' })))
    )
    await chooseAngelAndSave()
    expect(await screen.findByText('End your Zerodha session?')).toBeInTheDocument()
    expect(screen.getByText(/positions and orders stay at Zerodha/i)).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }))
    await waitFor(() => expect(screen.queryByText('End your Zerodha session?')).toBeNull())
    expect(client.post).not.toHaveBeenCalled()
  })

  it('saves once the trader confirms', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response(JSON.stringify({ logged_in: true, broker: 'zerodha' })))
    )
    client.post.mockResolvedValue({ data: { status: 'error', message: 'Not now' } })
    await chooseAngelAndSave()
    fireEvent.click(await screen.findByRole('button', { name: 'End session and switch' }))
    await waitFor(() => expect(client.post).toHaveBeenCalledTimes(1))
  })

  it('saves without asking when no broker session is live', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response(JSON.stringify({ logged_in: false, broker: null })))
    )
    client.post.mockResolvedValue({ data: { status: 'error', message: 'Not now' } })
    await chooseAngelAndSave()
    await waitFor(() => expect(client.post).toHaveBeenCalledTimes(1))
    expect(screen.queryByText(/End your .* session\?/)).toBeNull()
  })
})
