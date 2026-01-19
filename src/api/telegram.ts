/**
 * Telegram API stubs for OpenAlgo Desktop
 *
 * Telegram integration is not available in the desktop version.
 * This requires server-side bot hosting and webhook handling.
 */

import type {
  TelegramAnalytics,
  TelegramBotStatus,
  TelegramConfig,
  TelegramUser,
} from '@/types/telegram'

interface ApiResponse<T = void> {
  status: string
  message?: string
  data?: T
}

const NOT_AVAILABLE = 'Telegram integration is not available in desktop mode'

export const telegramApi = {
  getStatus: async (): Promise<TelegramBotStatus> => {
    console.warn(NOT_AVAILABLE)
    return {
      is_running: false,
      is_configured: false,
      bot_username: null,
      is_active: false,
    }
  },

  getConfig: async (): Promise<TelegramConfig> => {
    console.warn(NOT_AVAILABLE)
    return {
      broadcast_enabled: false,
      rate_limit_per_minute: 0,
      is_active: false,
    }
  },

  updateConfig: async (_config: Partial<TelegramConfig>): Promise<ApiResponse<void>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  getUsers: async (): Promise<TelegramUser[]> => {
    console.warn(NOT_AVAILABLE)
    return []
  },

  addUser: async (_chatId: string, _name?: string): Promise<ApiResponse<TelegramUser>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  removeUser: async (_chatId: string): Promise<ApiResponse<void>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  toggleUser: async (_chatId: string): Promise<ApiResponse<{ enabled: boolean }>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  getAnalytics: async (): Promise<TelegramAnalytics> => {
    console.warn(NOT_AVAILABLE)
    return {
      stats_7d: [],
      stats_30d: [],
      total_users: 0,
      active_users: 0,
      users: [],
    }
  },

  testConnection: async (): Promise<ApiResponse<{ bot_name: string }>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  sendTestMessage: async (_chatId: string): Promise<ApiResponse<void>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },
}

export default telegramApi
