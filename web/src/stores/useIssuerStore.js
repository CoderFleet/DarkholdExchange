import { create } from 'zustand'
import { getSupply, issuePasses, issuerErrorText } from '../issuerApi'

// A number the server uses to recognise a repeated request. Milliseconds times 1000 plus a
// random part stays far below the biggest number JavaScript can hold exactly.
const newRequestId = () => Date.now() * 1000 + Math.floor(Math.random() * 1000)

const plural = (qty) => (qty === 1 ? 'pass' : 'passes')

export const useIssuerStore = create((set, get) => ({
  supply: [],
  // 'loading' until the first list arrives, then 'ready'.
  status: 'loading',
  error: null,
  // A sentence about the last successful issue, shown once.
  notice: null,
  busy: false,
  // The request number of an issue whose answer we never saw (the connection dropped). If the
  // issuer presses the button again for the same user, market and quantity, the same number is
  // sent, so the server cannot credit the passes twice.
  pending: null,

  load: async () => {
    try {
      set({ supply: await getSupply(), status: 'ready', error: null })
    } catch (err) {
      set({ status: 'ready', error: issuerErrorText(err) })
    }
  },

  // Returns true when the passes were credited, so the form knows to clear itself.
  issue: async (username, marketId, qty) => {
    const name = username.trim()
    const key = `${name.toLowerCase()}|${marketId}|${qty}`
    const pending = get().pending
    const requestId = pending?.key === key ? pending.requestId : newRequestId()
    set({ busy: true, error: null, notice: null, pending: { key, requestId } })
    try {
      await issuePasses({ username: name, marketId, qty, requestId })
    } catch (err) {
      // An answer from the server (it has a status) means the request was judged and refused,
      // so a fresh number is fine next time. No answer means we cannot know, so keep the number.
      set({ busy: false, error: issuerErrorText(err), pending: err?.status ? null : { key, requestId } })
      return false
    }
    const market = get().supply.find((row) => row.market_id === marketId)
    set({
      busy: false,
      pending: null,
      notice: `Credited ${qty} ${plural(qty)} of ${market?.name ?? `market ${marketId}`} to ${name}.`,
    })
    // Show the new totals. If this fails the credit still happened, so only note the problem.
    try {
      set({ supply: await getSupply() })
    } catch (err) {
      set({ error: issuerErrorText(err) })
    }
    return true
  },

  clearMessages: () => set({ error: null, notice: null }),
}))