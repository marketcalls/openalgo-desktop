/**
 * Python Strategy API stubs for OpenAlgo Desktop
 *
 * Python strategy execution is not available in the desktop version.
 * This requires a server-side Python runtime environment.
 */

import type { PythonStrategy, PythonStrategyContent, ScheduleConfig, LogContent, EnvironmentVariables } from '@/types/python-strategy'

interface ApiResponse<T = void> {
  status: string
  message?: string
  data?: T
}

const NOT_AVAILABLE = 'Python strategies are not available in desktop mode'

export const pythonStrategyApi = {
  getStrategies: async (): Promise<PythonStrategy[]> => {
    console.warn(NOT_AVAILABLE)
    return []
  },

  getStrategy: async (_strategyId: string): Promise<PythonStrategy> => {
    throw new Error(NOT_AVAILABLE)
  },

  getStrategyContent: async (_strategyId: string): Promise<PythonStrategyContent> => {
    throw new Error(NOT_AVAILABLE)
  },

  createStrategy: async (
    _data: Partial<PythonStrategy> & { code?: string }
  ): Promise<ApiResponse<{ strategy_id: string }>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  saveStrategy: async (
    _strategyId: string,
    _code: string
  ): Promise<ApiResponse<void>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  updateStrategy: async (
    _strategyId: string,
    _data: Partial<PythonStrategy> & { code?: string }
  ): Promise<ApiResponse<void>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  uploadStrategy: async (
    _name: string,
    _file: File,
    _schedule?: ScheduleConfig
  ): Promise<ApiResponse<{ strategy_id: string }>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  exportStrategy: async (_strategyId: string, _version?: 'saved' | 'current'): Promise<Blob> => {
    throw new Error(NOT_AVAILABLE)
  },

  downloadStrategy: async (_strategyId: string): Promise<Blob> => {
    throw new Error(NOT_AVAILABLE)
  },

  deleteStrategy: async (_strategyId: string): Promise<ApiResponse<void>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  getMasterContractStatus: async (): Promise<{ ready: boolean; message: string; last_updated: string | null }> => {
    return { ready: true, message: 'Not available in desktop mode', last_updated: null }
  },

  clearError: async (_strategyId: string): Promise<ApiResponse<void>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  checkAndStartPending: async (): Promise<ApiResponse<{ started: number }>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  getLogFiles: async (_strategyId: string): Promise<{ name: string; path: string; size_kb: number; last_modified: string }[]> => {
    return []
  },

  getLogContent: async (_strategyId: string, _logFile: string): Promise<LogContent> => {
    return { content: '', lines: 0, size_kb: 0, last_updated: new Date().toISOString() }
  },

  scheduleStrategy: async (
    _strategyId: string,
    _schedule: ScheduleConfig
  ): Promise<ApiResponse<void>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  startStrategy: async (_strategyId: string): Promise<ApiResponse<{ process_id: number; started?: boolean }>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  stopStrategy: async (_strategyId: string): Promise<ApiResponse<void>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  restartStrategy: async (_strategyId: string): Promise<ApiResponse<void>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  updateSchedule: async (
    _strategyId: string,
    _schedule: ScheduleConfig
  ): Promise<ApiResponse<void>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  clearSchedule: async (_strategyId: string): Promise<ApiResponse<void>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  getRunningStrategies: async (): Promise<string[]> => {
    console.warn(NOT_AVAILABLE)
    return []
  },

  getLogs: async (_strategyId: string, _lines?: number): Promise<LogContent> => {
    return { content: '', lines: 0, size_kb: 0, last_updated: new Date().toISOString() }
  },

  clearLogs: async (_strategyId: string): Promise<ApiResponse<void>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  getEnvironmentVariables: async (_strategyId: string): Promise<EnvironmentVariables> => {
    return { regular: {}, secure: {} }
  },

  setEnvironmentVariables: async (
    _strategyId: string,
    _variables: EnvironmentVariables
  ): Promise<ApiResponse<void>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },
}

export default pythonStrategyApi
