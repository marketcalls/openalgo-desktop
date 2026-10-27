// Desktop-only (D-047): one paused call on the approval card, in a
// trader's words. The web's PendingConfirm (components/agent/Message.tsx)
// renders this in place of the tool name and the JSON of its arguments;
// the rest of that card (the waiting sentence, the missing-details
// refusal, Approve and Reject) stays the web's.

import { type ApprovalRequest, describeApproval } from '@/lib/agent/orderWords'

export function OrderApproval({ requirement }: { requirement: ApprovalRequest }) {
  const { sentence, details } = describeApproval(requirement)
  return (
    <div>
      <p className="text-xs font-medium text-foreground">{sentence}</p>
      {details.length > 0 && (
        <dl className="mt-1 grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5 text-[11px] text-muted-foreground">
          {details.map(({ name, label, value }) => (
            <div key={name} className="contents">
              <dt>{label}</dt>
              <dd className="break-words font-mono text-foreground">{value}</dd>
            </div>
          ))}
        </dl>
      )}
    </div>
  )
}
