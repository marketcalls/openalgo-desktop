/**
 * Desktop-only sentences in the Flow config panel, where the web's panel
 * says nothing or something the run does not do.
 */

/**
 * Parity F-587 (lead decision 14): the Price Alert's Play Sound and Alert
 * Message are saved with the node but nothing reads them, on the web or
 * here: when the price condition is met the alert runs the nodes wired after
 * it. They stay so a workflow moves between OpenAlgo web and the desktop
 * unchanged, and this says what they do.
 */
export function FlowUnusedAlertFieldsNote() {
  return (
    <p className="text-[10px] text-muted-foreground">
      Play Sound and Alert Message are saved with the workflow so it moves between OpenAlgo web and
      the desktop unchanged, but neither is used when the alert fires: the alert runs the nodes
      wired after it. Add a Telegram or WhatsApp node to be told.
    </p>
  )
}

/**
 * Parity F-586 with decision 20: the web shows 30 days on a History node that
 * stores none and then asks the broker with no range; the desktop refuses a
 * history request with no range before the broker. The field is blank when
 * nothing is stored, and this says the run needs it.
 */
export function FlowHistoryDaysNote() {
  return (
    <p className="text-[10px] text-muted-foreground">
      How many days of candles to fetch. Required: with no days the node stops before asking the
      broker.
    </p>
  )
}
