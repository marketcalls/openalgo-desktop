/**
 * Desktop-only: the built-in ngrok tunnel's settings and status.
 *
 * OpenAlgo web starts ngrok from .env (NGROK_ALLOW, HOST_SERVER) with the
 * authtoken in ngrok's own configuration file; the desktop keeps the
 * authtoken (encrypted, never sent back) and the reserved domain in the app.
 * The switch is NGROK_ALLOW, saved with the server addresses on the Broker
 * tab.
 */

import { webClient } from './client'

export type TunnelState = 'stopped' | 'connecting' | 'online' | 'error'

export interface TunnelStatus {
  state: TunnelState
  /** The public address, while online. */
  url: string | null
  /** The cause and the next step, in the trader's words. */
  message: string
}

export interface TunnelSettings {
  enabled: boolean
  /** Whether an authtoken is saved; the authtoken itself is never sent back. */
  authtoken_saved: boolean
  domain: string | null
  tunnel: TunnelStatus
}

export interface TunnelChange {
  enabled?: boolean
  authtoken?: string
  forget_authtoken?: boolean
  /** An empty string clears the domain. */
  domain?: string
}

interface TunnelResponse {
  status: 'success' | 'error'
  message?: string
  data?: TunnelSettings
}

function settingsOf(body: TunnelResponse, fallback: string): TunnelSettings {
  if (body.status !== 'success' || !body.data) {
    throw new Error(body.message || fallback)
  }
  return body.data
}

export const tunnelApi = {
  async get(): Promise<TunnelSettings> {
    const response = await webClient.get<TunnelResponse>('/api/tunnel')
    return settingsOf(response.data, 'Could not load the tunnel settings.')
  },

  async save(change: TunnelChange): Promise<TunnelSettings> {
    const notSaved = 'Your tunnel settings were not saved. Try again.'
    let body: TunnelResponse
    try {
      body = (await webClient.post<TunnelResponse>('/api/tunnel', change)).data
    } catch (e) {
      // The server's sentence when it gave one (a refused authtoken or
      // domain), never the transport's.
      const message = (e as { response?: { data?: { message?: string } } }).response?.data?.message
      throw new Error(message || notSaved)
    }
    return settingsOf(body, notSaved)
  },
}
