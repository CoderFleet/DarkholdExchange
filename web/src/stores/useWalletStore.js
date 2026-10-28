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

  setUserId: (userId) => {
    set({
      userId,
      historyItems: [],
      statementItems: [],
      escrowItems: [],
      escrowPublicKey: null,
    })
    get().fetchWallet()
  },

  resetAccount: () => {
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
    })
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
      set({ error: err.message, loading: false })
    }
  },

  topup: async (amountRupees) => {
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
      set({ error: err.message, topupLoading: false })
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
      set({ error: err.message, escrowLoading: false })
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
      set({ error: err.message, escrowLoading: false })
      throw err
    }
  },

  deliverEscrow: async (tradeId) => {
    const { userId, escrowItems } = get()
    const record = escrowItems.find((item) => item.trade_id === tradeId)
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
      set({ error: err.message, escrowLoading: false })
      throw err
    }
  },

  refundEscrowAction: async (tradeId) => {
    set({ escrowLoading: true, error: null })
    try {
      await refundEscrow(tradeId)
      await Promise.all([get().fetchEscrow(), get().fetchWallet()])
    } catch (err) {
      set({ error: err.message, escrowLoading: false })
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
      set({ error: err.message })
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
      set({ error: err.message })
    }
  },

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
