/**
 * Desktop-only: confirm before a broker switch ends the live broker session.
 *
 * The desktop connects one broker at a time, so making another broker the
 * active one ends the session that is live now. The broker page and the
 * Profile broker tab ask first, naming what stops and what stays.
 */

import { AlertTriangle } from 'lucide-react'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'

interface BrokerSwitchConfirmProps {
  /** The broker whose session ends (display name); null closes the dialog. */
  from: string | null
  /** The broker being switched to (display name). */
  to: string
  onCancel: () => void
  onConfirm: () => void
}

export function BrokerSwitchConfirm({ from, to, onCancel, onConfirm }: BrokerSwitchConfirmProps) {
  return (
    <AlertDialog
      open={from !== null}
      onOpenChange={(open) => {
        if (!open) onCancel()
      }}
    >
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle className="flex items-center gap-2">
            <AlertTriangle className="h-5 w-5 text-yellow-500" />
            End your {from} session?
          </AlertDialogTitle>
          <AlertDialogDescription asChild>
            <div className="space-y-3">
              <p>
                Switching to {to} ends the live {from} session in OpenAlgo now. OpenAlgo stops
                placing stop-loss and target exits for it, and your trading platforms and API
                clients can no longer trade through it.
              </p>
              <p>
                Open positions and orders stay at {from}. Close or protect them there first if you
                need to.
              </p>
            </div>
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel onClick={onCancel}>Cancel</AlertDialogCancel>
          <AlertDialogAction onClick={onConfirm}>End session and switch</AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}
