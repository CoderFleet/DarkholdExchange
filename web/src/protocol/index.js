// @ts-check
import { decode, encode } from '@msgpack/msgpack'

/** @typedef {import('./generated/Side').Side} SideValue */
/** @typedef {import('./generated/Command').Command} Command */
/** @typedef {import('./generated/WsClientMessage').WsClientMessage} WsClientMessage */
/** @typedef {import('./generated/WsServerMessage').WsServerMessage} WsServerMessage */
/** @typedef {Extract<Command, { type: 'PlaceLimit' }>} PlaceLimitCommand */
/** @typedef {Extract<Command, { type: 'PlaceMarket' }>} PlaceMarketCommand */
/** @typedef {Extract<Command, { type: 'Cancel' }>} CancelCommand */
/** @typedef {Extract<WsClientMessage, { type: 'Subscribe' }>} SubscribeCommand */

/** @type {{ Buy: SideValue, Sell: SideValue }} */
export const Side = Object.freeze({ Buy: 'Buy', Sell: 'Sell' })

/**
 * @param {{ clientOrderId: number, marketId: number, side: SideValue,
 *   priceTicks: number, qty: number, escrow?: boolean }} input
 * @returns {PlaceLimitCommand}
 */
export const placeLimit = ({ clientOrderId, marketId, side, priceTicks, qty, escrow }) => ({
  type: 'PlaceLimit',
  client_order_id: clientOrderId,
  market_id: marketId,
  side,
  price_ticks: priceTicks,
  qty,
  ...(escrow ? { escrow: true } : {}),
})

/**
 * @param {{ clientOrderId: number, marketId: number, side: SideValue,
 *   qty: number, protectPriceTicks: number, escrow?: boolean }} input
 * @returns {PlaceMarketCommand}
 */
export const placeMarket = ({ clientOrderId, marketId, side, qty, protectPriceTicks, escrow }) => ({
  type: 'PlaceMarket',
  client_order_id: clientOrderId,
  market_id: marketId,
  side,
  qty,
  protect_price_ticks: protectPriceTicks,
  ...(escrow ? { escrow: true } : {}),
})

/**
 * @param {{ clientOrderId: number, orderId: number }} input
 * @returns {CancelCommand}
 */
export const cancelOrder = ({ clientOrderId, orderId }) => ({
  type: 'Cancel',
  client_order_id: clientOrderId,
  order_id: orderId,
})

/**
 * @param {{ marketIds?: number[], depth?: number }} [input]
 * @returns {SubscribeCommand}
 */
export const subscribe = ({ marketIds = [], depth = 20 } = {}) => ({
  type: 'Subscribe',
  market_ids: marketIds,
  depth,
})

let commandSequence = 0

/** Client idempotency keys stay below Number.MAX_SAFE_INTEGER. */
export const nextClientOrderId = () => Date.now() * 100 + (commandSequence++ % 100)

/**
 * @param {Command | WsClientMessage} message
 * @returns {Uint8Array}
 */
export const encodeClientMessage = (message) => encode(message)

/**
 * @param {ArrayBuffer | Uint8Array} data
 * @returns {WsServerMessage}
 */
export const decodeServerMessage = (data) =>
  /** @type {WsServerMessage} */ (decode(new Uint8Array(data)))
