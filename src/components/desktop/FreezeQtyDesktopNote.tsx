import { Info } from 'lucide-react'
import { Alert, AlertDescription } from '@/components/ui/alert'

/**
 * Desktop departure from the web's Freeze Quantities page. The web copies
 * NSE's freeze file into its table once and lists those rows here. OpenAlgo
 * Desktop keeps NSE's NFO limits built in, so every release brings NSE's new
 * limits, and this table holds only the trader's own entries, which take
 * precedence (`services/freeze_qty_service.rs`, `FreezeLimits`). The note
 * says so, so an empty list is not read as "no limits".
 */
export function FreezeQtyDesktopNote() {
  return (
    <Alert>
      <Info className="h-4 w-4" />
      <AlertDescription>
        NSE's freeze quantities for NFO are built into OpenAlgo Desktop and updated with each
        release, so this list holds only your own entries. An entry replaces the built-in limit for
        its symbol at once: scalping order checks and exits, and the freeze quantity shown in symbol
        and option lookups, use it. Delete the entry to go back to the built-in limit.
      </AlertDescription>
    </Alert>
  )
}
