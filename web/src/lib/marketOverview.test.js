import { describe, expect, it } from 'vitest'
import {
  bestAskTicks,
  bestBidTicks,
  filterMarketViews,
  marketOverview,
  spreadTicks,
} from './marketOverview'

const view = (id, name, escrow, last) => ({
  config: {
    id,
    event_id: Math.ceil(id / 2),
    name,
    escrow_enabled: escrow,
  },
  snapshot: {
    last_trade_price: last,
    bids: [{ price_ticks: 4_900, qty: 2 }],
    asks: [{ price_ticks: 5_100, qty: 3 }],
  },
})

const views = [
  view(2, 'Semi-final', false, null),
  view(1, 'Day 1 General', false, 5_000),
  view(3, 'Relic: Obsidian Sigil', true, 4_200),
]

describe('top of book helpers', () => {
  it('reads the best levels and spread', () => {
    const snapshot = views[0].snapshot
    expect(bestBidTicks(snapshot)).toBe(4_900)
    expect(bestAskTicks(snapshot)).toBe(5_100)
    expect(spreadTicks(snapshot)).toBe(200)
  })

  it('returns null for an empty or missing book', () => {
    expect(bestBidTicks(null)).toBeNull()
    expect(spreadTicks({ bids: [], asks: [] })).toBeNull()
    expect(spreadTicks({ bids: [{ price_ticks: 100 }], asks: [] })).toBeNull()
  })
})

describe('filterMarketViews', () => {
  it('sorts by market id', () => {
    expect(filterMarketViews(views).map((item) => item.config.id)).toEqual([1, 2, 3])
  })

  it('matches name, id and event text', () => {
    expect(filterMarketViews(views, { query: 'obsidian' }).map((item) => item.config.id)).toEqual([3])
    expect(filterMarketViews(views, { query: 'm2' }).map((item) => item.config.id)).toEqual([2])
    expect(filterMarketViews(views, { query: 'event 1' }).map((item) => item.config.id)).toEqual([1, 2])
  })

  it('filters escrow and spot markets', () => {
    expect(filterMarketViews(views, { kind: 'escrow' }).map((item) => item.config.id)).toEqual([3])
    expect(filterMarketViews(views, { kind: 'spot' }).map((item) => item.config.id)).toEqual([1, 2])
  })
})

describe('marketOverview', () => {
  it('counts escrow, spot, and markets with prints', () => {
    expect(marketOverview(views)).toEqual({
      total: 3,
      escrow: 1,
      spot: 2,
      withPrints: 2,
    })
  })
})
