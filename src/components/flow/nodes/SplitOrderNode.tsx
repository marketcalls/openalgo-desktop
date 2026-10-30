/**
 * Split Order Node
 * Split large orders into smaller chunks
 */

import { Handle, Position } from '@xyflow/react'
import { Split } from 'lucide-react'
import { memo } from 'react'
import { cn } from '@/lib/utils'
import type { SplitOrderNodeData } from '@/types/flow'

interface SplitOrderNodeProps {
  data: SplitOrderNodeData
  selected?: boolean
}

export const SplitOrderNode = memo(({ data, selected }: SplitOrderNodeProps) => {
  // Desktop: the colour follows the side shown, BUY when none is stored (parity F-677).
  const actionClass = (data.action || 'BUY') === 'BUY' ? 'text-green-500' : 'text-red-500'

  return (
    <div className={cn('workflow-node node-action min-w-[120px]', selected && 'selected')}>
      <Handle type="target" position={Position.Top} className="!top-0 !-translate-y-1/2" />
      <div className="p-2">
        <div className="mb-1.5 flex items-center gap-1.5">
          <div className="node-icon flex h-5 w-5 items-center justify-center rounded">
            <Split className="h-3 w-3" />
          </div>
          <div>
            <div className="text-xs font-medium leading-tight">Split Order</div>
            <div className="text-[9px] text-muted-foreground">{data.exchange || 'NSE'}</div>
          </div>
        </div>
        <div className="space-y-1">
          <div className="flex items-center justify-between rounded bg-muted/50 px-1.5 py-1">
            <span className="text-[10px] text-muted-foreground">Symbol</span>
            <span className="mono-data text-[10px] font-medium">{data.symbol || '-'}</span>
          </div>
          <div className="flex items-center justify-between text-[9px]">
            <span className={cn('font-medium', actionClass)}>{data.action || 'BUY'}</span>
            <span className="text-muted-foreground">
              {/* Desktop: 10 is the run's default split size (parity F-586). */}
              {data.quantity || 0} / {data.splitSize || 10}
            </span>
          </div>
          {/* Desktop: the web's "{delayMs}ms delay" row is dropped; the run
              never pauses between splits (parity F-677). */}
        </div>
      </div>
      <Handle type="source" position={Position.Bottom} className="!bottom-0 !translate-y-1/2" />
    </div>
  )
})

SplitOrderNode.displayName = 'SplitOrderNode'
