// Desktop-only (D-047): one paused call on the approval card, in a
// trader's words. The web's PendingConfirm (components/agent/Message.tsx)
// renders this in place of the tool name and the JSON of its arguments;
// the rest of that card (the waiting sentence, the missing-details
// refusal, Approve and Reject) stays the web's.
//
// A smart order also shows what it will actually send from the position
// held now (the preview route), since that difference, not the target, is
// the order the trader approves and the risk check measures.

import { useQuery } from '@tanstack/react-query'
import { webClient } from '@/api/client'
import {
  type ApprovalRequest,
  describeApproval,
  describeSmartPreview,
  type SmartOrderPreview,
} from '@/lib/agent/orderWords'

const PREVIEW_FIELDS = ['symbol', 'exchange', 'product', 'position_size', 'quantity', 'action']

function SmartOrderSends({ args, runId }: { args: Record<string, unknown>; runId?: string }) {
  // The run id lets the server record the position this card shows, so the
  // order is not sent if the position has moved by the time it runs.
  const params: Record<string, string> = runId ? { run_id: runId } : {}
  for (const key of PREVIEW_FIELDS) {
    const value = args[key]
    if (value !== null && value !== undefined) params[key] = String(value)
  }
  const preview = useQuery({
    queryKey: ['agent', 'orders', 'preview', params],
    queryFn: async () => {
      const response = await webClient.get<{ data: SmartOrderPreview }>(
        '/agent/api/orders/preview',
        { params }
      )
      return response.data.data
    },
    retry: false,
  })
  return (
    <p className="mt-1 text-xs text-foreground">
      {describeSmartPreview(preview.data, preview.isError)}
    </p>
  )
}

export function OrderApproval({
  requirement,
  runId,
}: {
  requirement: ApprovalRequest
  runId?: string
}) {
  const { sentence, details } = describeApproval(requirement)
  const smart =
    requirement.tool_name === 'place_smart_order' && (requirement.missing ?? []).length === 0
  return (
    <div>
      <p className="text-xs font-medium text-foreground">{sentence}</p>
      {smart && <SmartOrderSends args={requirement.args ?? {}} runId={runId} />}
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
