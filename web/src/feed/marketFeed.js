// @ts-check
/**
 * Pure market-data reducer for the T1 feed.
 *
 * The worker owns the socket and MessagePack decoding. This module owns the
 * state transitions so snapshot/delta/resync behavior is deterministic and can
 * be tested without a browser or a live gateway.
 */

/** @typedef {import('../protocol/generated/BookLevel').BookLevel} BookLevel */
/** @typedef {import('../protocol/generated/BookSnapshot').BookSnapshot} BookSnapshot */
/** @typedef {import('../protocol/generated/Fill').Fill} Fill */
/** @typedef {import('../protocol/generated/MetricsSnapshot').MetricsSnapshot} MetricsSnapshot */
/** @typedef {import('../protocol/generated/Side').Side} Side */
/** @typedef {import('../protocol/generated/WsServerMessage').WsServerMessage} WsServerMessage */
/** @typedef {import('../protocol/generated/BookDelta').BookDelta} BookDelta */

/**
 * @typedef {{
 *   activeMarketId: number | null,
 *   snapshots: Record<number, BookSnapshot>,
 *   trades: Record<number, Fill[]>,
 *   metrics: MetricsSnapshot | null,
 *   resyncing: boolean,
 *   lastTrade: Fill | null,
 *   error: string | null,
 * }} FeedState
 */

/**
 * @typedef {{
 *   state: FeedState,
 *   needsResync: boolean,
 * }} FeedResult
 */

/** @type {number} */
export const DEFAULT_MAX_TRADES = 200

/**
 * @param {number | null} [activeMarketId]
 * @returns {FeedState}
 */
export const createFeedState = (activeMarketId = null) => ({
  activeMarketId,
  snapshots: {},
  trades: {},
  metrics: null,
  resyncing: false,
  lastTrade: null,
  error: null,
})

/**
 * Keep one trade per `trade_id`, sorted by command sequence, and bounded.
 *
 * @param {Fill[]} existing
 * @param {Fill[]} incoming
 * @param {number} [maxTrades]
 * @returns {Fill[]}
 */
export const mergeTrades = (
  existing,
  incoming,
  maxTrades = DEFAULT_MAX_TRADES,
) => {
  /** @type {Map<number, Fill>} */
  const byId = new Map()
  for (const fill of existing) byId.set(fill.trade_id, fill)
  for (const fill of incoming) byId.set(fill.trade_id, fill)

  return [...byId.values()]
    .sort((left, right) => left.seq - right.seq || left.trade_id - right.trade_id)
    .slice(-maxTrades)
}

/**
 * @param {Side} side
 * @param {BookLevel[]} levels
 * @returns {BookLevel[]}
 */
const sortLevels = (side, levels) => {
  const direction = side === 'Buy' ? -1 : 1
  return levels.sort(
    (left, right) => direction * (left.price_ticks - right.price_ticks),
  )
}

/**
 * Apply absolute level changes for one side of the book.
 *
 * @param {Side} side
 * @param {BookLevel[]} levels
 * @param {import('../protocol/generated/LevelChange').LevelChange[]} changes
 * @returns {BookLevel[]}
 */
const applyLevelChanges = (side, levels, changes) => {
  /** @type {Map<number, number>} */
  const quantities = new Map()
  for (const level of levels) quantities.set(level.price_ticks, level.qty)

  for (const change of changes) {
    if (change.side !== side) continue
    if (change.qty === 0) {
      quantities.delete(change.price_ticks)
    } else if (change.qty > 0) {
      quantities.set(change.price_ticks, change.qty)
    }
  }

  return sortLevels(
    side,
    [...quantities.entries()].map(([price_ticks, qty]) => ({
      price_ticks,
      qty,
    })),
  )
}

/**
 * Remove a market's baseline after a detected gap. The next Snapshot rebuilds
 * it; until then deltas are deliberately not applied.
 *
 * @param {FeedState} state
 * @param {number} marketId
 * @returns {FeedState}
 */
const clearMarket = (state, marketId) => {
  /** @type {Record<number, BookSnapshot>} */
  const snapshots = { ...state.snapshots }
  delete snapshots[marketId]
  return { ...state, snapshots, resyncing: true }
}

/**
 * @param {FeedState} state
 * @param {BookDelta} delta
 * @param {number} [depth]
 * @returns {FeedResult}
 */
export const applyDelta = (state, delta, depth = 50) => {
  const snapshot = state.snapshots[delta.market_id]
  if (!snapshot) {
    // A resync is already in flight. Deltas cannot be applied until its
    // Snapshot arrives; ignoring them avoids a Subscribe storm while waiting.
    if (state.resyncing) return { state, needsResync: false }
    return { state: clearMarket(state, delta.market_id), needsResync: true }
  }

  // Older frames can arrive after a newer baseline. They must not roll the
  // absolute quantities backwards.
  if (delta.book_seq < snapshot.book_seq) {
    return { state, needsResync: false }
  }

  if (delta.book_seq > snapshot.book_seq + 1) {
    return { state: clearMarket(state, delta.market_id), needsResync: true }
  }

  const nextSnapshot = {
    ...snapshot,
    book_seq: delta.book_seq,
    bids: applyLevelChanges('Buy', snapshot.bids, delta.changes).slice(0, depth),
    asks: applyLevelChanges('Sell', snapshot.asks, delta.changes).slice(0, depth),
  }

  return {
    state: {
      ...state,
      snapshots: {
        ...state.snapshots,
        [delta.market_id]: nextSnapshot,
      },
    },
    needsResync: false,
  }
}

/**
 * @param {FeedState} state
 * @param {BookSnapshot} snapshot
 * @param {number} [depth]
 * @returns {FeedState}
 */
export const applySnapshot = (state, snapshot, depth = 50) => {
  const nextSnapshot = {
    ...snapshot,
    bids: snapshot.bids.slice(0, depth),
    asks: snapshot.asks.slice(0, depth),
  }
  return {
    ...state,
    snapshots: { ...state.snapshots, [snapshot.market_id]: nextSnapshot },
    resyncing: false,
  }
}

/**
 * @param {FeedState} state
 * @param {Fill} fill
 * @param {number} [maxTrades]
 * @returns {FeedState}
 */
export const applyTrade = (state, fill, maxTrades = DEFAULT_MAX_TRADES) => {
  const previous = state.trades[fill.market_id] ?? []
  const snapshot = state.snapshots[fill.market_id]
  return {
    ...state,
    snapshots: snapshot
      ? {
          ...state.snapshots,
          [fill.market_id]: {
            ...snapshot,
            last_trade_price: fill.price_ticks,
          },
        }
      : state.snapshots,
    trades: {
      ...state.trades,
      [fill.market_id]: mergeTrades(previous, [fill], maxTrades),
    },
    lastTrade: fill,
  }
}

/**
 * @param {FeedState} state
 * @param {WsServerMessage} message
 * @param {{ depth?: number, maxTrades?: number }} [options]
 * @returns {FeedResult}
 */
export const applyServerMessage = (state, message, options = {}) => {
  const depth = options.depth ?? 50
  const maxTrades = options.maxTrades ?? DEFAULT_MAX_TRADES

  switch (message.type) {
    case 'Snapshot':
      return {
        state: applySnapshot(state, message.snapshot, depth),
        needsResync: false,
      }
    case 'Delta':
      return applyDelta(state, message.delta, depth)
    case 'Trade':
      return {
        state: applyTrade(state, message.fill, maxTrades),
        needsResync: false,
      }
    case 'ResyncRequired':
      return {
        state: clearMarket(state, message.market_id),
        needsResync: false,
      }
    case 'Metrics':
      return {
        state: { ...state, metrics: message.metrics },
        needsResync: false,
      }
    case 'Error':
      return {
        state: { ...state, error: message.code },
        needsResync: false,
      }
    case 'Hello':
    case 'Balances':
    case 'Pong':
      return { state, needsResync: false }
    default:
      return { state, needsResync: false }
  }
}
