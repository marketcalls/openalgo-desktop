/**
 * Chartink API stubs for OpenAlgo Desktop
 *
 * Chartink integration is not available in the desktop version.
 * This requires server-side webhook handling.
 */

import type {
  ChartinkStrategy,
  ChartinkSymbolMapping,
  CreateChartinkStrategyRequest,
  AddChartinkSymbolRequest,
} from '@/types/chartink'
import type { SymbolSearchResult } from '@/types/strategy'

interface ApiResponse<T = void> {
  status: string
  message?: string
  data?: T
}

const NOT_AVAILABLE = 'Chartink integration is not available in desktop mode'

export const chartinkApi = {
  getStrategies: async (): Promise<ChartinkStrategy[]> => {
    console.warn(NOT_AVAILABLE)
    return []
  },

  getStrategy: async (
    _strategyId: number
  ): Promise<{ strategy: ChartinkStrategy; mappings: ChartinkSymbolMapping[] }> => {
    throw new Error(NOT_AVAILABLE)
  },

  createStrategy: async (
    _data: CreateChartinkStrategyRequest
  ): Promise<ApiResponse<{ strategy_id: number }>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  toggleStrategy: async (_strategyId: number): Promise<ApiResponse<{ is_active: boolean }>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  deleteStrategy: async (_strategyId: number): Promise<ApiResponse<void>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  addSymbolMapping: async (
    _strategyId: number,
    _data: AddChartinkSymbolRequest
  ): Promise<ApiResponse<void>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  addBulkSymbols: async (
    _strategyId: number,
    _csvData: string
  ): Promise<ApiResponse<{ added: number; failed: number }>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  deleteSymbolMapping: async (
    _strategyId: number,
    _mappingId: number
  ): Promise<ApiResponse<void>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  searchSymbols: async (_query: string, _exchange?: string): Promise<SymbolSearchResult[]> => {
    console.warn(NOT_AVAILABLE)
    return []
  },
}

export default chartinkApi
