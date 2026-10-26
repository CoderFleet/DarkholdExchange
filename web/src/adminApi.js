// Calls for the admin endpoints in crates/admin. Every call carries the login token (api.js adds it).
import { request } from './api'

const send = (method, path, payload) =>
  request(path, {
    method,
    headers: payload ? { 'content-type': 'application/json' } : {},
    body: payload ? JSON.stringify(payload) : undefined,
  })

/** All events with all their tiers, drafts included. */
export const getAdminEvents = () => request('/api/admin/events')

export const createEvent = (name, venue) => send('POST', '/api/admin/events', { name, venue })

/** `tier` is { name, face_value_minor }. The server works out the price range. */
export const createTier = (eventId, tier) => send('POST', `/api/admin/events/${eventId}/tiers`, tier)

/** Asks the server what price range a face value would get, without saving anything. */
export const previewCollar = (faceValueMinor) =>
  send('POST', '/api/admin/collar-preview', { face_value_minor: faceValueMinor })

export const publishTier = (marketId) => send('POST', `/api/admin/tiers/${marketId}/publish`)

const MESSAGES = {
  invalid_request: 'Check the form: names need 1-80 characters and the face value must be a multiple of the tick.',
  tier_exists: 'This event already has a tier with that name.',
  already_published: 'That tier is already published.',
  not_found: 'That event or tier no longer exists. Reload the page.',
  market_setup_failed: 'The exchange could not create the market. The tier is still a draft, try again.',
  unauthorized:
    'The server did not accept your login. Log in again. (The mock login does not work here: VITE_MOCK_AUTH must not be true.)',
  forbidden: 'Only admins can use this page.',
}

/** Turns an API error into text a user can read. */
export const adminErrorText = (error) =>
  MESSAGES[error?.reason] ?? 'Cannot reach the server. Please try again.'