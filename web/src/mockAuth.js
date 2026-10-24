// A stand-in for the auth endpoints, used only while the Rust login does not exist.
// Accounts live in this browser only. Passwords are stored in plain text here
// because this is demo data in your own browser; the real server stores an Argon2 hash.

const USERS_KEY = 'dx-mock-users'
const TOKEN_LIFETIME_MS = 8 * 60 * 60 * 1000
const FIRST_NEW_USER_ID = 200

const SEED_USERS = {
  trader1: { user_id: 1, username: 'trader1', password: 'trader-pass-1', role: 'user' },
  admin: { user_id: 100, username: 'admin', password: 'admin-pass-1', role: 'admin' },
  issuer: { user_id: 101, username: 'issuer', password: 'issuer-pass-1', role: 'issuer' },
}

// localStorage can be blocked or missing (for example in tests), so keep a copy in memory too.
let memoryUsers = null

const loadUsers = () => {
  if (memoryUsers) return memoryUsers
  try {
    const saved = window.localStorage.getItem(USERS_KEY)
    if (saved) memoryUsers = JSON.parse(saved)
  } catch {
    // storage unavailable: fall through to the seed accounts
  }
  memoryUsers ??= structuredClone(SEED_USERS)
  return memoryUsers
}

const saveUsers = (users) => {
  memoryUsers = users
  try {
    window.localStorage.setItem(USERS_KEY, JSON.stringify(users))
  } catch {
    // storage unavailable: the in-memory copy still works for this tab
  }
}

const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms))

/** Throws an error shaped like the real API's errors: { status, reason }. */
const fail = (status, reason) => {
  const error = new Error(reason)
  error.status = status
  error.reason = reason
  throw error
}

const publicUser = ({ user_id, username, role }) => ({ user_id, username, role })

const makeToken = (user) =>
  `mock.${btoa(JSON.stringify({ sub: user.user_id, role: user.role, exp: Date.now() + TOKEN_LIFETIME_MS }))}`

const readToken = (token) => {
  try {
    const claims = JSON.parse(atob(String(token).replace(/^mock\./, '')))
    return claims.exp > Date.now() ? claims : null
  } catch {
    return null
  }
}

export const mockSignup = async (username, password) => {
  await delay(150)
  const name = String(username ?? '').toLowerCase()
  if (!/^[a-z0-9_]{3,32}$/.test(name)) fail(400, 'invalid_request')
  if (typeof password !== 'string' || password.length < 8 || password.length > 128) {
    fail(400, 'invalid_request')
  }
  const users = loadUsers()
  if (users[name]) fail(409, 'username_taken')
  const nextId = Math.max(FIRST_NEW_USER_ID - 1, ...Object.values(users).map((u) => u.user_id)) + 1
  // Sign-up always creates a normal user. Admin and issuer accounts come from the seed only.
  const user = { user_id: nextId, username: name, password, role: 'user' }
  saveUsers({ ...users, [name]: user })
  return { token: makeToken(user), user: publicUser(user) }
}

export const mockLogin = async (username, password) => {
  await delay(150)
  const user = loadUsers()[String(username ?? '').toLowerCase()]
  // Same error for "no such user" and "wrong password", so usernames cannot be probed.
  if (!user || user.password !== password) fail(401, 'invalid_credentials')
  return { token: makeToken(user), user: publicUser(user) }
}

export const mockMe = async (token) => {
  await delay(50)
  const claims = readToken(token)
  const user = claims && Object.values(loadUsers()).find((u) => u.user_id === claims.sub)
  if (!user) fail(401, 'unauthorized')
  return publicUser(user)
}