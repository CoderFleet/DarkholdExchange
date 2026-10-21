import { decode, encode } from '@msgpack/msgpack'

export const Side = Object.freeze({ Buy: 'Buy', Sell: 'Sell' })

/**
 * @typedef {{ type: 'PlaceLimit', client_order_id: number, market_id: number,
 *   side: 'Buy' | 'Sell', price_ticks: number, qty: number }} PlaceLimit
 * @typedef {{ type: 'PlaceMarket', client_order_id: number, market_id: number,
 *   side: 'Buy' | 'Sell', qty: number, protect_price_ticks: number }} PlaceMarket
 * @typedef {{ type: 'Cancel', client_order_id: number, order_id: number }} Cancel
 */

/** @returns {PlaceLimit} */
export const placeLimit = ({ clientOrderId, marketId, side, priceTicks, qty }) => ({
  type: 'PlaceLimit',
  client_order_id: clientOrderId,
  market_id: marketId,
  side,
  price_ticks: priceTicks,
  qty,
})

/** @returns {PlaceMarket} */
export const placeMarket = ({ clientOrderId, marketId, side, qty, protectPriceTicks }) => ({
  type: 'PlaceMarket',
  client_order_id: clientOrderId,
  market_id: marketId,
  side,
  qty,
  protect_price_ticks: protectPriceTicks,
})

/** @returns {Cancel} */
export const cancelOrder = ({ clientOrderId, orderId }) => ({
  type: 'Cancel',
  client_order_id: clientOrderId,
  order_id: orderId,
})

/** @returns {{ type: 'Subscribe', market_ids: number[], depth: number }} */
export const subscribe = ({ marketIds = [], depth = 20 }) => ({
  type: 'Subscribe',
  market_ids: marketIds,
  depth,
})

let commandSequence = 0

/** Client idempotency keys stay below Number.MAX_SAFE_INTEGER. */
export const nextClientOrderId = () => Date.now() * 100 + (commandSequence++ % 100)

export const encodeClientMessage = (message) => encode(message)

export const decodeServerMessage = (data) => decode(new Uint8Array(data))
