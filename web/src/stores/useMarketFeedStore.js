// @ts-check
/**
 * Zustand owner for T1 market data.
 *
 * REST hydrates the market list and an initial book. The worker-backed feed then
 * owns the selected market's snapshot/deltas/tape. Components only read this
 * store; they never open a WebSocket or decode MessagePack themselves.
 */

import { create } from 'zustand'
import { createMarketFeedClient } from '../feed/marketFeedClient'
import { getAuthToken } from '../authToken'
import { applyServerMessage, mergeTrades } from '../feed/marketFeed'

/** @typedef {import('../protocol/generated/BookSnapshot').BookSnapshot} BookSnapshot */
/** @typedef {import('../protocol/generated/Fill').Fill} Fill */
/** @typedef {import('../protocol/generated/MarketConfig').MarketConfig} MarketConfig */
/** @typedef {import('../protocol/generated/MarketView').MarketView} MarketView */
/** @typedef {import('../protocol/generated/MetricsSnapshot').MetricsSnapshot} MetricsSnapshot */
/** @typedef {import('../feed/marketFeedClient').FeedStatus} FeedStatus */

const FEED_DEPTH = 50
const MAX_TRADES = 200

/** @type {ReturnType<typeof createMarketFeedClient> | null} */
let client = null
let activeKey = ''

export const useMarketFeedStore = create((set, get) => {
  const ensureClient = () => {
    if (client) return client

    client = createMarketFeedClient({
      onStatus: (status) => set({ status }),
      onMessage: (message, marketId) => {
        const current = get()
        if (current.activeMarketId !== marketId) return

        const result = applyServerMessage(
          {
            activeMarketId: current.activeMarketId,
            snapshots: current.snapshots,
            trades: current.trades,
            metrics: current.metrics,
            resyncing: current.resyncing,
            lastTrade: current.lastTrade,
            error: current.error,
          },
          message,
          { depth: FEED_DEPTH, maxTrades: MAX_TRADES },
        )

        set(result.state)
        if (result.needsResync) client?.resync()
      },
      onError: (message) => set({ error: message }),
    })

    return client
  }

  return {
    /** @type {MarketConfig[]} */
    markets: [],
    /** @type {Record<number, BookSnapshot>} */
    snapshots: {},
    /** @type {Record<number, Fill[]>} */
    trades: {},
    /** @type {FeedStatus} */
    status: 'idle',
    /** @type {number | null} */
    activeMarketId: null,
    /** @type {MetricsSnapshot | null} */
    metrics: null,
    resyncing: false,
    /** @type {Fill | null} */
    lastTrade: null,
    /** @type {string | null} */
    error: null,

    /**
     * Seed the market list and REST snapshots without overwriting anything the
     * live feed has already received.
     *
     * @param {MarketView[]} views
     */
    hydrate: (views) => {
      const markets = views.map((view) => view.config)
      const restSnapshots = Object.fromEntries(
        views.map((view) => [view.config.id, view.snapshot]),
      )
      const current = get()
      set({
        markets,
        snapshots: { ...restSnapshots, ...current.snapshots },
      })
    },

    /**
     * Merge REST or command-response fills into the tape. The feed and REST can
     * race; `trade_id` de-duplication makes the order irrelevant.
     *
     * @param {number} marketId
     * @param {Fill[]} incoming
     */
    seedTrades: (marketId, incoming) => {
      if (!Number.isFinite(marketId) || !Array.isArray(incoming)) return
      const current = get()
      set({
        trades: {
          ...current.trades,
          [marketId]: mergeTrades(
            current.trades[marketId] ?? [],
            incoming,
            MAX_TRADES,
          ),
        },
      })
    },

    /**
     * Follow the canonical fixed-market feed.
     *
     * @param {number} userId
     * @param {number} marketId
     * @param {number} [depth]
     */
    connect: (userId, marketId, depth = FEED_DEPTH) => {
      // The worker has no access to the login token, so it is handed over here. It is part of
      // the key so logging in or out reconnects the feed as the new identity.
      const token = getAuthToken()
      const key = `${userId}:${marketId}:${depth}:${token ?? ''}`
      if (client && activeKey === key) return

      const previous = get()
      const sameMarket = previous.activeMarketId === marketId
      activeKey = key
      set({
        activeMarketId: marketId,
        status: 'connecting',
        resyncing: false,
        error: null,
        lastTrade: null,
        metrics: sameMarket ? previous.metrics : null,
      })
      ensureClient().connect({ userId, marketId, depth, token })
    },

    disconnect: () => {
      activeKey = ''
      client?.disconnect()
      set({
        status: 'offline',
        activeMarketId: null,
        resyncing: false,
      })
    },

    /** Request a fresh snapshot after a client-detected sequence gap. */
    resync: () => {
      client?.resync()
    },
  }
})
