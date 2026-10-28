import { describe, expect, it } from 'vitest'
import {
  buildEventList,
  commonWordPrefix,
  eventMatches,
  eventOfMarket,
  spanOf,
} from './eventList.js'

const market = (id, event_id, name, extra = {}) => ({
  id,
  event_id,
  name,
  face_value_minor: 100000,
  tick_size_minor: 100,
  taker_fee_bps: 100,
  holding_limit: 4,
  escrow_enabled: false,
  ...extra,
})
const adminTier = (market_id, event_id, name, market_name) => ({
  market_id,
  event_id,
  name,
  market_name,
  face_value_minor: 100000,
  tick_size_minor: 100,
  holding_limit: 4,
  taker_fee_bps: 100,
  escrow_enabled: false,
})
const adminEvent = (event_id, name, venue, tiers) => ({ event_id, name, venue, tiers })

describe('event list', () => {
  it('finds the words every name starts with, leaving something of each', () => {
    expect(commonWordPrefix(['Day 1 General', 'Day 1 VIP'])).toBe('Day 1')
    expect(commonWordPrefix(['Day 1 General', 'Night 1 VIP'])).toBe('')
    expect(commonWordPrefix(['Gold', 'Gold'])).toBe('')
    expect(commonWordPrefix(['A B', 'A B C'])).toBe('A')
    expect(commonWordPrefix([])).toBe('')
  })

  it('uses the admin event names and tier names for published events', () => {
    const events = [
      adminEvent(100, 'Coldplay Mumbai', 'DY Patil Stadium', [
        adminTier(100, 100, 'Gold', 'Coldplay Mumbai Gold'),
        adminTier(101, 100, 'General', 'Coldplay Mumbai General'),
      ]),
    ]
    const markets = [market(100, 100, 'Coldplay Mumbai Gold'), market(101, 100, 'Coldplay Mumbai General')]
    const [entry] = buildEventList(events, markets)
    expect(entry.name).toBe('Coldplay Mumbai')
    expect(entry.venue).toBe('DY Patil Stadium')
    expect(entry.tiers.map((tier) => tier.name)).toEqual(['Gold', 'General'])
    expect(entry.tiers[0].config).toBe(markets[0])
  })

  it('still lists an admin tier whose market the engine has not loaded yet', () => {
    const events = [adminEvent(100, 'Coldplay Mumbai', null, [adminTier(100, 100, 'Gold', 'Coldplay Mumbai Gold')])]
    const [entry] = buildEventList(events, [])
    expect(entry.tiers[0].config.name).toBe('Coldplay Mumbai Gold')
    expect(entry.tiers[0].config.face_value_minor).toBe(100000)
  })

  it('groups the demo markets by event number and names them from their market names', () => {
    const markets = [
      market(1, 1, 'Day 1 General'),
      market(2, 1, 'Day 1 VIP'),
      market(3, 2, 'Relic: Obsidian Sigil', { escrow_enabled: true }),
    ]
    const list = buildEventList([], markets)
    expect(list.map((event) => event.name)).toEqual(['Day 1', 'Relic: Obsidian Sigil'])
    expect(list[0].tiers.map((tier) => tier.name)).toEqual(['General', 'VIP'])
    expect(list[1].tiers).toHaveLength(1)
  })

  it('names a group with nothing in common after its event number', () => {
    const list = buildEventList([], [market(5, 7, 'Alpha'), market(6, 7, 'Beta')])
    expect(list[0].name).toBe('Event 7')
    expect(list[0].tiers.map((tier) => tier.name)).toEqual(['Alpha', 'Beta'])
  })

  it('lists events oldest first, demo and admin together', () => {
    const events = [adminEvent(100, 'Coldplay', null, [adminTier(100, 100, 'Gold', 'Coldplay Gold')])]
    const markets = [market(100, 100, 'Coldplay Gold'), market(1, 1, 'Day 1 General')]
    expect(buildEventList(events, markets).map((event) => event.eventId)).toEqual([1, 100])
  })

  it('does not list a market twice', () => {
    const events = [adminEvent(100, 'Coldplay', null, [adminTier(100, 100, 'Gold', 'Coldplay Gold')])]
    const list = buildEventList(events, [market(100, 100, 'Coldplay Gold')])
    expect(list).toHaveLength(1)
    expect(list[0].tiers).toHaveLength(1)
  })

  it('matches events by name, venue, tier name and numbers', () => {
    const events = [
      adminEvent(100, 'Coldplay Mumbai', 'DY Patil Stadium', [
        adminTier(100, 100, 'Gold', 'Coldplay Mumbai Gold'),
      ]),
    ]
    const [entry] = buildEventList(events, [market(100, 100, 'Coldplay Mumbai Gold')])
    expect(eventMatches(entry, '', 'all')).toBe(true)
    expect(eventMatches(entry, 'cold', 'all')).toBe(true)
    expect(eventMatches(entry, '  PATIL ', 'all')).toBe(true)
    expect(eventMatches(entry, 'gold', 'all')).toBe(true)
    expect(eventMatches(entry, 'event 100', 'all')).toBe(true)
    expect(eventMatches(entry, 'm100', 'all')).toBe(true)
    expect(eventMatches(entry, 'metallica', 'all')).toBe(false)
  })

  it('keeps an event for a kind when at least one tier is of that kind', () => {
    const spotOnly = buildEventList([], [market(1, 1, 'A')])[0]
    const mixed = buildEventList([], [market(2, 2, 'B x'), market(3, 2, 'B y', { escrow_enabled: true })])[0]
    expect(eventMatches(spotOnly, '', 'spot')).toBe(true)
    expect(eventMatches(spotOnly, '', 'escrow')).toBe(false)
    expect(eventMatches(mixed, '', 'spot')).toBe(true)
    expect(eventMatches(mixed, '', 'escrow')).toBe(true)
  })

  it('gives the lowest and highest of some numbers', () => {
    expect(spanOf([5, 2, 9])).toEqual({ low: 2, high: 9 })
    expect(spanOf([])).toBeNull()
  })

  it('finds the event a market belongs to', () => {
    const list = buildEventList([], [market(1, 1, 'Day 1 General'), market(2, 1, 'Day 1 VIP'), market(3, 2, 'Relic')])
    expect(eventOfMarket(list, 2)?.eventId).toBe(1)
    expect(eventOfMarket(list, 3)?.eventId).toBe(2)
    expect(eventOfMarket(list, 99)).toBeNull()
    expect(eventOfMarket(list, null)).toBeNull()
  })
})