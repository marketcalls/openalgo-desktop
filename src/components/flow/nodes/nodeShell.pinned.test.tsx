/**
 * The shell every node face shares (parity F-672): the root the Flow CSS
 * keys on, `workflow-node` with the face's category class (node-start,
 * node-trigger, node-action, node-condition, node-data, node-utility) or its
 * coloured left border, the `selected` class only while selected, how many
 * values it sets in `mono-data`, and the memo wrapper and display name each
 * face is registered with. The group box is the one face outside that shell.
 */

import { render } from '@testing-library/react'
import { type Node, ReactFlow, ReactFlowProvider } from '@xyflow/react'
import { describe, expect, it } from 'vitest'
import { nodeTypes } from './index'

/** The root element a face draws inside xyflow's node wrapper. */
function root(type: string, selected: boolean): HTMLElement {
  const node: Node = { id: `n-${type}`, type, position: { x: 0, y: 0 }, data: {}, selected }
  const { container } = render(
    <div style={{ width: 800, height: 600 }}>
      <ReactFlowProvider>
        <ReactFlow nodes={[node]} edges={[]} nodeTypes={nodeTypes} />
      </ReactFlowProvider>
    </div>
  )
  const el = container.querySelector<HTMLElement>(`.react-flow__node[data-id="${node.id}"]`)
  if (!el?.firstElementChild) throw new Error(`${type} not drawn`)
  return el.firstElementChild as HTMLElement
}

/** Every face but the group: its root classes, mono-data values and display name. */
const SHELLS: Array<[type: string, classes: string, mono: number, displayName: string]> = [
  ['start', 'workflow-node node-start min-w-[120px]', 0, 'StartNode'],
  ['priceAlert', 'workflow-node node-condition min-w-[120px]', 2, 'PriceAlertNode'],
  ['webhookTrigger', 'workflow-node node-trigger min-w-[120px]', 0, 'WebhookTriggerNode'],
  ['httpRequest', 'workflow-node min-w-[140px] border-l-primary', 0, 'HttpRequestNode'],
  ['orderUpdateTrigger', 'workflow-node node-trigger min-w-[120px]', 1, 'OrderUpdateTriggerNode'],
  ['placeOrder', 'workflow-node node-action min-w-[120px]', 2, 'PlaceOrderNode'],
  ['smartOrder', 'workflow-node node-action min-w-[120px]', 3, 'SmartOrderNode'],
  ['optionsOrder', 'workflow-node node-action min-w-[130px]', 2, 'OptionsOrderNode'],
  ['optionsMultiOrder', 'workflow-node node-action min-w-[130px]', 1, 'OptionsMultiOrderNode'],
  ['cancelAllOrders', 'workflow-node node-action min-w-[110px]', 0, 'CancelAllOrdersNode'],
  ['closePositions', 'workflow-node node-action min-w-[110px]', 0, 'ClosePositionsNode'],
  ['cancelOrder', 'workflow-node node-action min-w-[120px]', 1, 'CancelOrderNode'],
  ['modifyOrder', 'workflow-node node-action min-w-[120px]', 1, 'ModifyOrderNode'],
  ['basketOrder', 'workflow-node node-action min-w-[120px]', 1, 'BasketOrderNode'],
  ['splitOrder', 'workflow-node node-action min-w-[120px]', 1, 'SplitOrderNode'],
  ['positionCheck', 'workflow-node node-condition min-w-[120px]', 1, 'PositionCheckNode'],
  ['fundCheck', 'workflow-node node-condition min-w-[110px]', 1, 'FundCheckNode'],
  ['timeWindow', 'workflow-node node-condition min-w-[120px]', 2, 'TimeWindowNode'],
  ['timeCondition', 'workflow-node node-condition min-w-[120px]', 1, 'TimeConditionNode'],
  ['priceCondition', 'workflow-node node-condition min-w-[120px]', 1, 'PriceConditionNode'],
  ['varCondition', 'workflow-node node-condition min-w-[130px]', 1, 'VarConditionNode'],
  ['andGate', 'workflow-node node-condition min-w-[100px]', 0, 'AndGateNode'],
  ['orGate', 'workflow-node node-condition min-w-[100px]', 0, 'OrGateNode'],
  ['notGate', 'workflow-node node-condition min-w-[100px]', 0, 'NotGateNode'],
  ['getQuote', 'workflow-node min-w-[110px] border-l-primary', 1, 'GetQuoteNode'],
  ['getDepth', 'workflow-node min-w-[110px] border-l-primary', 1, 'GetDepthNode'],
  ['getOrderStatus', 'workflow-node min-w-[120px] border-l-primary', 1, 'GetOrderStatusNode'],
  ['history', 'workflow-node min-w-[110px] border-l-primary', 1, 'HistoryNode'],
  ['indicator', 'workflow-node min-w-[130px] border-l-primary', 1, 'IndicatorNode'],
  ['priorPeriodOhlc', 'workflow-node min-w-[120px] border-l-primary', 1, 'PriorPeriodOhlcNode'],
  ['strategyPnl', 'workflow-node min-w-[130px] border-l-primary', 1, 'StrategyPnlNode'],
  ['barOffset', 'workflow-node min-w-[120px] border-l-primary', 2, 'BarOffsetNode'],
  ['openPosition', 'workflow-node min-w-[110px] border-l-primary', 1, 'OpenPositionNode'],
  ['expiry', 'workflow-node min-w-[110px] border-l-primary', 2, 'ExpiryNode'],
  ['intervals', 'workflow-node min-w-[110px] border-l-primary', 0, 'IntervalsNode'],
  ['multiQuotes', 'workflow-node node-data min-w-[130px]', 1, 'MultiQuotesNode'],
  ['symbol', 'workflow-node min-w-[120px] border-l-primary', 1, 'SymbolNode'],
  ['optionSymbol', 'workflow-node min-w-[130px] border-l-primary', 2, 'OptionSymbolNode'],
  ['orderBook', 'workflow-node min-w-[110px] border-l-primary', 0, 'OrderBookNode'],
  ['tradeBook', 'workflow-node min-w-[110px] border-l-primary', 0, 'TradeBookNode'],
  ['positionBook', 'workflow-node min-w-[110px] border-l-primary', 0, 'PositionBookNode'],
  ['syntheticFuture', 'workflow-node min-w-[130px] border-l-primary', 2, 'SyntheticFutureNode'],
  ['optionChain', 'workflow-node min-w-[130px] border-l-primary', 2, 'OptionChainNode'],
  ['subscribeLtp', 'workflow-node min-w-[120px] border-l-green-500', 0, 'SubscribeLTPNode'],
  ['subscribeQuote', 'workflow-node min-w-[120px] border-l-green-500', 0, 'SubscribeQuoteNode'],
  ['subscribeDepth', 'workflow-node min-w-[120px] border-l-green-500', 0, 'SubscribeDepthNode'],
  ['unsubscribe', 'workflow-node min-w-[120px] border-l-red-500', 1, 'UnsubscribeNode'],
  ['holdings', 'workflow-node min-w-[120px] border-l-amber-500', 0, 'HoldingsNode'],
  ['funds', 'workflow-node min-w-[120px] border-l-amber-500', 0, 'FundsNode'],
  ['margin', 'workflow-node min-w-[120px] border-l-amber-500', 1, 'MarginNode'],
  [
    'telegramAlert',
    'workflow-node min-w-[120px] border-l-muted-foreground',
    0,
    'TelegramAlertNode',
  ],
  [
    'whatsappAlert',
    'workflow-node min-w-[120px] border-l-muted-foreground',
    0,
    'WhatsappAlertNode',
  ],
  ['delay', 'workflow-node min-w-[100px] border-l-muted-foreground', 1, 'DelayNode'],
  ['waitUntil', 'workflow-node node-utility min-w-[120px]', 1, 'WaitUntilNode'],
  ['variable', 'workflow-node node-utility min-w-[120px]', 2, 'VariableNode'],
  [
    'mathExpression',
    'workflow-node min-w-[140px] border-purple-500/50 bg-purple-500/5',
    0,
    'MathExpressionNode',
  ],
  ['log', 'workflow-node node-utility min-w-[110px]', 0, 'LogNode'],
  ['holidays', 'workflow-node min-w-[110px] border-l-purple-400', 1, 'HolidaysNode'],
  ['timings', 'workflow-node min-w-[110px] border-l-purple-400', 1, 'TimingsNode'],
  ['calendar', 'workflow-node min-w-[130px] border-l-purple-400', 1, 'CalendarNode'],
]

describe('every node face shares the workflow-node shell (F-672)', () => {
  it('lists every registered face but the group', () => {
    expect(SHELLS.map(([type]) => type).sort()).toEqual(
      Object.keys(nodeTypes)
        .filter((t) => t !== 'group')
        .sort()
    )
  })

  it.each(SHELLS)('%s draws its root as "%s"', (type, classes, mono) => {
    const el = root(type, false)
    expect(el.className).toBe(classes)
    expect(el.classList.contains('workflow-node')).toBe(true)
    expect(el.classList.contains('selected')).toBe(false)
    expect(el.querySelectorAll('.mono-data')).toHaveLength(mono)
  })

  it.each(SHELLS)('%s adds only "selected" while selected', (type, classes) => {
    expect(root(type, true).className).toBe(`${classes} selected`)
  })

  it.each(SHELLS)(
    '%s is a memo component with its own display name',
    (type, _c, _m, displayName) => {
      const face = nodeTypes[type as keyof typeof nodeTypes] as unknown as {
        $$typeof: symbol
        displayName?: string
      }
      expect(face.$$typeof).toBe(Symbol.for('react.memo'))
      expect(face.displayName).toBe(displayName)
    }
  )

  it('draws the group as a dashed box, ringed while selected', () => {
    const box =
      'min-h-[150px] min-w-[200px] rounded-lg border-2 border-dashed border-border/50 bg-muted/20'
    expect(root('group', false).className).toBe(box)
    expect(root('group', true).className).toBe(
      `${box} ring-2 ring-primary ring-offset-2 ring-offset-background`
    )
  })
})
