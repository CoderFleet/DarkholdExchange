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

const jsonHeaders = {
  'content-type': 'application/json',
}

/**
 * Adds the login token when there is one. Protected calls are only made from pages that
 * RequireRole already put behind a login, so there is no anonymous identity fallback.
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
 * Current core state hash, for the trust strip and proof pages.
 *
 * @returns {Promise<{ state_hash: string }>}
 */
export const getStateHash = () => request('/api/state/hash')

/**
 * @param {number} userId
 * @returns {Promise<Balance[]>}
 */
export const getBalances = (userId) => request(`/api/users/${userId}/balances`)

/**
 * @returns {Promise<OpenOrderView[]>}
 */
export const getOpenOrders = () => request('/api/orders/open')

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
 * The caller's Merkle-sum inclusion proofs plus the published per-asset roots.
 *
 * @returns {Promise<{ seq: number, roots: any[], proofs: any[] }>}
 */
export const getMySolvency = () => request('/api/me/solvency')

/**
 * @param {number} marketId
 * @param {number} [limit]
 * @returns {Promise<Fill[]>}
 */
export const getTrades = (marketId, limit = 40) =>
  request(`/api/markets/${marketId}/trades?limit=${limit}`)

/**
 * @param {Command} command
 * @returns {Promise<CommandResponse>}
 */
export const postCommand = async (command) => {
  try {
    return await request('/api/orders', {
      method: 'POST',
      headers: jsonHeaders,
      body: JSON.stringify(command),
    })
  } catch (err) {
    // Older gateways may not expose the canonical endpoint. Fall back to the
    // legacy command route only when the response status says it is missing.
    const status = /** @type {ApiError} */ (err).status
    if (status === 404 || status === 405) {
      return await request('/api/commands', {
        method: 'POST',
        headers: jsonHeaders,
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
 * @returns {Promise<CommandResponse>}
 */
export const postOrder = (command) =>
  request('/api/orders', {
    method: 'POST',
    headers: jsonHeaders,
    body: JSON.stringify(command),
  })

/**
 * @param {number} orderId
 * @param {number} clientOrderId
 * @returns {Promise<CommandResponse>}
 */
export const cancelOrderRequest = (orderId, clientOrderId) =>
  request(`/api/orders/${orderId}?client_order_id=${clientOrderId}`, {
    method: 'DELETE',
  })

/**
 * @returns {Promise<{ user_id: number, cash_avail: number, cash_locked: number, escrow_cash: number, passes: any[] }>}
 */
export const getWallet = () => request('/api/wallet')

/**
 * Top up user wallet in paise (100 paise = ₹1). Maximum 10,000,000 paise (₹100,000).
 *
 * @param {number} amountPaise
 * @returns {Promise<any>}
 */
export const topupWallet = (amountPaise) =>
  request('/api/wallet/topup', {
    method: 'POST',
    headers: jsonHeaders,
    body: JSON.stringify({ amount: amountPaise }),
  })

/**
 * @param {{ market_id?: number, side?: 'buy' | 'sell', limit?: number, cursor?: number }} [query]
 * @returns {Promise<{ items: any[], next_cursor: number | null }>}
 */
export const getHistory = (query = {}) => {
  const params = new URLSearchParams()
  if (query.market_id != null) params.set('market_id', String(query.market_id))
  if (query.side != null) params.set('side', query.side)
  if (query.limit != null) params.set('limit', String(query.limit))
  if (query.cursor != null) params.set('cursor', String(query.cursor))
  const qs = params.toString() ? `?${params.toString()}` : ''

  return request(`/api/history${qs}`)
}

/**
 * @param {{ limit?: number, cursor?: number }} [query]
 * @returns {Promise<{ items: any[], next_cursor: number | null }>}
 */
export const getStatements = (query = {}) => {
  const params = new URLSearchParams()
  if (query.limit != null) params.set('limit', String(query.limit))
  if (query.cursor != null) params.set('cursor', String(query.cursor))
  const qs = params.toString() ? `?${params.toString()}` : ''

  return request(`/api/statements${qs}`)
}

/**
 * The caller's registered ed25519 seller key, if any.
 *
 * @returns {Promise<{ public_key: string | null }>}
 */
export const getEscrowPublicKey = () => request('/api/me/public-key')

/**
 * Register the public half of the seller's delivery-signing key.
 *
 * @param {string} publicKeyHex
 * @returns {Promise<{ public_key: string | null }>}
 */
export const registerEscrowPublicKey = (publicKeyHex) =>
  request('/api/me/public-key', {
    method: 'POST',
    headers: jsonHeaders,
    body: JSON.stringify({ public_key: publicKeyHex }),
  })

/**
 * Escrows where the caller is buyer or seller.
 *
 * @returns {Promise<{ items: any[], now_ms: number }>}
 */
export const getMyEscrow = () => request('/api/me/escrow')

/**
 * Submit the seller's signed delivery receipt.
 *
 * @param {number} tradeId
 * @param {number} deliveredAtMs
 * @param {string} signatureHex
 * @returns {Promise<any>}
 */
export const releaseEscrow = (tradeId, deliveredAtMs, signatureHex) =>
  request(`/api/escrow/${tradeId}/receipt`, {
    method: 'POST',
    headers: jsonHeaders,
    body: JSON.stringify({
      delivered_at_ms: deliveredAtMs,
      signature: signatureHex,
    }),
  })

/**
 * Refund an escrow after its deadline.
 *
 * @param {number} tradeId
 * @returns {Promise<any>}
 */
export const refundEscrow = (tradeId) =>
  request(`/api/escrow/${tradeId}/refund`, {
    method: 'POST',
  })

/**
 * Active and past Dutch auctions, optionally for one market.
 *
 * @param {{ market_id?: number }} [query]
 * @returns {Promise<{ items: any[], now_ms: number }>}
 */
export const getAuctions = (query = {}) => {
  const params = new URLSearchParams()
  if (query.market_id != null) params.set('market_id', String(query.market_id))
  const qs = params.toString() ? `?${params.toString()}` : ''
  return request(`/api/auctions${qs}`)
}

/**
 * Create a seller-backed Dutch auction. The seller's passes are reserved.
 *
 * @param {{ client_order_id: number, market_id: number, qty: number, start_price_ticks: number, floor_price_ticks: number, duration_ms: number }} body
 * @returns {Promise<any>}
 */
export const createAuction = (body) =>
  request('/api/auctions', {
    method: 'POST',
    headers: jsonHeaders,
    body: JSON.stringify(body),
  })

/**
 * Accept a slice of an auction at or below `max_price_ticks`.
 *
 * @param {number} auctionId
 * @param {{ client_order_id: number, qty: number, max_price_ticks: number }} body
 * @returns {Promise<any>}
 */
export const acceptAuction = (auctionId, body) =>
  request(`/api/auctions/${auctionId}/accept`, {
    method: 'POST',
    headers: jsonHeaders,
    body: JSON.stringify(body),
  })

/**
 * Return an auction's unsold passes to the seller after its deadline.
 *
 * @param {number} auctionId
 * @returns {Promise<any>}
 */
export const expireAuction = (auctionId) =>
  request(`/api/auctions/${auctionId}/expire`, {
    method: 'POST',
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

/**
 * @param {string} [state]
 * @returns {Promise<any[]>}
 */
export const getEscrows = (state) => {
  const qs = state ? `?state=${encodeURIComponent(state)}` : ''
  return request(`/api/escrow${qs}`)
}

/**
 * @param {number} id
 * @returns {Promise<any>}
 */
export const getEscrowById = (id) => request(`/api/escrow/${id}`)

/**
 * @param {number} id
 * @returns {Promise<any>}
 */
export const acceptEscrow = (id) =>
  request(`/api/escrow/${id}/accept`, {
    method: 'POST',
  })

/**
 * @param {number} id
 * @param {string} reason
 * @param {string} note
 * @returns {Promise<any>}
 */
export const rejectEscrow = (id, reason, note) =>
  request(`/api/escrow/${id}/reject`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ reason, note }),
  })

/**
 * @param {number} id
 * @returns {Promise<{ valid: boolean }>}
 */
export const verifyEscrowReceipt = (id) =>
  request(`/api/escrow/${id}/verify`, {
    method: 'POST',
  })

/**
 * @returns {Promise<any[]>}
 */
export const getAdminDisputes = () => request('/api/admin/disputes')

/**
 * @param {number} id
 * @param {'buyer' | 'seller'} ruling
 * @param {string} note
 * @returns {Promise<any>}
 */
export const ruleAdminDispute = (id, ruling, note) =>
  request(`/api/admin/disputes/${id}/rule`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ ruling, note }),
  })

