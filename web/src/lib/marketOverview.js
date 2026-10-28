// Small pure helpers shared by the landing page, the markets index, and the
// terminal. They keep sorting/filtering and top-of-book logic out of JSX.

/**
 * @param {{ bids: Array<{ price_ticks: number, qty: number }>, asks: Array<{ price_ticks: number, qty: number }> } | null | undefined} snapshot
 */
export const bestBidTicks = (snapshot) => snapshot?.bids?.[0]?.price_ticks ?? null

/**
 * @param {{ bids: Array<{ price_ticks: number, qty: number }>, asks: Array<{ price_ticks: number, qty: number }> } | null | undefined} snapshot
 */
export const bestAskTicks = (snapshot) => snapshot?.asks?.[0]?.price_ticks ?? null

/**
 * @param {any} snapshot
 * @returns {number | null}
 */
export const spreadTicks = (snapshot) => {
  const bid = bestBidTicks(snapshot)
  const ask = bestAskTicks(snapshot)
  if (bid == null || ask == null) return null
  return ask - bid
}

/**
 * @param {any} snapshot
 * @returns {number | null}
 */
export const lastTradeTicks = (snapshot) => snapshot?.last_trade_price ?? null

/**
 * @param {import('../protocol/generated/MarketView').MarketView[]} views
 */
export const sortMarketViews = (views) =>
  [...views].sort((left, right) => left.config.id - right.config.id)

/**
 * @param {import('../protocol/generated/MarketView').MarketView} view
 * @param {string} query
 * @param {'all' | 'spot' | 'escrow'} kind
 */
export const marketMatches = (view, query, kind) => {
  if (kind === 'escrow' && !view.config.escrow_enabled) return false
  if (kind === 'spot' && view.config.escrow_enabled) return false
  const needle = query.trim().toLowerCase()
  if (!needle) return true
  const haystack = [
    view.config.name,
    `market ${view.config.id}`,
    `m${view.config.id}`,
    `event ${view.config.event_id}`,
  ]
    .join(' ')
    .toLowerCase()
  return haystack.includes(needle)
}

/**
 * @param {import('../protocol/generated/MarketView').MarketView[]} views
 * @param {{ query?: string, kind?: 'all' | 'spot' | 'escrow' }} [options]
 */
export const filterMarketViews = (views, options = {}) => {
  const query = options.query ?? ''
  const kind = options.kind ?? 'all'
  return sortMarketViews(views).filter((view) => marketMatches(view, query, kind))
}

/**
 * @param {import('../protocol/generated/MarketView').MarketView[]} views
 */
export const marketOverview = (views) => ({
  total: views.length,
  escrow: views.filter((view) => view.config.escrow_enabled).length,
  spot: views.filter((view) => !view.config.escrow_enabled).length,
  withPrints: views.filter((view) => view.snapshot?.last_trade_price != null).length,
})
