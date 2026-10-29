import { create } from 'zustand'
import {
  getWallet,
  topupWallet,
  getHistory,
  getStatements,
  getMyEscrow,
  getEscrowPublicKey,
  registerEscrowPublicKey,
  releaseEscrow,
  refundEscrow,
} from '../api'
import { generateSellerKey, signDeliveryReceipt } from '../lib/escrowReceipt'
import { getAuthToken } from '../authToken'
import { applyPrivateFrame, isPrivateFrame } from '../feed/walletFeed'
import { createWalletFeedClient } from '../feed/walletFeedClient'
import { useMarketFeedStore } from './useMarketFeedStore'

/** @type {ReturnType<typeof createWalletFeedClient> | null} */
let privateFeedClient = null
/** The user id + token identity the current socket was opened with. */
let privateFeedKey = ''

export const useWalletStore = create((set, get) => ({
  userId: null,
  cashAvail: 0,
  cashLocked: 0,
  escrowCash: 0,
  escrowItems: [],
  escrowNow: 0,
  escrowPublicKey: null,
  escrowLoading: false,
  passes: [],
  historyItems: [],
  statementItems: [],
  loading: false,
  topupLoading: false,
  error: null,
  historyCursor: null,
  statementCursor: null,
  /** @type {import('../feed/walletFeedClient').PrivateFeedStatus} */
  privateFeedStatus: 'idle',
  privateFeedError: null,
  /** The user's most recent own fill, straight from the private feed. */
  lastSelfFill: null,

  setUserId: (/** @type {number | null} */ userId) => {
    set({
      userId,
      historyItems: [],
      statementItems: [],
      escrowItems: [],
      escrowPublicKey: null,
      lastSelfFill: null,
    })
    get().fetchWallet()
    get().fetchEscrow()
    get().connectPrivateFeed()
  },

  resetAccount: () => {
    get().disconnectPrivateFeed()
    set({
      userId: null,
      cashAvail: 0,
      cashLocked: 0,
      escrowCash: 0,
      escrowItems: [],
      escrowNow: 0,
      escrowPublicKey: null,
      escrowLoading: false,
      passes: [],
      historyItems: [],
      statementItems: [],
      loading: false,
      topupLoading: false,
      error: null,
      historyCursor: null,
      statementCursor: null,
      privateFeedStatus: 'idle',
      privateFeedError: null,
      lastSelfFill: null,
    })
  },

  /**
   * Open the per-user private stream. One socket identity = user id + token;
   * logging in or out reconnects it as the new identity.
   */
  connectPrivateFeed: () => {
    const { userId } = get()
    if (userId == null) return
    // The server only opens private state for a token-checked socket. Without
    // a token there is no private feed, not even a dev-mode one, so stay idle.
    const token = getAuthToken()
    if (!token) return
    if (privateFeedClient && privateFeedKey === `${userId}:${token}`) return

    privateFeedKey = `${userId}:${token}`
    set({ privateFeedStatus: 'connecting', privateFeedError: null })
    ensurePrivateFeedClient().connect({ userId, token })
  },

  disconnectPrivateFeed: () => {
    privateFeedKey = ''
    if (privateFeedClient) {
      privateFeedClient.disconnect()
      privateFeedClient = null
    }
    set({ privateFeedStatus: 'idle', privateFeedError: null })
  },

  /** One private frame from the wallet feed worker.
   *
   * @param {import('../protocol/generated/WsServerMessage').WsServerMessage} message
   */
  applyPrivateMessage: (message) => {
    const { userId } = get()
    if (userId == null || !isPrivateFrame(message)) return

    const markets = useMarketFeedStore.getState().markets
    const next = applyPrivateFrame(
      message,
      privateFragment(get()),
      { userId, markets },
    )
    set(next.fragment)
    if (next.needsEscrowRefresh && !get().escrowLoading) {
      // A fill settled into escrow mints a brand-new record that has no push
      // of its own; pick it up over REST once per fill.
      get().fetchEscrow()
    }
  },

  /** Force the wallet fragment back onto the REST truth (dev rescue hatch). */
  rebaselinePrivateFeed: async () => {
    await get().fetchWallet()
    await get().fetchEscrow()
  },

  fetchWallet: async () => {
    const { userId } = get()
    if (userId == null) return
    set({ loading: true, error: null })
    try {
      const data = await getWallet()
      set({
        cashAvail: Number(data.cash_avail ?? 0),
        cashLocked: Number(data.cash_locked ?? 0),
        escrowCash: Number(data.escrow_cash ?? 0),
        passes: data.passes ?? [],
        loading: false,
      })
    } catch (err) {
      set({ error: errorMessage(err), loading: false })
    }
  },

  topup: async (/** @type {number} */ amountRupees) => {
    const { userId } = get()
    if (userId == null) throw new Error('Log in to top up your wallet')
    const amountPaise = Math.round(Number(amountRupees) * 100)
    if (amountPaise <= 0) throw new Error('Top-up amount must be positive')
    if (amountPaise > 10_000_000) throw new Error('Maximum top-up is ₹1,00,000')

    set({ topupLoading: true, error: null })
    try {
      await topupWallet(amountPaise)
      await get().fetchWallet()
      set({ topupLoading: false })
    } catch (err) {
      set({ error: errorMessage(err), topupLoading: false })
      throw err
    }
  },

  fetchEscrow: async () => {
    const { userId } = get()
    if (userId == null) return
    set({ escrowLoading: true, error: null })
    try {
      const [escrow, key] = await Promise.all([
        getMyEscrow(),
        getEscrowPublicKey(),
      ])
      set({
        escrowItems: escrow.items ?? [],
        escrowNow: Number(escrow.now_ms ?? Date.now()),
        escrowPublicKey: key.public_key ?? null,
        escrowLoading: false,
      })
    } catch (err) {
      set({ error: errorMessage(err), escrowLoading: false })
    }
  },

  registerSigningKey: async () => {
    const { userId } = get()
    if (userId == null) throw new Error('Log in to register a signing key')
    set({ escrowLoading: true, error: null })
    try {
      const publicKey = await generateSellerKey(userId)
      const result = await registerEscrowPublicKey(publicKey)
      set({
        escrowPublicKey: result.public_key ?? publicKey,
        escrowLoading: false,
      })
      return publicKey
    } catch (err) {
      set({ error: errorMessage(err), escrowLoading: false })
      throw err
    }
  },

  /**
   * Sign and release one pending delivery.
   *
   * @param {number} tradeId
   */
  deliverEscrow: async (tradeId) => {
    const { userId, escrowItems } = get()
    const record = escrowItems.find(
      (/** @type {{ trade_id: number }} */ item) => item.trade_id === tradeId,
    )
    if (!record) throw new Error('Escrow record not found')
    const deliveredAtMs = Date.now()
    if (deliveredAtMs > record.deadline_ms) {
      throw new Error('The delivery deadline has passed; refund the trade instead.')
    }
    const signature = await signDeliveryReceipt({
      trade_id: record.trade_id,
      market_id: record.market_id,
      qty: record.qty,
      buyer_id: record.buyer_id,
      seller_id: record.seller_id,
      delivered_at_ms: deliveredAtMs,
    }, userId)
    set({ escrowLoading: true, error: null })
    try {
      await releaseEscrow(tradeId, deliveredAtMs, signature)
      await Promise.all([get().fetchEscrow(), get().fetchWallet()])
    } catch (err) {
      set({ error: errorMessage(err), escrowLoading: false })
      throw err
    }
  },

  /**
   * Give up on one pending delivery and take the refund path.
   *
   * @param {number} tradeId
   */
  refundEscrowAction: async (tradeId) => {
    set({ escrowLoading: true, error: null })
    try {
      await refundEscrow(tradeId)
      await Promise.all([get().fetchEscrow(), get().fetchWallet()])
    } catch (err) {
      set({ error: errorMessage(err), escrowLoading: false })
      throw err
    }
  },

  fetchHistory: async (filter = {}) => {
    const { userId } = get()
    if (userId == null) return
    try {
      const res = await getHistory(filter)
      set({
        historyItems: res.items ?? [],
        historyCursor: res.next_cursor ?? null,
      })
    } catch (err) {
      set({ error: errorMessage(err) })
    }
  },

  fetchStatements: async (filter = {}) => {
    const { userId } = get()
    if (userId == null) return
    try {
      const res = await getStatements(filter)
      set({
        statementItems: res.items ?? [],
        statementCursor: res.next_cursor ?? null,
      })
    } catch (err) {
      set({ error: errorMessage(err) })
    }
  },

  /**
   * Project a REST `/api/balances` snapshot into the wallet. Used before the
   * feed was wired; kept for the App boot path.
   *
   * @param {import('../protocol/generated/Balance').Balance[]} balances
   * @param {import('../protocol/generated/MarketConfig').MarketConfig[]} [markets]
   */
  updateFromBalances: (balances, markets = []) => {
    const marketMap = Object.fromEntries((markets || []).map((m) => [m.id, m]))
    const cash = balances.find((b) => b.asset.kind === 'cash')
    const passBalances = balances.flatMap((b) => {
      if (b.asset.kind !== 'pass') return []
      const m = marketMap[b.asset.market_id]
      return [
        {
          market_id: b.asset.market_id,
          event_name: m?.name ?? `Market #${b.asset.market_id}`,
          tier_name: m ? `Event #${m.event_id}` : 'General',
          avail: b.available,
          locked: b.held,
          held: 0,
        },
      ]
    })

    set({
      cashAvail: cash?.available ?? 0,
      cashLocked: cash?.held ?? 0,
      passes: passBalances,
    })
  },
}))

/**
 * @param {unknown} err
 * @returns {string}
 */
const errorMessage = (err) => (err instanceof Error ? err.message : String(err))

/**
 * Lazily create the single wallet-feed worker. Its handlers run for the
 * lifetime of the page and always address the store's latest state.
 */
const ensurePrivateFeedClient = () => {
  if (privateFeedClient) return privateFeedClient

  privateFeedClient = createWalletFeedClient({
    onStatus: (status) => useWalletStore.setState({ privateFeedStatus: status }),
    onMessage: (message) => useWalletStore.getState().applyPrivateMessage(message),
    onError: (message) => useWalletStore.setState({ privateFeedError: message }),
  })
  return privateFeedClient
}

/**
 * The slice of wallet state the private reducer owns; everything else in the
 * store (history, statements, loading flags) stays where it is.
 *
 * @param {ReturnType<typeof useWalletStore.getState>} wallet
 */
const privateFragment = (wallet) => ({
  cashAvail: wallet.cashAvail,
  cashLocked: wallet.cashLocked,
  passes: wallet.passes,
  escrowItems: wallet.escrowItems,
  escrowCash: wallet.escrowCash,
  lastSelfFill: wallet.lastSelfFill,
})
