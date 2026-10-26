import { describe, expect, it } from 'vitest'
import { getAuthToken, notifyUnauthorized, setAuthToken } from '../authToken'
import { useAuthStore } from './useAuthStore'

describe('useAuthStore', () => {
  it('logs out when the server rejects the token', () => {
    setAuthToken('abc.def')
    useAuthStore.setState({
      token: 'abc.def',
      user: { user_id: 100, username: 'admin', role: 'admin' },
      status: 'authenticated',
    })

    notifyUnauthorized()

    const state = useAuthStore.getState()
    expect(state.user).toBeNull()
    expect(state.token).toBeNull()
    expect(state.status).toBe('anonymous')
    expect(getAuthToken()).toBeNull()
    expect(state.sessionExpired).toBe(true)
  })

  it('does not flag an expired session when the user logs out themselves', () => {
    useAuthStore.setState({ sessionExpired: true })

    useAuthStore.getState().logout()

    expect(useAuthStore.getState().sessionExpired).toBe(false)
  })
})