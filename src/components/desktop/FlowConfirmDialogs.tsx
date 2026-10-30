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

/**
 * Desktop departure from the web's /flow list (parity F-120, F-137, lead
 * decision 14). On the web one click on Delete removes a workflow, active or
 * not, with its schedule, webhook URL and run history, and one click on
 * Regenerate breaks every alert already set up with the old webhook URL.
 * Both are asked first here.
 */

interface ConfirmProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  onConfirm: () => void
}

const DESTRUCTIVE = 'bg-destructive text-destructive-foreground hover:bg-destructive/90'

export function DeleteWorkflowDialog({
  name,
  isActive,
  open,
  onOpenChange,
  onConfirm,
}: ConfirmProps & { name: string; isActive: boolean }) {
  return (
    <AlertDialog open={open} onOpenChange={onOpenChange}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>Delete {name}?</AlertDialogTitle>
          <AlertDialogDescription asChild>
            <div className="space-y-2">
              <p>
                The workflow is deleted with its schedule, its webhook URL and its run history. This
                cannot be undone. Export it first if you may want it back.
              </p>
              {isActive && (
                <p className="font-medium text-destructive">
                  It is active now: it stops, and alerts sent to its webhook URL are refused.
                </p>
              )}
            </div>
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>Cancel</AlertDialogCancel>
          <AlertDialogAction onClick={onConfirm} className={DESTRUCTIVE}>
            Delete workflow
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}

export function RegenerateWebhookDialog({ open, onOpenChange, onConfirm }: ConfirmProps) {
  return (
    <AlertDialog open={open} onOpenChange={onOpenChange}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>Regenerate the webhook URL and secret?</AlertDialogTitle>
          <AlertDialogDescription>
            Every alert set up with the current URL or secret (TradingView, Chartink, your own
            scripts) stops reaching this workflow until you paste the new URL and secret there.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>Cancel</AlertDialogCancel>
          <AlertDialogAction onClick={onConfirm} className={DESTRUCTIVE}>
            Regenerate
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}
