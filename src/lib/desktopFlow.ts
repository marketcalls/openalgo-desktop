/**
 * Desktop-only helpers for the Flow editor, kept out of the files carried
 * from the web so those still take web changes by diff. Each fixes a web
 * defect named in .lane/105/parity.md.
 */

/**
 * The output that carries a branching node's "condition held" path.
 *
 * Parity F-693: the web's edge "+" menu inserts a Logic node and wires its
 * output to the old target with no source handle. These nodes only have
 * branch outputs, so the wire cannot be drawn, and the run treats a
 * handle-less edge out of a branching node as pass-through: the order after
 * it ran whether or not the condition held. The new wire leaves from the
 * pass branch instead (timeCondition and the NOT gate name it "yes").
 */
const PASS_BRANCH: Readonly<Record<string, string>> = {
  timeCondition: 'yes',
  notGate: 'yes',
  positionCheck: 'true',
  fundCheck: 'true',
  priceCondition: 'true',
  timeWindow: 'true',
  varCondition: 'true',
  priceAlert: 'true',
  andGate: 'true',
  orGate: 'true',
}

/** The pass branch of a branching node type; undefined for a node with one output. */
export function passBranchHandle(nodeType: string): string | undefined {
  return PASS_BRANCH[nodeType]
}
