import { beforeEach, describe, expect, it, vi } from 'vitest'
import { useWalletStore } from './useWalletStore'
import { setAuthToken } from '../authToken'
import { useMarketFeedStore } from './useMarketFeedStore'
import * as api from '../api'

// Replace the network calls the store makes while a user signs in.
vi.mock('../api', () => ({
  getWallet: vi.fn(),
  topupWallet: vi.fn(),
  getHistory: vi.fn(),
  getStatements: vi.fn(),
  getMyEscrow: vi.fn(),
  getEscrowPublicKey: vi.fn(),
  registerEscrowPublicKey: vi.fn(),
  releaseEscrow: vi.fn(),
  refundEscrow: vi.fn(),
}))

// No real worker: capture the handlers so the tests can push private frames.
const feed = {
  handlers: null,
  connect: vi.fn(),
  disconnect: vi.fn(),
  dispose: vi.fn(),
}
vi.mock('../feed/walletFeedClient', () => ({
  createWalletFeedClient: vi.fn((handlers) => {
    feed.handlers = handlers
    return { connect: feed.connect, disconnect: feed.disconnect, dispose: feed.dispose }
  }),
}))

const token = 'tok-9'
const walletView = {
  cash_avail: 1_000,
  cash_locked: 0,
  escrow_cash: 0,
  passes: [
    {
      market_id: 1,
      event_name: 'Day 1 General',
      tier_name: 'General',
      avail: 3,
      locked: 0,
      held: 0,
    },
  ],
}

beforeEach(async () => {
  vi.clearAllMocks()
  api.getWallet.mockResolvedValue(walletView)
  api.getMyEscrow.mockResolvedValue({ items: [], now_ms: 1_000 })
  api.getEscrowPublicKey.mockResolvedValue({ public_key: null })
  api.getHistory.mockResolvedValue({ items: [], next_cursor: null })
  api.getStatements.mockResolvedValue({ items: [], next_cursor: null })
  useMarketFeedStore.setState({ markets: [] })
  setAuthToken(token)
  useWalletStore.getState().resetAccount()
  // resetAccount disposes the feed; a fresh connect captures handlers again.
  await new Promise((resolve) => setTimeout(resolve, 0))
})

const settleUser = async (userId) => {
  useWalletStore.getState().setUserId(userId)
  await vi.waitFor(() => {
    expect(useWalletStore.getState().loading).toBe(false)
    expect(useWalletStore.getState().escrowLoading).toBe(false)
  })
}

const balancesFrame = (overrides = {}) => ({
  type: 'Balances',
  user_id: 9,
  balances: [
    { asset: { kind: 'cash' }, available: 250_000, held: 4_000 },
    { asset: { kind: 'pass', market_id: 1 }, available: 2, held: 3 },
  ],
  ...overrides,
})

const escrowRecord = (overrides = {}) => ({
  trade_id: 31,
  market_id: 1,
  buyer_id: 9,
  seller_id: 2,
  qty: 2,
  notional: 100_000,
  fee: 1_500,
  fee_payer: 9,
  state: 'pending_delivery',
  created_at_ms: 1_000,
  deadline_ms: 301_000,
  delivered_at_ms: null,
  seller_public_key: [26],
  ...overrides,
})

describe("the wallet store's private feed", () => {
  it('opens one socket per user identity and closes it on logout', async () => {
    await settleUser(9)
    expect(feed.connect).toHaveBeenCalledWith({ userId: 9, token })
    expect(useWalletStore.getState().privateFeedStatus).toBe('connecting')

    feed.handlers.onStatus('live')
    expect(useWalletStore.getState().privateFeedStatus).toBe('live')

    // A repeated setUserId must not stack a second socket.
    await settleUser(9)
    expect(feed.connect).toHaveBeenCalledTimes(1)

    feed.handlers.onStatus('offline')
    useWalletStore.getState().resetAccount()
    expect(feed.disconnect).toHaveBeenCalled()
    expect(useWalletStore.getState().privateFeedStatus).toBe('idle')
  })

  it('does not open the private feed without a login token', async () => {
    setAuthToken(null)
    await settleUser(9)
    expect(feed.connect).not.toHaveBeenCalled()
    expect(useWalletStore.getState().privateFeedStatus).toBe('idle')
  })

  it('updates cash and passes from a balances frame', async () => {
    await settleUser(9)
    feed.handlers.onMessage(balancesFrame())

    const state = useWalletStore.getState()
    expect(state.cashAvail).toBe(250_000)
    expect(state.cashLocked).toBe(4_000)
    expect(state.passes).toEqual([
      {
        market_id: 1,
        event_name: 'Day 1 General',
        tier_name: 'General',
        avail: 2,
        locked: 3,
        held: 0,
      },
    ])
  })

  it('names a pass row after the market when the push introduces one', async () => {
    useMarketFeedStore.setState({
      markets: [{ id: 2, name: 'Obsidian Sigil', event_id: 12 }],
    })
    await settleUser(9)
    feed.handlers.onMessage(
      balancesFrame({
        balances: [
          { asset: { kind: 'pass', market_id: 2 }, available: 1, held: 0 },
        ],
      }),
    )
    expect(useWalletStore.getState().passes).toEqual([
      {
        market_id: 1,
        event_name: 'Day 1 General',
        tier_name: 'General',
        avail: 3,
        locked: 0,
        held: 0,
      },
      {
        market_id: 2,
        event_name: 'Obsidian Sigil',
        tier_name: 'General',
        avail: 1,
        locked: 0,
        held: 0,
      },
    ])
  })

  it('ignores a balances frame addressed to someone else', async () => {
    await settleUser(9)
    const before = useWalletStore.getState()
    feed.handlers.onMessage(balancesFrame({ user_id: 5 }))
    expect(useWalletStore.getState().cashAvail).toBe(before.cashAvail)
    expect(useWalletStore.getState().passes).toEqual(before.passes)
  })

  it('upserts escrow records and recounts the escrow cash and held passes', async () => {
    await settleUser(9)
    feed.handlers.onMessage({ type: 'EscrowUpdate', record: escrowRecord() })

    const state = useWalletStore.getState()
    expect(state.escrowItems).toHaveLength(1)
    expect(state.escrowCash).toBe(101_500)
    expect(state.passes[0]).toMatchObject({ market_id: 1, avail: 3, locked: 0, held: 2 })

    // A transition to delivered both reverts the holds and empties the cash.
    feed.handlers.onMessage({
      type: 'EscrowUpdate',
      record: escrowRecord({ state: 'delivered', delivered_at_ms: 9_000 }),
    })
    expect(useWalletStore.getState().escrowCash).toBe(0)
    expect(useWalletStore.getState().passes[0].held).toBe(0)
  })

  it('refreshes escrow over REST when an own fill was escrowed', async () => {
    await settleUser(9)
    await new Promise((resolve) => setTimeout(resolve, 0))
    expect(api.getMyEscrow).toHaveBeenCalledTimes(1)

    feed.handlers.onMessage({
      type: 'FillSelf',
      fill: { seq: 77, escrow_id: 5 },
    })
    await vi.waitFor(() => expect(api.getMyEscrow).toHaveBeenCalledTimes(2))

    // Spot fills must not touch the escrow endpoint.
    feed.handlers.onMessage({
      type: 'FillSelf',
      fill: { seq: 78 },
    })
    await new Promise((resolve) => setTimeout(resolve, 0))
    expect(api.getMyEscrow).toHaveBeenCalledTimes(2)
    expect(useWalletStore.getState().lastSelfFill).toEqual({ seq: 78 })

    // Public market frames are ignored on the private store as well.
    feed.handlers.onMessage({ type: 'Snapshot' })
    expect(useWalletStore.getState().lastSelfFill).not.toEqual({ type: 'Snapshot' })

    await settleUser(9) // keeps feed.connect stable for one-socket test
  })
})
