import { create } from 'zustand'
import { authErrorText, getMe, login, signup } from '../authApi'
import { setAuthToken } from '../authToken'

const TOKEN_KEY = 'dx-auth-token'

// Where each role lands after logging in.
export const roleHome = (role) => ({ admin: '/admin', issuer: '/issuer' })[role] ?? '/market'

// localStorage can be blocked, so every read and write is wrapped. Without it the app still works,
// you just have to log in again after a refresh.
const readToken = () => {
  try {
    return window.localStorage.getItem(TOKEN_KEY)
  } catch {
    return null
  }
}

const writeToken = (token) => {
  // Let api.js know, so every request carries (or stops carrying) the token.
  setAuthToken(token)
  try {
    if (token) window.localStorage.setItem(TOKEN_KEY, token)
    else window.localStorage.removeItem(TOKEN_KEY)
  } catch {
    // storage unavailable: ignore
  }
}

// Use a saved token for the very first requests after a refresh, before restore() has checked it.
setAuthToken(readToken())

export const useAuthStore = create((set) => {
  // Shared by login and signup: call the API, then store who we are.
  const authenticate = async (call) => {
    set({ submitting: true, error: null })
    try {
      const { token, user } = await call()
      writeToken(token)
      set({ token, user, status: 'authenticated', submitting: false })
    } catch (err) {
      set({ error: authErrorText(err), submitting: false })
    }
  }

  return {
    token: null,
    user: null,
    // 'loading' until we know whether a saved token is still valid, then 'anonymous' or 'authenticated'.
    status: 'loading',
    error: null,
    submitting: false,

    restore: async () => {
      const token = readToken()
      if (!token) {
        set({ status: 'anonymous' })
        return
      }
      try {
        const user = await getMe(token)
        setAuthToken(token)
        set({ token, user, status: 'authenticated' })
      } catch {
        writeToken(null)
        set({ token: null, user: null, status: 'anonymous' })
      }
    },

    login: (username, password) => authenticate(() => login(username, password)),
    signup: (username, password) => authenticate(() => signup(username, password)),

    logout: () => {
      writeToken(null)
      set({ token: null, user: null, status: 'anonymous', error: null })
    },

    clearError: () => set({ error: null }),
  }
})