import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { ServerComponent } from '@/api/server-settings'
import { fireEvent, render, screen, waitFor } from '@/test/test-utils'

const client = vi.hoisted(() => ({ get: vi.fn(), post: vi.fn() }))
vi.mock('@/api/client', () => ({ webClient: client }))

import Profile from '@/pages/Profile'
import { ServerStatusCard } from './ServerStatusCard'

const PARTS: ServerComponent[] = [
  {
    name: 'App and API',
    address: '127.0.0.1:5000',
    state: 'ok',
    label: 'Running',
    detail: null,
  },
  {
    name: 'Market data server',
    address: '127.0.0.1:8765',
    state: 'problem',
    label: 'Port in use',
    detail: 'Port 8765 is already used by another program.',
  },
  {
    name: 'Broker market data',
    address: null,
    state: 'off',
    label: 'Not signed in',
    detail: 'Log in to your broker from Select Broker to stream prices.',
  },
]

function serve(parts: ServerComponent[]) {
  client.get.mockImplementation(async (url: string) => {
    if (url === '/settings/api/server') {
      return { data: { status: 'success', data: { components: parts } } }
    }
    if (url === '/api/broker/credentials') {
      // What an older server sent: the web's always-Running entries.
      return {
        data: {
          status: 'success',
          data: {
            current_broker: '',
            valid_brokers: [],
            ngrok_allow: false,
            host_server: 'http://127.0.0.1:5000',
            websocket_url: 'ws://127.0.0.1:8765',
            server_status: {
              flask: { host: '127.0.0.1', port: '5000' },
              websocket: { host: '127.0.0.1', port: '8765' },
              zmq: { host: '127.0.0.1', port: '5555' },
            },
          },
        },
      }
    }
    return { data: { status: 'success', data: {} } }
  })
}

beforeEach(() => serve(PARTS))

afterEach(() => {
  window.history.pushState({}, '', '/')
  vi.clearAllMocks()
})

// docs-35: the card showed the web's Flask App, WebSocket and ZeroMQ, each
// always "Running". It shows the desktop's parts with the state the server
// reports, and the cause and fix of a problem.
describe('Server Status', () => {
  it('shows each part with its real state and what to do about a problem', async () => {
    render(<ServerStatusCard />)
    expect(await screen.findByLabelText('App and API: Running')).toHaveTextContent('127.0.0.1:5000')
    expect(screen.getByLabelText('Market data server: Port in use')).toHaveTextContent(
      'Port 8765 is already used by another program.'
    )
    expect(screen.getByLabelText('Broker market data: Not signed in')).toBeInTheDocument()
    expect(screen.getByText(/as of /)).toBeInTheDocument()
  })

  it('reads the state again on Refresh', async () => {
    render(<ServerStatusCard />)
    await screen.findByLabelText('Market data server: Port in use')
    serve([{ ...PARTS[1], state: 'ok', label: 'Running', detail: null }])
    fireEvent.click(screen.getByRole('button', { name: /refresh/i }))
    expect(await screen.findByLabelText('Market data server: Running')).toBeInTheDocument()
  })

  it('says so when the state cannot be read', async () => {
    client.get.mockRejectedValue(new Error('offline'))
    render(<ServerStatusCard />)
    expect(await screen.findByRole('alert')).toHaveTextContent(
      'The server status could not be read. Choose Refresh to try again.'
    )
  })

  it('is what the Profile Broker tab shows, with no Flask or ZeroMQ', async () => {
    window.history.pushState({}, '', '/profile?tab=broker')
    render(<Profile />)
    expect(await screen.findByLabelText('Market data server: Port in use')).toBeInTheDocument()
    await waitFor(() => expect(screen.getByText('Server Status')).toBeInTheDocument())
    const text = document.body.textContent ?? ''
    for (const webOnly of ['Flask App', 'ZeroMQ', '5555']) expect(text).not.toContain(webOnly)
  })
})
