import { describe, expect, it } from 'vitest'
import {
  buildDepthSeries,
  buildTradeChartData,
  padDomain,
} from './chartData.js'

const level = (price_ticks, qty) => ({ price_ticks, qty })

describe('chart data', () => {
  it('sorts book levels and builds cumulative depth from the best price', () => {
    const series = buildDepthSeries(
      [level(4_900, 2), level(5_000, 3), level(4_800, 0)],
      [level(5_200, 4), level(5_100, 2)],
    )

    expect(series.bids).toEqual([
      { priceTicks: 5_000, qty: 3, cumulative: 3 },
      { priceTicks: 4_900, qty: 2, cumulative: 5 },
    ])
    expect(series.asks).toEqual([
      { priceTicks: 5_100, qty: 2, cumulative: 2 },
      { priceTicks: 5_200, qty: 4, cumulative: 6 },
    ])
    expect(series.bestBid).toBe(5_000)
    expect(series.bestAsk).toBe(5_100)
    expect(series.minPrice).toBe(4_900)
    expect(series.maxPrice).toBe(5_200)
    expect(series.totalBidQty).toBe(5)
    expect(series.totalAskQty).toBe(6)
    expect(series.maxCumulative).toBe(6)
  })

  it('returns an empty but usable domain for an empty book', () => {
    const series = buildDepthSeries(undefined, null)

    expect(series.bids).toEqual([])
    expect(series.asks).toEqual([])
    expect(series.bestBid).toBeNull()
    expect(series.bestAsk).toBeNull()
    expect(series.minPrice).toBeNull()
    expect(series.maxPrice).toBeNull()
    expect(series.maxCumulative).toBe(1)
  })

  it('orders trades by sequence and keeps the newest window', () => {
    const trade = (trade_id, seq, price_ticks, qty = 1) => ({
      trade_id,
      seq,
      price_ticks,
      qty,
      market_id: 1,
      maker_order_id: 1,
      taker_order_id: 2,
      maker_user_id: 1,
      taker_user_id: 2,
      maker_side: 'Buy',
    })

    const chart = buildTradeChartData(
      [trade(3, 12, 5_300), trade(1, 10, 5_100), trade(2, 11, 5_200)],
      2,
      1,
    )

    expect(chart.points.map((point) => point.time)).toEqual([11, 12])
    expect(chart.candles.map((candle) => candle.close)).toEqual([
      5_200, 5_300,
    ])
    expect(chart.firstSeq).toBe(11)
    expect(chart.lastSeq).toBe(12)
    expect(chart.minPrice).toBe(5_200)
    expect(chart.maxPrice).toBe(5_300)
    expect(chart.tradeCount).toBe(2)
  })

  it('groups fills from one command sequence into a single candle', () => {
    const trade = (trade_id, price_ticks, qty) => ({
      trade_id,
      seq: 42,
      price_ticks,
      qty,
      market_id: 1,
      maker_order_id: 1,
      taker_order_id: 2,
      maker_user_id: 1,
      taker_user_id: 2,
      maker_side: 'Sell',
    })

    const chart = buildTradeChartData(
      [trade(1, 5_100, 2), trade(2, 5_300, 1), trade(3, 5_000, 4)],
      160,
      1,
    )

    expect(chart.candles).toEqual([
      {
        time: 42,
        open: 5_100,
        high: 5_300,
        low: 5_000,
        close: 5_000,
        qty: 7,
      },
    ])
    expect(chart.points).toEqual([{ time: 42, value: 5_000 }])
  })

  it('aggregates multiple matching sequences into readable candles', () => {
    const trade = (trade_id, seq, price_ticks, qty) => ({
      trade_id,
      seq,
      price_ticks,
      qty,
      market_id: 1,
      maker_order_id: 1,
      taker_order_id: 2,
      maker_user_id: 1,
      taker_user_id: 2,
      maker_side: 'Buy',
    })

    const chart = buildTradeChartData(
      [
        trade(1, 10, 5_000, 1),
        trade(2, 11, 5_120, 2),
        trade(3, 12, 4_980, 3),
        trade(4, 13, 5_040, 4),
        trade(5, 14, 5_200, 5),
      ],
      160,
      2,
    )

    expect(chart.points.map((point) => point.time)).toEqual([
      10, 11, 12, 13, 14,
    ])
    expect(chart.candles).toEqual([
      {
        time: 11,
        open: 5_000,
        high: 5_120,
        low: 5_000,
        close: 5_120,
        qty: 3,
      },
      {
        time: 13,
        open: 4_980,
        high: 5_040,
        low: 4_980,
        close: 5_040,
        qty: 7,
      },
      {
        time: 14,
        open: 5_200,
        high: 5_200,
        low: 5_200,
        close: 5_200,
        qty: 5,
      },
    ])
    expect(chart.sequenceCount).toBe(5)
  })

  it('pads equal and uneven price domains without collapsing them', () => {
    expect(padDomain(100, 100)).toEqual({ low: 99, high: 101 })
    expect(padDomain(100, 200, 0.1)).toEqual({ low: 90, high: 210 })
    expect(padDomain(null, null)).toEqual({ low: 0, high: 1 })
  })
})
