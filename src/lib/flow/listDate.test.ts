import { describe, expect, it } from 'vitest'
import { listDate } from './listDate'

/** 30 October 2026 as the browser writes a date, in IST. */
const OCT_30 = new Date(Date.UTC(2026, 9, 30, 6, 30)).toLocaleDateString(undefined, {
  timeZone: 'Asia/Kolkata',
})

describe('listDate', () => {
  it('reads the server time as UTC and shows the IST date', () => {
    // 20:00 UTC on the 29th is 01:30 IST on the 30th.
    expect(listDate('2026-10-29T20:00:00')).toBe(OCT_30)
    expect(listDate('2026-10-29T18:30:00')).toBe(OCT_30)
    expect(listDate('2026-10-29T18:29:59')).not.toBe(OCT_30)
  })

  it('keeps a time that carries its own zone', () => {
    expect(listDate('2026-10-29T20:00:00Z')).toBe(OCT_30)
    expect(listDate('2026-10-30T01:30:00+05:30')).toBe(OCT_30)
  })
})
