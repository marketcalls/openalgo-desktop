/**
 * Auth API for OpenAlgo Desktop
 *
 * Uses Tauri IPC commands for authentication operations.
 */

import { authCommands, brokerCommands, settingsCommands } from './client'
import type {
  BrokerCredentials,
  BrokerInfo,
  BrokerLoginResponse,
  BrokerStatus,
  LoginResponse,
  UserInfo,
} from './client'

// Re-export types for compatibility
export type { BrokerInfo, LoginResponse, UserInfo }

export interface LoginCredentials {
  username: string
  password: string
}

export interface SessionInfo {
  authenticated: boolean
  user?: UserInfo
  broker?: BrokerStatus
}

export const authApi = {
  /**
   * Login with username and password
   */
  login: async (credentials: LoginCredentials): Promise<LoginResponse> => {
    return authCommands.login({
      username: credentials.username,
      password: credentials.password,
    })
  },

  /**
   * Logout current user
   */
  logout: async (): Promise<void> => {
    await authCommands.logout()
  },

  /**
   * Get current session info
   */
  getSession: async (): Promise<SessionInfo> => {
    const [isAuthenticated, user, brokerStatus] = await Promise.all([
      authCommands.checkSession(),
      authCommands.getCurrentUser(),
      brokerCommands.getBrokerStatus(),
    ])

    return {
      authenticated: isAuthenticated,
      user: user || undefined,
      broker: brokerStatus,
    }
  },

  /**
   * Check if user is authenticated
   */
  checkSession: async (): Promise<boolean> => {
    return authCommands.checkSession()
  },

  /**
   * Get current user info
   */
  getCurrentUser: async (): Promise<UserInfo | null> => {
    return authCommands.getCurrentUser()
  },

  /**
   * Get list of available brokers
   */
  getBrokers: async (): Promise<BrokerInfo[]> => {
    return brokerCommands.getAvailableBrokers()
  },

  /**
   * Login to broker with credentials
   */
  brokerLogin: async (
    brokerId: string,
    credentials: BrokerCredentials
  ): Promise<BrokerLoginResponse> => {
    return brokerCommands.brokerLogin({
      broker_id: brokerId,
      credentials,
    })
  },

  /**
   * Logout from broker
   */
  brokerLogout: async (): Promise<void> => {
    await brokerCommands.brokerLogout()
  },

  /**
   * Get broker connection status
   */
  getBrokerStatus: async (): Promise<BrokerStatus> => {
    return brokerCommands.getBrokerStatus()
  },

  /**
   * Set active broker
   */
  setActiveBroker: async (brokerId: string): Promise<void> => {
    await brokerCommands.setActiveBroker(brokerId)
  },

  /**
   * Save broker API credentials to OS keychain
   */
  saveBrokerCredentials: async (
    brokerId: string,
    apiKey: string,
    apiSecret?: string,
    clientId?: string
  ): Promise<void> => {
    await settingsCommands.saveBrokerCredentials({
      broker_id: brokerId,
      api_key: apiKey,
      api_secret: apiSecret,
      client_id: clientId,
    })
  },

  /**
   * Delete broker credentials from OS keychain
   */
  deleteBrokerCredentials: async (brokerId: string): Promise<void> => {
    await settingsCommands.deleteBrokerCredentials(brokerId)
  },

  /**
   * Change password (for app user, not broker)
   */
  changePassword: async (_currentPassword: string, _newPassword: string): Promise<void> => {
    // TODO: Implement password change command in Rust
    throw new Error('Password change not yet implemented')
  },
}

export default authApi
