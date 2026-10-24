// Calls for the auth endpoints in docs/AUTH.md.
// While the Rust login does not exist, `isMockAuth` is true in dev and the stand-in in
// mockAuth.js answers instead. To use the real server, set VITE_MOCK_AUTH=false in web/.env.local.
import { mockLogin, mockMe, mockSignup } from './mockAuth'

export const isMockAuth = import.meta.env.DEV && import.meta.env.VITE_MOCK_AUTH !== 'false'

/**
 * @param {string} path
 * @param {RequestInit} [options]
 */
const request = async (path, options = {}) => {
  const response = await fetch(path, options)
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
}

/** Turns an API error into text a user can read. */
export const authErrorText = (error) =>
  MESSAGES[error?.reason] ?? 'Cannot reach the server. Please try again.'