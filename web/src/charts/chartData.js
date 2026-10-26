// @ts-check
/**
 * Pure data shaping for the market charts.
 *
 * Canvas drawing lives in the components; keeping ranges and cumulative
 * quantities here makes the non-visual chart logic testable in Node.
 */

/** @typedef {import('../protocol/generated/BookLevel').BookLevel} BookLevel */
/** @typedef {import('../protocol/generated/Fill').Fill} Fill */

/**
 * @typedef {{
 *   priceTicks: number,
 *   qty: number,
 *   cumulative: number,
 * }} DepthPoint
 */

/**
 * @typedef {{
 *   bids: DepthPoint[],
 *   asks: DepthPoint[],
 *   bestBid: number | null,
 *   bestAsk: number | null,
 *   minPrice: number | null,
 *   maxPrice: number | null,
 *   maxCumulative: number,
 *   totalBidQty: number,
 *   totalAskQty: number,
 * }} DepthSeries
 */

/**
 * @typedef {{
 *   time: number,
 *   open: number,
 *   high: number,
 *   low: number,
 *   close: number,
 *   qty: number,
 * }} TradeCandle
 */

/**
 * @typedef {{
 *   candles: TradeCandle[],
 *   points: Array<{ time: number, value: number }>,
 *   minPrice: number | null,
 *   maxPrice: number | null,
 *   firstSeq: number | null,
 *   lastSeq: number | null,
 *   tradeCount: number,
 *   sequenceCount: number,
 * }} TradeChartData
 */

/**
 * @param {BookLevel[] | null | undefined} [bids]
 * @param {BookLevel[] | null | undefined} [asks]
 * @returns {DepthSeries}
 */
export const buildDepthSeries = (bids, asks) => {
  const bidLevels = [...(bids ?? [])]
    .filter(
      (level) =>
        Number.isFinite(level.price_ticks) &&
        Number.isFinite(level.qty) &&
        level.qty > 0,
    )
    .sort((left, right) => right.price_ticks - left.price_ticks)
  const askLevels = [...(asks ?? [])]
    .filter(
      (level) =>
        Number.isFinite(level.price_ticks) &&
        Number.isFinite(level.qty) &&
        level.qty > 0,
    )
    .sort((left, right) => left.price_ticks - right.price_ticks)

  let bidCumulative = 0
  const bidPoints = bidLevels.map((level) => {
    bidCumulative += level.qty
    return {
      priceTicks: level.price_ticks,
      qty: level.qty,
      cumulative: bidCumulative,
    }
  })

  let askCumulative = 0
  const askPoints = askLevels.map((level) => {
    askCumulative += level.qty
    return {
      priceTicks: level.price_ticks,
      qty: level.qty,
      cumulative: askCumulative,
    }
  })

  const prices = [
    ...bidPoints.map((point) => point.priceTicks),
    ...askPoints.map((point) => point.priceTicks),
  ]

  return {
    bids: bidPoints,
    asks: askPoints,
    bestBid: bidPoints[0]?.priceTicks ?? null,
    bestAsk: askPoints[0]?.priceTicks ?? null,
    minPrice: prices.length > 0 ? Math.min(...prices) : null,
    maxPrice: prices.length > 0 ? Math.max(...prices) : null,
    maxCumulative: Math.max(bidCumulative, askCumulative, 1),
    totalBidQty: bidCumulative,
    totalAskQty: askCumulative,
  }
}

/**
 * Build line points per matching sequence and aggregate OHLC candles over a
 * fixed number of sequences. Grouping only by sequence would make each candle
 * a flat tick when a match has a single print, so candles intentionally span
 * multiple sequences while the line/area modes keep the full print detail.
 *
 * @param {Fill[] | null | undefined} [trades]
 * @param {number} [limit]
 * @param {number} [sequenceGroup]
 * @returns {TradeChartData}
 */
export const buildTradeChartData = (
  trades,
  limit = 160,
  sequenceGroup = 8,
) => {
  const ordered = [...(trades ?? [])]
    .filter(
      (trade) =>
        Number.isFinite(trade.seq) &&
        Number.isFinite(trade.trade_id) &&
        Number.isFinite(trade.price_ticks),
    )
    .sort(
      (left, right) =>
        left.seq - right.seq || left.trade_id - right.trade_id,
    )
    .slice(-Math.max(1, limit))

  /** @type {TradeCandle[]} */
  const sequenceCandles = []

  for (const trade of ordered) {
    const time = Math.max(1, Math.round(Number(trade.seq)))
    const price = Number(trade.price_ticks)
    const qty = Number.isFinite(trade.qty) ? Number(trade.qty) : 0
    const last = sequenceCandles[sequenceCandles.length - 1]

    if (last && last.time === time) {
      last.high = Math.max(last.high, price)
      last.low = Math.min(last.low, price)
      last.close = price
      last.qty += qty
      continue
    }

    sequenceCandles.push({
      time,
      open: price,
      high: price,
      low: price,
      close: price,
      qty,
    })
  }

  const groupSize = Math.max(1, Math.floor(sequenceGroup))
  /** @type {TradeCandle[]} */
  const candles = []

  for (let index = 0; index < sequenceCandles.length; index += groupSize) {
    const bucket = sequenceCandles.slice(index, index + groupSize)
    const first = bucket[0]
    const last = bucket[bucket.length - 1]
    candles.push({
      time: last.time,
      open: first.open,
      high: Math.max(...bucket.map((candle) => candle.high)),
      low: Math.min(...bucket.map((candle) => candle.low)),
      close: last.close,
      qty: bucket.reduce((sum, candle) => sum + candle.qty, 0),
    })
  }

  const points = sequenceCandles.map((candle) => ({
    time: candle.time,
    value: candle.close,
  }))
  const lows = candles.map((candle) => candle.low)
  const highs = candles.map((candle) => candle.high)

  return {
    candles,
    points,
    minPrice: lows.length > 0 ? Math.min(...lows) : null,
    maxPrice: highs.length > 0 ? Math.max(...highs) : null,
    firstSeq: sequenceCandles[0]?.time ?? null,
    lastSeq: sequenceCandles[sequenceCandles.length - 1]?.time ?? null,
    tradeCount: ordered.length,
    sequenceCount: sequenceCandles.length,
  }
}

/**
 * Add visual breathing room above and below a numeric range.
 *
 * @param {number | null} min
 * @param {number | null} max
 * @param {number} [ratio]
 * @returns {{ low: number, high: number }}
 */
export const padDomain = (min, max, ratio = 0.08) => {
  if (min == null || max == null) return { low: 0, high: 1 }
  if (min === max) {
    const padding = Math.max(1, Math.abs(min) * 0.01)
    return { low: min - padding, high: max + padding }
  }

  const padding = (max - min) * ratio
  return { low: min - padding, high: max + padding }
}
