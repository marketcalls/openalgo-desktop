/**
 * Desktop-only: the local server's listen addresses.
 *
 * OpenAlgo web reads these from .env; the desktop app has no .env, so they are
 * read and changed in-app through the local server's settings endpoint.
 */

import { webClient } from './client'

export interface ServerSettings {
  http_host: string
  http_port: number
  ws_host: string
  ws_port: number
  lan_enabled: boolean
  /** The market data feed's state; `message` names the cause and the fix. */
  ws_status?: MarketDataStatus
}

export interface MarketDataStatus {
  state: 'running' | 'starting' | 'port_in_use' | 'failed'
  port?: number
  message: string | null
}

export interface ServerSettingsResponse {
  status: 'success' | 'error'
  message?: string
  data?: ServerSettings
  /** Settings in use now (the market data feed moves within seconds). */
  applied?: string[]
  /** Settings that take effect after OpenAlgo restarts (the app address). */
  pending?: string[]
  restart_required?: boolean
}

export const serverSettingsApi = {
  async get(): Promise<ServerSettings> {
    const response = await webClient.get<ServerSettingsResponse>('/settings/api/server')
    if (response.data.status !== 'success' || !response.data.data) {
      throw new Error(response.data.message || 'Could not load the server settings.')
    }
    return response.data.data
  },

  async save(settings: ServerSettings): Promise<ServerSettingsResponse> {
    const response = await webClient.post<ServerSettingsResponse>('/settings/api/server', settings)
    return response.data
  },
}
