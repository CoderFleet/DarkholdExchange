import { create } from 'zustand'
import { getWallet, topupWallet, getHistory, getStatements } from '../api'

export const useWalletStore = create((set, get) => ({
  userId: 1,
  cashAvail: 0,
  cashLocked: 0,
  escrowCash: 0,
  passes: [],
  historyItems: [],
  statementItems: [],
  loading: false,
  topupLoading: false,
  error: null,
  historyCursor: null,
  statementCursor: null,

  setUserId: (userId) => {
    set({ userId, historyItems: [], statementItems: [] })
    get().fetchWallet()
  },

  fetchWallet: async () => {
    const { userId } = get()
    set({ loading: true, error: null })
    try {
      const data = await getWallet(userId)
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
    const amountPaise = Math.round(Number(amountRupees) * 100)
    if (amountPaise <= 0) throw new Error('Top-up amount must be positive')
    if (amountPaise > 10_000_000) throw new Error('Maximum top-up is ₹1,00,000')

    set({ topupLoading: true, error: null })
    try {
      await topupWallet(userId, amountPaise)
      // Optimistically update cash and then sync full wallet
      set((state) => ({
        cashAvail: state.cashAvail + amountPaise,
        topupLoading: false,
      }))
      await get().fetchWallet()
    } catch (err) {
      set({ error: err.message, topupLoading: false })
      throw err
    }
  },

  fetchHistory: async (filter = {}) => {
    const { userId } = get()
    try {
      const res = await getHistory(userId, filter)
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
    try {
      const res = await getStatements(userId, filter)
      set({
        statementItems: res.items ?? [],
        statementCursor: res.next_cursor ?? null,
      })
    } catch (err) {
      set({ error: err.message })
    }
  },

  updateFromBalances: (balances) => {
    const cash = balances.find((b) => b.asset.kind === 'cash')
    const passBalances = balances
      .filter((b) => b.asset.kind === 'pass')
      .map((b) => ({
        market_id: b.asset.market_id,
        event_name: `Market #${b.asset.market_id}`,
        tier_name: 'General',
        avail: b.available,
        locked: b.held,
        held: 0,
      }))

    set({
      cashAvail: cash?.available ?? 0,
      cashLocked: cash?.held ?? 0,
      passes: passBalances,
    })
  },
}))
