// @ts-check
/** @typedef {import('./protocol/generated/Balance').Balance} Balance */
/** @typedef {import('./protocol/generated/BookSnapshot').BookSnapshot} BookSnapshot */
/** @typedef {import('./protocol/generated/Command').Command} Command */
/** @typedef {import('./protocol/generated/CommandResponse').CommandResponse} CommandResponse */
/** @typedef {import('./protocol/generated/ErrorBody').ErrorBody} ErrorBody */
/** @typedef {import('./protocol/generated/Fill').Fill} Fill */
/** @typedef {import('./protocol/generated/MarketView').MarketView} MarketView */
/** @typedef {import('./protocol/generated/MetricsSnapshot').MetricsSnapshot} MetricsSnapshot */
/** @typedef {import('./protocol/generated/OpenOrderView').OpenOrderView} OpenOrderView */
/** @typedef {import('./protocol/generated/ReasonCode').ReasonCode} ReasonCode */
/** @typedef {Error & { reason?: ReasonCode }} ApiError */
/** @typedef {Exclude<Command, { type: 'Cancel' }>} PlaceCommand */

/**
 * @param {number} userId
 * @returns {Record<string, string>}
 */
const authHeaders = (userId) => ({
  'x-dev-user': String(userId),
})

/**
 * @param {number} userId
 * @returns {Record<string, string>}
 */
const jsonHeaders = (userId) => ({
  ...authHeaders(userId),
  'content-type': 'application/json',
})

/**
 * @template T
 * @param {string} path
 * @param {RequestInit} [options]
 * @returns {Promise<T>}
 */
const request = async (path, options = {}) => {
  const response = await fetch(path, options)
  /** @type {ErrorBody | null} */
  const body = await response.json().catch(() => null)
  if (!response.ok) {
    const reason = body?.reason
    const error = /** @type {ApiError} */ (
      new Error(reason ? String(reason).replaceAll('_', ' ') : `HTTP ${response.status}`)
    )
    error.reason = reason
    throw error
  }
  return /** @type {T} */ (body)
}

/** @returns {Promise<MarketView[]>} */
export const getMarkets = () => request('/api/markets')

/**
 * @param {number} userId
 * @returns {Promise<Balance[]>}
 */
export const getBalances = (userId) => request(`/api/users/${userId}/balances`)

/**
 * @param {number} userId
 * @returns {Promise<OpenOrderView[]>}
 */
export const getOpenOrders = (userId) =>
  request('/api/orders/open', { headers: authHeaders(userId) })

/**
 * @param {number} marketId
 * @param {number} [depth]
 * @returns {Promise<BookSnapshot>}
 */
export const getBook = (marketId, depth = 20) =>
  request(`/api/markets/${marketId}/book?depth=${depth}`)

/**
 * @returns {Promise<MetricsSnapshot>}
 */
export const getLatencyMetrics = () => request('/api/metrics/latency')

/**
 * @param {number} marketId
 * @param {number} [limit]
 * @returns {Promise<Fill[]>}
 */
export const getTrades = (marketId, limit = 40) =>
  request(`/api/markets/${marketId}/trades?limit=${limit}`)

/**
 * @param {Command} command
 * @param {number} userId
 * @returns {Promise<CommandResponse>}
 */
export const postCommand = (command, userId) =>
  request('/api/commands', {
    method: 'POST',
    headers: jsonHeaders(userId),
    body: JSON.stringify(command),
  })

/**
 * Canonical T1 placement endpoint. Cancel remains `/api/commands` for the
 * current UI until it migrates to `cancelOrderRequest`.
 *
 * @param {PlaceCommand} command
 * @param {number} userId
 * @returns {Promise<CommandResponse>}
 */
export const postOrder = (command, userId) =>
  request('/api/orders', {
    method: 'POST',
    headers: jsonHeaders(userId),
    body: JSON.stringify(command),
  })

/**
 * @param {number} orderId
 * @param {number} clientOrderId
 * @param {number} userId
 * @returns {Promise<CommandResponse>}
 */
export const cancelOrderRequest = (orderId, clientOrderId, userId) =>
  request(`/api/orders/${orderId}?client_order_id=${clientOrderId}`, {
    method: 'DELETE',
    headers: authHeaders(userId),
  })

/**
 * @param {number} userId
 * @returns {string}
 */
export const websocketUrl = (userId) => {
  const protocol = window.location.protocol === 'https:' ? 'wss' : 'ws'
  return `${protocol}://${window.location.host}/ws?user_id=${userId}`
}

/**
 * Canonical per-market feed.
 *
 * @param {number} userId
 * @param {number} marketId
 * @returns {string}
 */
export const marketWebsocketUrl = (userId, marketId) => {
  const protocol = window.location.protocol === 'https:' ? 'wss' : 'ws'
  return `${protocol}://${window.location.host}/ws/market/${marketId}?user_id=${userId}`
}
