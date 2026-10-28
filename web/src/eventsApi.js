// @ts-check
// The public events list (crates/admin: GET /api/events). It needs no login and holds only
// events that have at least one published tier.
import { request } from './api.js'

/**
 * One price tier of an event. Each tier is its own market.
 *
 * @typedef {Object} EventTier
 * @property {number} market_id
 * @property {number} event_id
 * @property {string} name
 * @property {string} market_name
 * @property {number} face_value_minor
 * @property {number} tick_size_minor
 * @property {number} holding_limit
 * @property {number} taker_fee_bps
 * @property {boolean} escrow_enabled
 * @property {number} floor_minor lowest price the market accepts, in paise
 * @property {number} cap_minor highest price the market accepts, in paise
 */

/**
 * @typedef {Object} EventSummary
 * @property {number} event_id
 * @property {string} name
 * @property {string | null} venue
 * @property {EventTier[]} tiers
 */

/** @returns {Promise<EventSummary[]>} */
export const getEvents = () => request('/api/events')