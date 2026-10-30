/**
 * A workflow's date on the /flow list, in market time (IST). The server
 * sends UTC without an offset, as the web's `isoformat()` does; the web
 * reads it as the browser's local time, so a save between 00:00 and 05:30
 * IST showed the day before (parity S-06, decision 23).
 */
export function listDate(updatedAt: string): string {
  const hasZone = /(?:Z|[+-]\d{2}:?\d{2})$/i.test(updatedAt)
  return new Date(hasZone ? updatedAt : `${updatedAt}Z`).toLocaleDateString(undefined, {
    timeZone: 'Asia/Kolkata',
  })
}
