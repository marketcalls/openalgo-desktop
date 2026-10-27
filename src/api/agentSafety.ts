/**
 * Desktop-only calls behind the Trading safety panel on /agent/config.
 *
 * The risk limits themselves travel through the web's own settings route
 * (`getSettings` / `updateSettings` in `api/agent.ts`), unchanged. The kill
 * switch has a route of its own because the panel's switch sets both of its
 * halves together, the setting and the file (decision 2 of the 1.0.4 plan),
 * and reports either one engaged.
 */

import { webClient } from './client'

/** The kill switch as the server reads it. */
export interface KillSwitchState {
  /** Either half engaged: every agent order is refused. */
  engaged: boolean
  by_setting: boolean
  by_file: boolean
  /** Where the file half lives (`<app data folder>/agent_kill_switch` by default). */
  path: string
}

export const killSwitchQueryKey = ['agent', 'kill-switch'] as const

export async function getKillSwitch(): Promise<KillSwitchState> {
  const response = await webClient.get<{ data: KillSwitchState }>('/agent/api/kill-switch')
  return response.data.data
}

export async function setKillSwitch(engaged: boolean): Promise<KillSwitchState> {
  const response = await webClient.put<{ data: KillSwitchState }>('/agent/api/kill-switch', {
    engaged,
  })
  return response.data.data
}
