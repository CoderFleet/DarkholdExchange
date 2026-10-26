// @ts-check
import { getAuthToken, notifyUnauthorized } from './authToken.js'
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
/** @typedef {Error & { reason?: ReasonCode, status?: number, retryAfter?: number }} ApiError */
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
 * Adds the login token when there is one. Without a token the request is sent exactly as
 * before, so the dev-user header flow keeps working.
 *
 * @param {RequestInit} options
 * @returns {RequestInit}
 */
const withAuth = (options) => {
  const token = getAuthToken()
  if (!token) return options
  return { ...options, headers: { ...options.headers, authorization: `Bearer ${token}` } }
}

/**
 * @template T
 * @param {string} path
 * @param {RequestInit} [options]
 * @returns {Promise<T>}
 */
export const request = async (path, options = {}) => {
  const response = await fetch(path, withAuth(options))
  /** @type {ErrorBody | null} */
  const body = await response.json().catch(() => null)
  if (!response.ok) {
    const reason = body?.reason
    const error = /** @type {ApiError} */ (
      new Error(reason ? String(reason).replaceAll('_', ' ') : `HTTP ${response.status}`)
    )
    error.reason = reason
    error.status = response.status
    // "Try again later" replies say how many seconds to wait.
    const retryAfter = Number(response.headers?.get?.('retry-after'))
    if (retryAfter > 0) error.retryAfter = retryAfter
    // The server refused the login token we sent: tell the auth store, so the user is logged out.
    // Compared as text because the shared ReasonCode list does not include the auth codes yet.
    if (response.status === 401 && String(reason) === 'unauthorized' && getAuthToken()) {
      notifyUnauthorized()
    }
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
export const postCommand = async (command, userId) => {
  try {
    return await request('/api/orders', {
      method: 'POST',
      headers: jsonHeaders(userId),
      body: JSON.stringify(command),
    })
  } catch (err) {
    // Older gateways may not expose the canonical endpoint. Fall back to the
    // legacy command route only when the response status says it is missing.
    const status = /** @type {ApiError} */ (err).status
    if (status === 404 || status === 405) {
      return await request('/api/commands', {
        method: 'POST',
        headers: jsonHeaders(userId),
        body: JSON.stringify(command),
      })
    }
    throw err
  }
}

/**
 * Canonical T1 placement endpoint.
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
 * @returns {Promise<{ user_id: number, cash_avail: number, cash_locked: number, escrow_cash: number, passes: any[] }>}
 */
export const getWallet = (userId) =>
  request('/api/wallet', {
    headers: { ...authHeaders(userId), 'x-user-id': String(userId) },
  })

/**
 * Top up user wallet in paise (100 paise = ₹1). Maximum 10,000,000 paise (₹100,000).
 *
 * @param {number} userId
 * @param {number} amountPaise
 * @returns {Promise<any>}
 */
export const topupWallet = (userId, amountPaise) =>
  request('/api/wallet/topup', {
    method: 'POST',
    headers: {
      ...jsonHeaders(userId),
      'x-user-id': String(userId),
    },
    body: JSON.stringify({ amount: amountPaise }),
  })

/**
 * @param {number} userId
 * @param {{ market_id?: number, side?: 'buy' | 'sell', limit?: number, cursor?: number }} [query]
 * @returns {Promise<{ items: any[], next_cursor: number | null }>}
 */
export const getHistory = (userId, query = {}) => {
  const params = new URLSearchParams()
  if (query.market_id != null) params.set('market_id', String(query.market_id))
  if (query.side != null) params.set('side', query.side)
  if (query.limit != null) params.set('limit', String(query.limit))
  if (query.cursor != null) params.set('cursor', String(query.cursor))
  const qs = params.toString() ? `?${params.toString()}` : ''

  return request(`/api/history${qs}`, {
    headers: { ...authHeaders(userId), 'x-user-id': String(userId) },
  })
}

/**
 * @param {number} userId
 * @param {{ limit?: number, cursor?: number }} [query]
 * @returns {Promise<{ items: any[], next_cursor: number | null }>}
 */
export const getStatements = (userId, query = {}) => {
  const params = new URLSearchParams()
  if (query.limit != null) params.set('limit', String(query.limit))
  if (query.cursor != null) params.set('cursor', String(query.cursor))
  const qs = params.toString() ? `?${params.toString()}` : ''

  return request(`/api/statements${qs}`, {
    headers: { ...authHeaders(userId), 'x-user-id': String(userId) },
  })
}

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
