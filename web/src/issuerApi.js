// Calls for the issuer endpoints in the gateway (docs/AUTH.md section 6).
// Every call carries the login token (api.js adds it).
import { request } from './api'

/** The most passes the server accepts in one request. */
export const MAX_ISSUE_QTY = 1000

/** One row per market: { market_id, name, passes_held }. */
export const getSupply = () => request('/api/issuer/supply')

/**
 * Credits passes to a user. `requestId` is the retry guard: sending the same number again
 * gives the same answer and creates nothing more.
 */
export const issuePasses = ({ username, marketId, qty, requestId }) =>
  request('/api/issuer/issue', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      username,
      market_id: marketId,
      qty,
      client_request_id: requestId,
    }),
  })

const MESSAGES = {
  unknown_user: 'No user has that username. Ask them to sign up first.',
  unknown_market: 'That market no longer exists. Reload the page.',
  invalid_quantity: `Enter a whole number from 1 to ${MAX_ISSUE_QTY}.`,
  invalid_request: 'Check the form and try again.',
  unauthorized:
    'The server did not accept your login. Log in again. (The mock login does not work here: VITE_MOCK_AUTH must not be true.)',
  forbidden: 'Only the issuer can use this page.',
}

/** Turns an API error into text a user can read. */
export const issuerErrorText = (error) =>
  MESSAGES[error?.reason] ?? 'Cannot reach the server. Please try again.'