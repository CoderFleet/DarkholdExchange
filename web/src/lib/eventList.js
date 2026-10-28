// @ts-check
// Builds the one list of "events" that the Markets page and the trading page's tier tabs share.
//
// An event is what people trade passes for, and its tiers are the price levels (Gold, General...).
// Behind the scenes every tier is one market in the engine. Events made in the admin console come
// from GET /api/events and have real names. The demo markets the engine starts with have no event
// name, so they are grouped by their event number and named from their market names.

/** @typedef {import('../eventsApi.js').EventSummary} EventSummary */
/** @typedef {import('../eventsApi.js').EventTier} EventTier */

/**
 * The parts of a market's configuration that these helpers read (GET /api/markets, `config`).
 *
 * @typedef {Object} MarketLike
 * @property {number} id
 * @property {number} event_id
 * @property {string} name
 * @property {number} face_value_minor
 * @property {number} tick_size_minor
 * @property {number} taker_fee_bps
 * @property {number} holding_limit
 * @property {boolean} escrow_enabled
 */

/**
 * @typedef {Object} Tier
 * @property {number} marketId
 * @property {string} name
 * @property {MarketLike} config
 */

/**
 * @typedef {Object} EventEntry
 * @property {number} eventId
 * @property {string} name
 * @property {string | null} venue
 * @property {Tier[]} tiers
 */

/**
 * The words that every name starts with, always leaving at least one word of each name over.
 * ["Day 1 General", "Day 1 VIP"] gives "Day 1".
 *
 * @param {string[]} names
 */
export const commonWordPrefix = (names) => {
  const split = names.map((name) => name.trim().split(/\s+/))
  const first = split[0]
  if (!first) return ''
  const room = Math.min(...split.map((words) => words.length)) - 1
  let count = 0
  while (count < room && split.every((words) => words[count] === first[count])) count += 1
  return first.slice(0, count).join(' ')
}

/**
 * What an admin tier looks like when the engine has not listed its market yet.
 *
 * @param {EventTier} tier
 * @returns {MarketLike}
 */
const configFromTier = (tier) => ({
  id: tier.market_id,
  event_id: tier.event_id,
  name: tier.market_name,
  face_value_minor: tier.face_value_minor,
  tick_size_minor: tier.tick_size_minor,
  taker_fee_bps: tier.taker_fee_bps,
  holding_limit: tier.holding_limit,
  escrow_enabled: tier.escrow_enabled,
})

/**
 * Joins the published events with the engine's markets into one list, oldest event first.
 *
 * @param {EventSummary[]} events from GET /api/events
 * @param {MarketLike[]} markets the engine's markets
 * @returns {EventEntry[]}
 */
export const buildEventList = (events, markets) => {
  const byId = new Map(markets.map((market) => [market.id, market]))
  const inEvents = new Set()

  /** @type {EventEntry[]} */
  const list = events.map((event) => ({
    eventId: event.event_id,
    name: event.name,
    venue: event.venue,
    tiers: event.tiers.map((tier) => {
      inEvents.add(tier.market_id)
      return {
        marketId: tier.market_id,
        name: tier.name,
        config: byId.get(tier.market_id) ?? configFromTier(tier),
      }
    }),
  }))

  // Markets that belong to no listed event: group them by event number.
  /** @type {Map<number, MarketLike[]>} */
  const loose = new Map()
  for (const market of [...markets].sort((a, b) => a.id - b.id)) {
    if (inEvents.has(market.id)) continue
    loose.set(market.event_id, [...(loose.get(market.event_id) ?? []), market])
  }
  for (const [eventId, group] of loose) {
    const prefix = group.length > 1 ? commonWordPrefix(group.map((market) => market.name)) : ''
    list.push({
      eventId,
      name: prefix || (group.length === 1 ? group[0].name : `Event ${eventId}`),
      venue: null,
      tiers: group.map((market) => ({
        marketId: market.id,
        name: prefix ? market.name.slice(prefix.length).trim() || market.name : market.name,
        config: market,
      })),
    })
  }

  return list.sort((a, b) => a.eventId - b.eventId)
}

/**
 * Whether an event passes the search box and the kind filter. The search looks at the event's
 * name, venue and number and at each tier's name and market number; the kind filter keeps an event
 * when at least one of its tiers is of that kind.
 *
 * @param {EventEntry} event
 * @param {string} query
 * @param {'all' | 'spot' | 'escrow'} kind
 */
export const eventMatches = (event, query, kind) => {
  if (kind === 'escrow' && !event.tiers.some((tier) => tier.config.escrow_enabled)) return false
  if (kind === 'spot' && !event.tiers.some((tier) => !tier.config.escrow_enabled)) return false
  const needle = query.trim().toLowerCase()
  if (!needle) return true
  const haystack = [
    event.name,
    event.venue ?? '',
    `event ${event.eventId}`,
    ...event.tiers.flatMap((tier) => [tier.name, tier.config.name, `market ${tier.marketId}`, `m${tier.marketId}`]),
  ]
    .join(' ')
    .toLowerCase()
  return haystack.includes(needle)
}

/**
 * Lowest and highest of some numbers. Null when there are none.
 *
 * @param {number[]} values
 */
export const spanOf = (values) =>
  values.length === 0 ? null : { low: Math.min(...values), high: Math.max(...values) }

/**
 * The event a market belongs to, or null.
 *
 * @param {EventEntry[]} list
 * @param {number | null} marketId
 */
export const eventOfMarket = (list, marketId) =>
  list.find((event) => event.tiers.some((tier) => tier.marketId === marketId)) ?? null