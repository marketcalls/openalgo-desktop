// The P&L Tracker's percentage under Current MTM (docs-49). The web divides
// the MTM by a fixed 1,00,000; the desktop reads it against the capital the
// tracker sends: the sandbox's capital in analyzer mode, the account's margin
// (Available Balance plus Utilised) live. With no capital known it shows no
// percentage rather than one against a made-up base.

export type CapitalBasis = 'sandbox' | 'account'

interface MtmPercentOfCapitalProps {
  mtm: number
  /** `undefined` before the first load, `null` when it could not be read. */
  capital: number | null | undefined
  basis: CapitalBasis | null | undefined
}

const BASIS_LABEL: Record<CapitalBasis, string> = {
  sandbox: 'of sandbox capital',
  account: 'of account margin',
}

export function MtmPercentOfCapital({ mtm, capital, basis }: MtmPercentOfCapitalProps) {
  if (capital === undefined) {
    return <div className="text-sm text-muted-foreground">--</div>
  }
  if (capital === null || capital <= 0 || !basis) {
    return (
      <div className="text-sm text-muted-foreground">
        % not shown: your funds could not be read. Refresh to try again.
      </div>
    )
  }
  const tone = mtm >= 0 ? 'text-green-500' : 'text-red-500'
  return (
    <div className={`text-sm ${tone}`}>
      {mtm >= 0 ? '+' : ''}
      {((mtm / capital) * 100).toFixed(2)}%{' '}
      <span className="text-muted-foreground">{BASIS_LABEL[basis]}</span>
    </div>
  )
}
