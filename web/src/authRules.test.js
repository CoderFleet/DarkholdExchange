import { describe, expect, it } from 'vitest'
import { passwordProblem, usernameProblem } from './authRules'

describe('usernameProblem', () => {
  it('accepts good names, ignoring case and surrounding spaces', () => {
    expect(usernameProblem('stuti_1')).toBeNull()
    expect(usernameProblem('  Stuti_1  ')).toBeNull()
    expect(usernameProblem('abc')).toBeNull()
    expect(usernameProblem('a'.repeat(32))).toBeNull()
  })

  it('refuses names that are too short or too long', () => {
    expect(usernameProblem('ab')).toMatch(/3-32 characters/)
    expect(usernameProblem('')).toMatch(/3-32 characters/)
    expect(usernameProblem('a'.repeat(33))).toMatch(/3-32 characters/)
  })

  it('refuses characters other than letters, numbers and underscores', () => {
    expect(usernameProblem('has space')).toMatch(/letters, numbers and underscores/)
    expect(usernameProblem('dash-name')).toMatch(/letters, numbers and underscores/)
    expect(usernameProblem('émile')).toMatch(/letters, numbers and underscores/)
  })
})

describe('passwordProblem', () => {
  it('accepts 8 to 128 characters', () => {
    expect(passwordProblem('12345678')).toBeNull()
    expect(passwordProblem('a'.repeat(128))).toBeNull()
  })

  it('refuses passwords that are too short or too long', () => {
    expect(passwordProblem('1234567')).toMatch(/8-128 characters/)
    expect(passwordProblem('')).toMatch(/8-128 characters/)
    expect(passwordProblem('a'.repeat(129))).toMatch(/8-128 characters/)
  })

  it('counts an emoji as one character, like the server', () => {
    expect(passwordProblem('😀'.repeat(8))).toBeNull()
    expect(passwordProblem('😀'.repeat(7))).toMatch(/8-128 characters/)
  })
})