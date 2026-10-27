import { describe, expect, it } from 'vitest'
import { auctionPhase, auctionPriceTicks, formatCountdown } from './dutchAuction'

const auction = {
  start_price_ticks: 5000,
  floor_price_ticks: 2500,
  start_ms: 1_000,
  end_ms: 11_000,
}

describe('auctionPriceTicks', () => {
  it('matches the Rust integer decay', () => {
    expect(auctionPriceTicks(auction, 0)).toBe(5000)
    expect(auctionPriceTicks(auction, 1_000)).toBe(5000)
    expect(auctionPriceTicks(auction, 6_000)).toBe(3750)
    expect(auctionPriceTicks(auction, 10_999)).toBe(2501)
    expect(auctionPriceTicks(auction, 11_000)).toBe(2500)
    expect(auctionPriceTicks(auction, 20_000)).toBe(2500)
  })

  it('treats a zero-length window as sold at the floor', () => {
    expect(
      auctionPriceTicks(
        { start_price_ticks: 5_000, floor_price_ticks: 2_500, start_ms: 10, end_ms: 10 },
        10,
      ),
    ).toBe(2_500)
  })
})

describe('auctionPhase', () => {
  const base = { status: 'active', qty_remaining: 3, start_ms: 1_000, end_ms: 11_000 }

  it('walks scheduled -> active -> ended', () => {
    expect(auctionPhase(base, 999)).toBe('scheduled')
    expect(auctionPhase(base, 1_000)).toBe('active')
    expect(auctionPhase(base, 10_999)).toBe('active')
    expect(auctionPhase(base, 11_000)).toBe('ended')
  })

  it('reports terminal states and sold-out auctions', () => {
    expect(auctionPhase({ ...base, status: 'sold' }, 5_000)).toBe('sold')
    expect(auctionPhase({ ...base, status: 'expired' }, 20_000)).toBe('expired')
    expect(auctionPhase({ ...base, qty_remaining: 0 }, 5_000)).toBe('ended')
  })
})

describe('formatCountdown', () => {
  it('rounds up to whole seconds and caps at zero', () => {
    expect(formatCountdown(0)).toBe('0:00')
    expect(formatCountdown(-500)).toBe('0:00')
    expect(formatCountdown(1)).toBe('0:01')
    expect(formatCountdown(61_000)).toBe('1:01')
    expect(formatCountdown(3_600_000)).toBe('1h 00m')
  })
})
