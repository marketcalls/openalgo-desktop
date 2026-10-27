/**
 * One visualization inside an answer: a chart, an options figure, a card, or
 * OpenUI markup.
 *
 * Desktop: a placeholder until the visualization part lands with the web's
 * `components/agent/viz/`. It draws nothing, which is what the web's block
 * draws for a kind it does not know, so an answer's prose still reads in
 * order around where a chart will go.
 */

import type { AgentVizItem } from '@/lib/agent/viz'

export interface VizBlockProps {
  /** The item, straight from `AgentMessage.viz`. */
  item: AgentVizItem
  /** True while the turn is still streaming into this message. */
  streaming?: boolean
  /** Extra classes on the block, for a host that needs to adjust margins. */
  className?: string
}

export function VizBlock(_props: VizBlockProps) {
  return null
}
