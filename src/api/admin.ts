/**
 * Admin API stubs for OpenAlgo Desktop
 *
 * Admin features (freeze quantities, holidays, market timings) are not
 * available in the desktop version. These are server-side features.
 */

import type {
  AddFreezeQtyRequest,
  AddHolidayRequest,
  AdminStats,
  FreezeQty,
  Holiday,
  HolidaysResponse,
  TimingsResponse,
  TodayTiming,
  UpdateFreezeQtyRequest,
  UpdateTimingRequest,
} from '@/types/admin'

interface ApiResponse<T = void> {
  status: string
  message?: string
  data?: T
}

const NOT_AVAILABLE = 'Admin features are not available in desktop mode'

export const adminApi = {
  // ============================================================================
  // Admin Stats
  // ============================================================================

  getStats: async (): Promise<AdminStats> => {
    console.warn(NOT_AVAILABLE)
    return {
      freeze_count: 0,
      holiday_count: 0,
    }
  },

  // ============================================================================
  // Freeze Quantity APIs
  // ============================================================================

  getFreezeList: async (): Promise<FreezeQty[]> => {
    console.warn(NOT_AVAILABLE)
    return []
  },

  addFreezeQty: async (_data: AddFreezeQtyRequest): Promise<ApiResponse<FreezeQty>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  // Alias for compatibility
  addFreeze: async (_data: AddFreezeQtyRequest): Promise<ApiResponse<FreezeQty>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  updateFreezeQty: async (
    _id: number,
    _data: UpdateFreezeQtyRequest
  ): Promise<ApiResponse<FreezeQty>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  // Alias for compatibility
  editFreeze: async (
    _id: number,
    _data: UpdateFreezeQtyRequest
  ): Promise<ApiResponse<FreezeQty>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  deleteFreezeQty: async (_id: number): Promise<ApiResponse<void>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  // Alias for compatibility
  deleteFreeze: async (_id: number): Promise<ApiResponse<void>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  uploadFreezeCSV: async (_file: File, _exchange?: string): Promise<ApiResponse<{ count: number }>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  refreshFreezeQty: async (): Promise<ApiResponse<{ count: number }>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  // ============================================================================
  // Holiday APIs
  // ============================================================================

  getHolidays: async (_year?: number): Promise<HolidaysResponse> => {
    console.warn(NOT_AVAILABLE)
    return { status: 'success', data: [], current_year: new Date().getFullYear(), years: [], exchanges: [] }
  },

  addHoliday: async (_data: AddHolidayRequest): Promise<ApiResponse<Holiday>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  deleteHoliday: async (_id: number): Promise<ApiResponse<void>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  // ============================================================================
  // Market Timings APIs
  // ============================================================================

  getTimings: async (): Promise<TimingsResponse> => {
    console.warn(NOT_AVAILABLE)
    return { status: 'success', data: [], today_timings: [], today: new Date().toISOString().split('T')[0], exchanges: [] }
  },

  updateTiming: async (_id: number, _data: UpdateTimingRequest): Promise<ApiResponse<void>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  // Alias for compatibility - takes exchange instead of id
  editTiming: async (_exchange: string, _data: UpdateTimingRequest): Promise<ApiResponse<void>> => {
    return { status: 'error', message: NOT_AVAILABLE }
  },

  getTodayTimings: async (): Promise<TodayTiming[]> => {
    console.warn(NOT_AVAILABLE)
    return []
  },

  checkTimings: async (_date?: string): Promise<{ status: string; timings: TodayTiming[]; message?: string }> => {
    console.warn(NOT_AVAILABLE)
    return { status: 'success', timings: [] }
  },
}

export default adminApi
