// Calls for the auth endpoints in docs/AUTH.md.
// The real login (crates/auth) is the default. The browser-only stand-in in mockAuth.js is used
// only in dev and only when you ask for it with VITE_MOCK_AUTH=true (for example in web/.env.local).
import { mockLogin, mockMe, mockSignup } from './mockAuth'
import { fetchWithPuzzle } from './pow/fetchWithPuzzle.js'

export const isMockAuth = import.meta.env.DEV && import.meta.env.VITE_MOCK_AUTH === 'true'

/**
 * @param {string} path
 * @param {RequestInit} [options]
 */
const request = async (path, options = {}) => {
  const response = await fetchWithPuzzle(path, options)
  const body = await response.json().catch(() => null)
  if (!response.ok) {
    const error = new Error(body?.reason ?? `HTTP ${response.status}`)
    error.status = response.status
    error.reason = body?.reason
    throw error
  }
  return body
}

const postJson = (path, payload) =>
  request(path, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(payload),
  })

/** @returns {Promise<{ token: string, user: { user_id: number, username: string, role: string } }>} */
export const signup = (username, password) =>
  isMockAuth ? mockSignup(username, password) : postJson('/api/auth/signup', { username, password })

/** @returns {Promise<{ token: string, user: { user_id: number, username: string, role: string } }>} */
export const login = (username, password) =>
  isMockAuth ? mockLogin(username, password) : postJson('/api/auth/login', { username, password })

/**
 * @param {string} token
 * @returns {Promise<{ user_id: number, username: string, role: string }>}
 */
export const getMe = (token) =>
  isMockAuth ? mockMe(token) : request('/api/me', { headers: { authorization: `Bearer ${token}` } })

const MESSAGES = {
  invalid_request: 'Check the form: username 3-32 letters, numbers or _, password 8-128 characters.',
  invalid_credentials: 'Wrong username or password.',
  username_taken: 'That username is already taken.',
  unauthorized: 'Your session has expired. Please log in again.',
  forbidden: 'You do not have access to that.',
  rate_limited: 'Too many attempts. Please wait a moment and try again.',
    pow_required: 'Too many attempts from your network. Please wait a minute and try again.',
}

/** Turns an API error into text a user can read. */
export const authErrorText = (error) =>
  MESSAGES[error?.reason] ?? 'Cannot reach the server. Please try again.'