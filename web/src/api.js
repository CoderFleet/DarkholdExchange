// @ts-check
import { getAuthToken } from './authToken.js'
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
const request = async (path, options = {}) => {
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
export const getWallet = async (userId) => {
  try {
    return await request('/api/wallet', {
      headers: { ...authHeaders(userId), 'x-user-id': String(userId) },
    })
  } catch {
    // Graceful fallback to gateway balance endpoint if projection worker is not standalone
    const rawBalances = await getBalances(userId)
    const cash = rawBalances.find((b) => b.asset.kind === 'cash')
    const passes = rawBalances.flatMap((b) => {
      if (b.asset.kind !== 'pass') return []
      return [
        {
          market_id: b.asset.market_id,
          event_name: `Market #${b.asset.market_id}`,
          tier_name: 'General',
          avail: b.available,
          locked: b.held,
          held: 0,
        },
      ]
    })
    return {
      user_id: userId,
      cash_avail: cash?.available ?? 0,
      cash_locked: cash?.held ?? 0,
      escrow_cash: 0,
      passes,
    }
  }
}

/**
 * Top up user wallet in paise (100 paise = ₹1). Maximum 10,000,000 paise (₹100,000).
 *
 * @param {number} userId
 * @param {number} amountPaise
 * @returns {Promise<any>}
 */
export const topupWallet = async (userId, amountPaise) => {
  try {
    return await request('/api/wallet/topup', {
      method: 'POST',
      headers: {
        ...jsonHeaders(userId),
        'x-user-id': String(userId),
      },
      body: JSON.stringify({ amount: amountPaise }),
    })
  } catch {
    // Fallback to gateway dev deposit
    return await request('/api/dev/deposit', {
      method: 'POST',
      headers: jsonHeaders(userId),
      body: JSON.stringify({
        user_id: userId,
        amount: amountPaise,
        client_request_id: Date.now() + Math.floor(Math.random() * 1000),
      }),
    })
  }
}

/**
 * @param {number} userId
 * @param {{ market_id?: number, side?: 'buy' | 'sell', limit?: number, cursor?: number }} [query]
 * @returns {Promise<{ items: any[], next_cursor: number | null }>}
 */
export const getHistory = async (userId, query = {}) => {
  const params = new URLSearchParams()
  if (query.market_id != null) params.set('market_id', String(query.market_id))
  if (query.side != null) params.set('side', query.side)
  if (query.limit != null) params.set('limit', String(query.limit))
  if (query.cursor != null) params.set('cursor', String(query.cursor))
  const qs = params.toString() ? `?${params.toString()}` : ''

  try {
    return await request(`/api/history${qs}`, {
      headers: { ...authHeaders(userId), 'x-user-id': String(userId) },
    })
  } catch {
    try {
      const marketsList = await getMarkets().catch(() => [])
      const marketConfigMap = Object.fromEntries(
        marketsList.map((m) => [m.config.id, m.config]),
      )
      const marketIds = query.market_id
        ? [query.market_id]
        : marketsList.length > 0
          ? marketsList.map((m) => m.config.id)
          : [1, 2, 3]

      const allTradesArrays = await Promise.all(
        marketIds.map((mid) => getTrades(mid, 50).catch(() => [])),
      )

      const allTrades = allTradesArrays.flat()
      const userFills = allTrades
        .filter((f) => f.maker_user_id === userId || f.taker_user_id === userId)
        .map((f) => {
          const isTaker = f.taker_user_id === userId
          const isBuyer =
            (f.maker_side === 'Sell' && isTaker) ||
            (f.maker_side === 'Buy' && !isTaker)

          const cfg = marketConfigMap[f.market_id]
          const tickSize = cfg?.tick_size_minor ?? 100
          const eventName = cfg?.name ?? `Market #${f.market_id}`
          const tierName = cfg ? `Event #${cfg.event_id}` : 'Standard'

          const pricePaise = f.price_ticks * tickSize
          const notionalPaise = pricePaise * f.qty
          // Takers pay fee bps from config (default 100 bps / 1%), makers pay 0 bps
          const feeBps = isTaker ? (cfg?.taker_fee_bps ?? 100) : 0
          const feePaise = Math.floor((notionalPaise * feeBps) / 10_000)
          const netPaise = isBuyer ? notionalPaise + feePaise : notionalPaise - feePaise

          return {
            id: f.trade_id,
            market_id: f.market_id,
            event_name: eventName,
            tier_name: tierName,
            side: isBuyer ? 'buy' : 'sell',
            price: pricePaise,
            qty: f.qty,
            fee: feePaise,
            total_paise: netPaise,
            ts: Date.now() - (f.seq ? (1000 - f.seq) * 60000 : 0),
          }
        })
      return { items: userFills, next_cursor: null }
    } catch {
      return { items: [], next_cursor: null }
    }
  }
}

/**
 * @param {number} userId
 * @param {{ limit?: number, cursor?: number }} [query]
 * @returns {Promise<{ items: any[], next_cursor: number | null }>}
 */
export const getStatements = async (userId, query = {}) => {
  const params = new URLSearchParams()
  if (query.limit != null) params.set('limit', String(query.limit))
  if (query.cursor != null) params.set('cursor', String(query.cursor))
  const qs = params.toString() ? `?${params.toString()}` : ''

  try {
    return await request(`/api/statements${qs}`, {
      headers: { ...authHeaders(userId), 'x-user-id': String(userId) },
    })
  } catch {
    return { items: [], next_cursor: null }
  }
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
