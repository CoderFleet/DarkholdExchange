import { describe, expect, it } from 'vitest'
import {
  applyDelta,
  applyServerMessage,
  applySnapshot,
  applyTrade,
  createFeedState,
  mergeTrades,
} from './marketFeed.js'

const snapshot = {
  market_id: 1,
  book_seq: 7,
  bids: [
    { price_ticks: 5000, qty: 3 },
    { price_ticks: 4900, qty: 2 },
  ],
  asks: [
    { price_ticks: 5100, qty: 2 },
    { price_ticks: 5200, qty: 4 },
  ],
  last_trade_price: 5050,
}

const filledState = () => ({
  ...createFeedState(1),
  snapshots: { 1: snapshot },
})

const fill = (overrides = {}) => ({
  trade_id: 11,
  market_id: 1,
  maker_order_id: 1,
  taker_order_id: 2,
  maker_user_id: 1,
  taker_user_id: 2,
  maker_side: 'Buy',
  price_ticks: 5050,
  qty: 1,
  seq: 9,
  ...overrides,
})

describe('market feed reducer', () => {
  it('replaces a baseline and clears the resyncing flag', () => {
    const state = applySnapshot(
      { ...createFeedState(1), resyncing: true },
      snapshot,
    )
    expect(state.snapshots[1]).toEqual(snapshot)
    expect(state.resyncing).toBe(false)
  })

  it('applies absolute level changes, removes zero levels, and sorts', () => {
    const result = applyServerMessage(filledState(), {
      type: 'Delta',
      delta: {
        market_id: 1,
        book_seq: 8,
        changes: [
          { side: 'Buy', price_ticks: 5000, qty: 5 },
          { side: 'Buy', price_ticks: 4800, qty: 1 },
          { side: 'Sell', price_ticks: 5100, qty: 0 },
          { side: 'Sell', price_ticks: 5150, qty: 3 },
        ],
      },
    })

    expect(result.needsResync).toBe(false)
    expect(result.state.snapshots[1].book_seq).toBe(8)
    expect(result.state.snapshots[1].bids).toEqual([
      { price_ticks: 5000, qty: 5 },
      { price_ticks: 4900, qty: 2 },
      { price_ticks: 4800, qty: 1 },
    ])
    expect(result.state.snapshots[1].asks).toEqual([
      { price_ticks: 5150, qty: 3 },
      { price_ticks: 5200, qty: 4 },
    ])
  })

  it('treats a repeated sequence as idempotent', () => {
    const delta = {
      market_id: 1,
      book_seq: 7,
      changes: [{ side: 'Buy', price_ticks: 5000, qty: 9 }],
    }

    const once = applyDelta(filledState(), delta)
    const twice = applyDelta(once.state, delta)
    expect(once.state.snapshots[1].book_seq).toBe(7)
    expect(twice.state.snapshots[1].bids[0]).toEqual({
      price_ticks: 5000,
      qty: 9,
    })
  })

  it('ignores a stale delta instead of rolling the book backwards', () => {
    const state = filledState()
    const result = applyDelta(state, {
      market_id: 1,
      book_seq: 6,
      changes: [{ side: 'Buy', price_ticks: 5000, qty: 99 }],
    })

    expect(result.state).toBe(state)
    expect(result.needsResync).toBe(false)
  })

  it('clears the book and requests a resync on a sequence gap', () => {
    const result = applyDelta(filledState(), {
      market_id: 1,
      book_seq: 9,
      changes: [{ side: 'Buy', price_ticks: 5000, qty: 1 }],
    })

    expect(result.needsResync).toBe(true)
    expect(result.state.resyncing).toBe(true)
    expect(result.state.snapshots[1]).toBeUndefined()

    const whileResyncing = applyDelta(result.state, {
      market_id: 1,
      book_seq: 10,
      changes: [{ side: 'Buy', price_ticks: 5000, qty: 2 }],
    })
    expect(whileResyncing.needsResync).toBe(false)
    expect(whileResyncing.state.resyncing).toBe(true)
  })

  it('clears the book when the server sends ResyncRequired', () => {
    const result = applyServerMessage(filledState(), {
      type: 'ResyncRequired',
      market_id: 1,
    })

    expect(result.needsResync).toBe(false)
    expect(result.state.resyncing).toBe(true)
    expect(result.state.snapshots[1]).toBeUndefined()
  })

  it('recovers after a fresh snapshot following a gap', () => {
    const gap = applyDelta(filledState(), {
      market_id: 1,
      book_seq: 10,
      changes: [{ side: 'Buy', price_ticks: 5000, qty: 1 }],
    })
    const recovered = applyServerMessage(gap.state, {
      type: 'Snapshot',
      snapshot: { ...snapshot, book_seq: 10 },
    })

    expect(recovered.state.resyncing).toBe(false)
    expect(recovered.state.snapshots[1].book_seq).toBe(10)
  })

  it('de-duplicates trades and updates the last-trade price', () => {
    const trade = { type: 'Trade', fill: fill() }
    const once = applyServerMessage(filledState(), trade)
    const twice = applyServerMessage(once.state, trade)

    expect(twice.state.trades[1]).toHaveLength(1)
    expect(twice.state.trades[1][0].trade_id).toBe(11)
    expect(twice.state.snapshots[1].last_trade_price).toBe(5050)
    expect(twice.state.lastTrade).toEqual(fill())
  })

  it('updates the tape when a trade arrives before a snapshot', () => {
    const result = applyTrade(createFeedState(1), fill())

    expect(result.trades[1]).toHaveLength(1)
    expect(result.snapshots[1]).toBeUndefined()
    expect(result.lastTrade).toEqual(fill())
  })

  it('keeps trades ordered by command sequence and bounded', () => {
    const merged = mergeTrades(
      [fill({ trade_id: 2, seq: 8 })],
      [fill({ trade_id: 1, seq: 7 }), fill({ trade_id: 2, seq: 8 })],
      2,
    )

    expect(merged.map((trade) => trade.trade_id)).toEqual([1, 2])
    expect(
      mergeTrades(
        [fill({ trade_id: 1 }), fill({ trade_id: 2 })],
        [fill({ trade_id: 3 })],
        2,
      ).map((trade) => trade.trade_id),
    ).toEqual([2, 3])
  })
})
