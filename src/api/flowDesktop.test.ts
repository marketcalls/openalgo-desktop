import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { WebhookInfo } from './flow'
import type { TunnelStatus } from './tunnel'

const getWebhookInfo = vi.hoisted(() => vi.fn())
const tunnelGet = vi.hoisted(() => vi.fn())

vi.mock('./flow', async (importOriginal) => ({
  ...(await importOriginal<typeof import('./flow')>()),
  getWebhookInfo,
}))
vi.mock('./tunnel', () => ({ tunnelApi: { get: tunnelGet } }))

const { getWebhookInfoForTrader, onTunnelAddress } = await import('./flowDesktop')

const LOCAL = 'http://127.0.0.1:5000'
const TUNNEL = 'https://desk.ngrok-free.app'

function info(base: string, withSecret = true): WebhookInfo {
  return {
    webhook_token: 'wf-token',
    webhook_secret: 'wf-secret',
    webhook_enabled: true,
    webhook_auth_type: withSecret ? 'url' : 'payload',
    webhook_url: `${base}/flow/webhook/wf-token`,
    webhook_url_with_symbol: `${base}/flow/webhook/wf-token/{symbol}`,
    webhook_url_with_secret: withSecret ? `${base}/flow/webhook/wf-token?secret=wf-secret` : null,
  }
}

const online: TunnelStatus = { state: 'online', url: `${TUNNEL}/`, message: 'Online' }

describe('onTunnelAddress', () => {
  it('moves every URL to the tunnel while it is online, keeping path and query', () => {
    expect(onTunnelAddress(info(LOCAL), online)).toEqual(info(TUNNEL))
  })

  it('keeps a missing secret URL missing', () => {
    expect(onTunnelAddress(info(LOCAL, false), online).webhook_url_with_secret).toBeNull()
  })

  it.each([
    ['stopped', { state: 'stopped', url: null, message: '' }],
    ['connecting', { state: 'connecting', url: null, message: '' }],
    ['in error', { state: 'error', url: `${TUNNEL}`, message: 'Authtoken refused' }],
    ['online without an address', { state: 'online', url: null, message: '' }],
    ['online with an address that is not a URL', { state: 'online', url: 'desk', message: '' }],
  ] as const)('leaves the server URLs alone while the tunnel is %s', (_label, tunnel) => {
    const local = info(LOCAL)
    expect(onTunnelAddress(local, tunnel)).toBe(local)
  })

  it('leaves the server URLs alone when the tunnel status is unknown', () => {
    const local = info(LOCAL)
    expect(onTunnelAddress(local, null)).toBe(local)
  })

  it('moves a Host Server URL to the tunnel too, since the tunnel is what is up', () => {
    expect(onTunnelAddress(info('https://algo.example.com'), online)).toEqual(info(TUNNEL))
  })
})

describe('getWebhookInfoForTrader', () => {
  beforeEach(() => {
    getWebhookInfo.mockReset()
    tunnelGet.mockReset()
    getWebhookInfo.mockResolvedValue(info(LOCAL))
  })

  it('asks for the workflow settings and the tunnel together', async () => {
    tunnelGet.mockResolvedValue({ enabled: true, tunnel: online })
    expect(await getWebhookInfoForTrader(7)).toEqual(info(TUNNEL))
    expect(getWebhookInfo).toHaveBeenCalledWith(7)
  })

  it('still shows the settings when the tunnel status cannot be read', async () => {
    tunnelGet.mockRejectedValue(new Error('Could not load the tunnel settings.'))
    expect(await getWebhookInfoForTrader(7)).toEqual(info(LOCAL))
  })

  it('fails as the settings request fails', async () => {
    tunnelGet.mockResolvedValue({ enabled: false, tunnel: { state: 'stopped', url: null } })
    getWebhookInfo.mockRejectedValue(new Error('Workflow not found'))
    await expect(getWebhookInfoForTrader(7)).rejects.toThrow('Workflow not found')
  })
})
