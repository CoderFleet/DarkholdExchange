import { describe, expect, it } from 'vitest'
import { mockLogin, mockMe, mockSignup } from './mockAuth.js'

// The stand-in must behave like the real endpoints in docs/AUTH.md, so these tests
// pin down the contract: the same inputs, outputs and error codes.

const failure = async (promise) => {
  try {
    await promise
  } catch (error) {
    return { status: error.status, reason: error.reason }
  }
  throw new Error('expected the call to fail, but it succeeded')
}

describe('mockLogin', () => {
  it('logs in each seeded role', async () => {
    const admin = await mockLogin('admin', 'admin-pass-1')
    expect(admin.user).toEqual({ user_id: 100, username: 'admin', role: 'admin' })
    const issuer = await mockLogin('issuer', 'issuer-pass-1')
    expect(issuer.user.role).toBe('issuer')
    const trader = await mockLogin('trader1', 'trader-pass-1')
    expect(trader.user.role).toBe('user')
  })

  it('never returns the password', async () => {
    const { user } = await mockLogin('admin', 'admin-pass-1')
    expect(user).not.toHaveProperty('password')
  })

  it('gives the same error for a wrong password and an unknown user', async () => {
    const wrongPassword = await failure(mockLogin('admin', 'nope-nope-1'))
    const unknownUser = await failure(mockLogin('nobody_here', 'whatever-1'))
    expect(wrongPassword).toEqual({ status: 401, reason: 'invalid_credentials' })
    expect(unknownUser).toEqual(wrongPassword)
  })

  it('ignores the case of the username', async () => {
    const { user } = await mockLogin('ADMIN', 'admin-pass-1')
    expect(user.username).toBe('admin')
  })
})

describe('mockSignup', () => {
  it('creates a normal user and logs them in', async () => {
    const { token, user } = await mockSignup('Test_User_1', 'password-123')
    expect(user.username).toBe('test_user_1')
    expect(user.role).toBe('user')
    expect(user.user_id).toBeGreaterThanOrEqual(200)
    expect(await mockMe(token)).toEqual(user)
    expect((await mockLogin('test_user_1', 'password-123')).user).toEqual(user)
  })

  it('gives each new user a different id', async () => {
    const a = await mockSignup('id_check_a', 'password-123')
    const b = await mockSignup('id_check_b', 'password-123')
    expect(a.user.user_id).not.toBe(b.user.user_id)
  })

  it('refuses a username that is already taken', async () => {
    await mockSignup('taken_name', 'password-123')
    expect(await failure(mockSignup('TAKEN_NAME', 'password-456'))).toEqual({
      status: 409,
      reason: 'username_taken',
    })
  })

  it('refuses admin as a username, because it is already taken', async () => {
    expect(await failure(mockSignup('admin', 'password-123'))).toEqual({
      status: 409,
      reason: 'username_taken',
    })
  })

  it('refuses bad usernames', async () => {
    for (const name of ['ab', 'has space', 'bad-dash', 'x'.repeat(33), '']) {
      expect(await failure(mockSignup(name, 'password-123'))).toEqual({
        status: 400,
        reason: 'invalid_request',
      })
    }
  })

  it('refuses bad passwords', async () => {
    for (const password of ['short', 'x'.repeat(129), '']) {
      expect(await failure(mockSignup('pw_check', password))).toEqual({
        status: 400,
        reason: 'invalid_request',
      })
    }
  })
})

describe('mockMe', () => {
  it('returns the user for a valid token', async () => {
    const { token, user } = await mockLogin('issuer', 'issuer-pass-1')
    expect(await mockMe(token)).toEqual(user)
  })

  it('rejects garbage and missing tokens', async () => {
    for (const token of ['junk', '', undefined, 'mock.not-base64!!']) {
      expect(await failure(mockMe(token))).toEqual({ status: 401, reason: 'unauthorized' })
    }
  })

  it('rejects an expired token', async () => {
    const expired = `mock.${btoa(JSON.stringify({ sub: 100, role: 'admin', exp: Date.now() - 1 }))}`
    expect(await failure(mockMe(expired))).toEqual({ status: 401, reason: 'unauthorized' })
  })

  it('rejects a token for a user that does not exist', async () => {
    const ghost = `mock.${btoa(JSON.stringify({ sub: 99999, role: 'user', exp: Date.now() + 60000 }))}`
    expect(await failure(mockMe(ghost))).toEqual({ status: 401, reason: 'unauthorized' })
  })
})
