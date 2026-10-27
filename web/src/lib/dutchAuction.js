// Dutch-auction countdown maths. The server is the source of truth: it recomputes
// the price from the sequenced time. These helpers only mirror that curve so the
// page can tick between polls.

/**
 * Price in ticks at `nowMs`, matching `DutchAuction::price_at` in Rust:
 * linear decay, rounded down, clamped to [floor, start].
 *
 * @param {{ start_price_ticks: number, floor_price_ticks: number, start_ms: number, end_ms: number }} auction
 * @param {number} nowMs
 * @returns {number}
 */
export const auctionPriceTicks = (auction, nowMs) => {
  if (nowMs >= auction.end_ms) return auction.floor_price_ticks
  if (nowMs <= auction.start_ms) return auction.start_price_ticks
  const duration = auction.end_ms - auction.start_ms
  if (duration <= 0) return auction.floor_price_ticks
  const span = auction.start_price_ticks - auction.floor_price_ticks
  const elapsed = Math.min(Math.max(nowMs - auction.start_ms, 0), duration)
  const drop = Math.floor((span * elapsed) / duration)
  return Math.max(auction.floor_price_ticks, auction.start_price_ticks - drop)
}

/**
 * @param {{ status: string, qty_remaining: number, end_ms: number }} auction
 * @param {number} nowMs
 * @returns {'scheduled' | 'active' | 'ended' | 'sold' | 'expired'}
 */
export const auctionPhase = (auction, nowMs) => {
  if (auction.status === 'sold') return 'sold'
  if (auction.status === 'expired') return 'expired'
  if (nowMs < auction.start_ms) return 'scheduled'
  if (nowMs >= auction.end_ms || auction.qty_remaining === 0) return 'ended'
  return 'active'
}

/**
 * @param {number} ms
 * @returns {string}
 */
export const formatCountdown = (ms) => {
  const total = Math.max(0, Math.ceil(ms / 1000))
  const minutes = Math.floor(total / 60)
  const seconds = total % 60
  if (minutes >= 60) {
    const hours = Math.floor(minutes / 60)
    return `${hours}h ${String(minutes % 60).padStart(2, '0')}m`
  }
  return `${minutes}:${String(seconds).padStart(2, '0')}`
}
