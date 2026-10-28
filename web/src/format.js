const money = new Intl.NumberFormat('en-IN', {
  minimumFractionDigits: 2,
  maximumFractionDigits: 2,
})

const integer = new Intl.NumberFormat('en-IN', { maximumFractionDigits: 0 })

/** Minor units (paise) to a rupee string. */
export const formatMoney = (minor) => `₹${money.format(Number(minor ?? 0) / 100)}`

/** Price ticks to a rupee string. */
export const formatPrice = (market, ticks) =>
  market ? formatMoney(Number(ticks) * Number(market.tick_size_minor)) : '—'

export const formatQty = (qty) => integer.format(Number(qty ?? 0))

export const formatPercent = (bps) => `${(Number(bps) / 100).toFixed(2)}%`

export const marketBounds = (market) => {
  if (!market) return { floor: 0, cap: 0, faceTicks: 0 }
  const faceTicks = Math.floor(
    Number(market.face_value_minor) / Number(market.tick_size_minor),
  )
  return {
    faceTicks,
    floor: Math.ceil((faceTicks * Number(market.collar_floor_bps)) / 10_000),
    cap: Math.floor((faceTicks * Number(market.collar_cap_bps)) / 10_000),
  }
}

export const reasonText = (reason) =>
  ({
    outside_collar: 'Price is outside the market collar',
    insufficient_funds: 'Not enough available cash',
    insufficient_inventory: 'Not enough passes',
    holding_limit_exceeded: 'Holding limit would be exceeded',
    self_trade: 'Self-trade prevented',
    unknown_market: 'Unknown market',
    market_closed: 'Market is closed',
    duplicate_client_order_id: 'Client order id already used',
    invalid_quantity: 'Quantity must be positive',
    invalid_price: 'Invalid price',
    price_not_tick_aligned: 'Price is not tick aligned',
    order_not_found: 'Order is no longer open',
    not_order_owner: 'You do not own that order',
    cancel_not_allowed: 'This order cannot be cancelled',
    auction_not_active: 'Auction is not active',
    auction_closed: 'Auction is closed',
    stale_auction_price: 'Auction price updated, please retry',
    escrow_not_ready: 'Seller has not registered a delivery signing key yet',
    invalid_receipt: 'Invalid delivery receipt',
    receipt_expired: 'Delivery receipt expired',
    rate_limited: 'Rate limited',
    pow_required: 'Too many requests from your network. Please wait a minute and try again',
    queue_full: 'Core queue is full',
    internal_error: 'Internal engine error',
  })[reason] ?? String(reason ?? 'Request failed').replaceAll('_', ' ')

export const statusText = (response) => {
  if (!response) return 'No response'
  if (response.status === 'Rejected') return reasonText(response.reason)
  if (response.status === 'Filled') return `Filled ${formatQty(response.filled_qty)}`
  if (response.status === 'PartiallyFilled') {
    return `Filled ${formatQty(response.filled_qty)} · ${formatQty(response.remaining_qty)} resting`
  }
  if (response.status === 'Rested') return `Order #${response.order_id} resting`
  if (response.status === 'Cancelled') return `Cancelled ${formatQty(response.remaining_qty)}`
  return response.status
}
