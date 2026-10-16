import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { BrokerList } from '@/api/broker-config'
import { brokerEntry, brokerList, saved } from '@/test/brokerList'
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

// Desktop: the Broker tab is a summary with links; keys are edited in one
// place, the Broker Configuration page (the logged-in-broker switch and its
// confirmation live on Select Broker).
describe('Profile broker summary', () => {
  afterEach(() => vi.unstubAllGlobals())

  function serveList(list: BrokerList) {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (url: string) =>
        url === '/api/broker/list'
          ? new Response(JSON.stringify({ status: 'success', data: list }))
          : new Response('{}', { status: 404 })
      )
    )
  }

  it('shows the logged-in broker and the configured ones, with links to both pages', async () => {
    serveList(
      brokerList([
        { ...saved(brokerEntry('zerodha', 'Zerodha', 'redirect'), {}), active: true },
        saved(brokerEntry('fyers', 'Fyers', 'redirect'), {}),
        brokerEntry('angel', 'Angel One', 'form'),
      ])
    )
    render(<Profile />)
    expect(await screen.findByTestId('broker-logged-in')).toHaveTextContent('Zerodha')
    await waitFor(() =>
      expect(screen.getByTestId('broker-configured')).toHaveTextContent('FyersZerodha')
    )
    expect(screen.getByRole('link', { name: 'Select Broker' })).toHaveAttribute('href', '/broker')
    expect(screen.getByRole('link', { name: 'Broker Configuration' })).toHaveAttribute(
      'href',
      '/broker/config'
    )
    // No key form here any more.
    expect(screen.queryByRole('button', { name: /save broker credentials/i })).toBeNull()
  })

  it('says when no broker is logged in', async () => {
    serveList(brokerList([saved(brokerEntry('fyers', 'Fyers', 'redirect'), {})]))
    render(<Profile />)
    await waitFor(() =>
      expect(screen.getByTestId('broker-logged-in')).toHaveTextContent('No broker logged in')
    )
    await waitFor(() => expect(screen.getByTestId('broker-configured')).toHaveTextContent('Fyers'))
  })
})

// UI-05, UI-06: the desktop server has no SMTP settings and no file-permission
// check of a server install, so the web's SMTP and System tabs are left out and
// a note says where those needs are met.
describe('Profile tabs the desktop can serve', () => {
  it('offers no SMTP or System tab and never asks for system permissions', async () => {
    window.history.pushState({}, '', '/profile')
    render(<Profile />)
    const tabs = await screen.findAllByRole('tab')
    expect(tabs.map((t) => t.textContent?.trim())).toEqual([
      'Account',
      'Broker',
      'Alerts',
      'Theme',
      'TOTP',
    ])
    expect(screen.getByRole('link', { name: 'Diagnostics' })).toHaveAttribute(
      'href',
      '/admin/diagnostics'
    )
    expect(screen.getByText(/sends no email/)).toBeInTheDocument()
    const asked = client.get.mock.calls.map((c) => String(c[0]))
    expect(asked.some((u) => u.includes('/api/system/permissions'))).toBe(false)
  })
})
