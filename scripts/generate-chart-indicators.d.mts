/**
 * Types for what `generate-chart-indicators.mjs` exports, so the test that
 * imports it (`src/lib/trading/chartIndicators.test.ts`) is type checked.
 * Keep in step with the script.
 */

/** One indicator the chart can draw, as the committed catalogue lists it. */
export interface ChartIndicatorRow {
  id: string
  name: string
  category: string
  /** `onchart` draws on the price; anything else gets its own pane. */
  placement: string
}

/** Where the committed catalogue lives. */
export const OUT: string

/** Read the registry the chart itself uses, after the built-ins register. */
export function catalogue(): Promise<ChartIndicatorRow[]>

/** Render the catalogue exactly as the committed file holds it. */
export function render(rows: readonly ChartIndicatorRow[]): string
