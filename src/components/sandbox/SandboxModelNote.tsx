import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'

/** What the sandbox simulates and what it leaves out, shown where traders
 * review sandbox results. The full text is docs/user/sandbox.md. */
export const SANDBOX_MODEL_POINTS = [
  'Orders fill in full or not at all, at the price your broker reports: no partial fills, queue position or market impact.',
  'On the Indian exchanges each fill pays brokerage under your plan and the statutory charges; on a crypto venue it pays only the venue fee you set, nothing until you enter one. Charges come out of the available balance unless they are switched off (Sandbox Charges). P&L is shown before charges, with the charges beside it.',
  'Quantities are in contracts; a crypto contract value (such as 0.001 BTC per BTCUSD contract) applies to its margin, trade value and P&L.',
  'Capital is in rupees and crypto prices are in US dollars, with no conversion between them.',
  'CNC buys move to holdings at 00:00 IST the next calendar day, weekends and holidays included.',
  'Fills need prices from your broker: with no broker connected, or no recent price for a symbol, orders wait.',
] as const

export function SandboxModelNote({ className }: { className?: string }) {
  return (
    <Alert className={className} aria-label="Sandbox model">
      <AlertTitle>Sandbox model</AlertTitle>
      <AlertDescription>
        <ul className="list-disc list-inside space-y-1 mt-1">
          {SANDBOX_MODEL_POINTS.map((point) => (
            <li key={point}>{point}</li>
          ))}
        </ul>
      </AlertDescription>
    </Alert>
  )
}
