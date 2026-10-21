export const Side = Object.freeze({ Buy: 'Buy', Sell: 'Sell' })

/**
 * @typedef {{ type: 'PlaceLimit', user_id: number, market_id: number,
 *   side: 'Buy' | 'Sell', price_ticks: number, qty: number }} PlaceLimit
 */

/** @returns {PlaceLimit} */
export const placeLimit = ({ userId, marketId, side, priceTicks, qty }) => ({
  type: 'PlaceLimit',
  user_id: userId,
  market_id: marketId,
  side,
  price_ticks: priceTicks,
  qty,
})
