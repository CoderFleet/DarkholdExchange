import { createHash } from 'node:crypto'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { fetchWithPuzzle } from './fetchWithPuzzle.js'
import { sha256 } from './sha256.js'
import { leadingZeroBits, solve } from './solve.js'

const hex = (bytes) => Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('')

describe('sha256', () => {
  it('matches Node on short, boundary and long inputs', () => {
    const inputs = [
      '',
      'abc',
      'a'.repeat(55),
      'a'.repeat(56),
      'a'.repeat(63),
      'a'.repeat(64),
      'a'.repeat(65),
      'a'.repeat(1000),
      'héllo ₹ 世界',
      'v1.1700000000.16.00ff00ff00ff00ff.' + 'ab'.repeat(32) + '~123456',
    ]
    for (const text of inputs) {
      const bytes = new TextEncoder().encode(text)
      expect(hex(sha256(bytes))).toBe(createHash('sha256').update(bytes).digest('hex'))
    }
  })
})

describe('the puzzle', () => {
  it('counts leading zero bits', () => {
    expect(leadingZeroBits(new Uint8Array([0xff]))).toBe(0)
    expect(leadingZeroBits(new Uint8Array([0x7f]))).toBe(1)
    expect(leadingZeroBits(new Uint8Array([0x01]))).toBe(7)
    expect(leadingZeroBits(new Uint8Array([0x00, 0x10]))).toBe(11)
    expect(leadingZeroBits(new Uint8Array([0, 0]))).toBe(16)
  })

  it('finds a nonce whose hash has enough zero bits', () => {
    const nonce = solve('some-challenge', 12)
    const hash = createHash('sha256').update(`some-challenge~${nonce}`).digest()
    expect(leadingZeroBits(hash)).toBeGreaterThanOrEqual(12)
  })

  it('finds the same answer as the Rust solver (golden value, also tested in crates/pow)', () => {
    expect(solve('golden', 12)).toBe(2561)
  })
})

const reply = (body, status = 200) => ({
  ok: status >= 200 && status < 300,
  status,
  json: async () => body,
})

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('fetchWithPuzzle', () => {
  it('passes ordinary answers straight through, with the original arguments', async () => {
    const fetchMock = vi.fn().mockResolvedValue(reply({ ok: 1 }))
    vi.stubGlobal('fetch', fetchMock)
    const options = { method: 'POST', headers: { a: 'b' } }
    const response = await fetchWithPuzzle('/api/orders', options)
    expect(response.status).toBe(200)
    expect(fetchMock).toHaveBeenCalledTimes(1)
    expect(fetchMock).toHaveBeenCalledWith('/api/orders', options)
  })

  it('solves the puzzle the server asks for and sends the request again once', async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(reply({ reason: 'pow_required', bits: 8 }, 428))
      .mockResolvedValueOnce(reply({ challenge: 'v1.test.challenge', bits: 8, ttl_secs: 120 }))
      .mockResolvedValueOnce(reply({ done: true }))
    vi.stubGlobal('fetch', fetchMock)

    const response = await fetchWithPuzzle('/api/orders', {
      method: 'POST',
      headers: { authorization: 'Bearer t' },
      body: '{}',
    })

    expect(response.status).toBe(200)
    expect(fetchMock).toHaveBeenCalledTimes(3)
    expect(fetchMock.mock.calls[1][0]).toBe('/api/pow/challenge')
    const [path, retry] = fetchMock.mock.calls[2]
    expect(path).toBe('/api/orders')
    expect(retry.method).toBe('POST')
    expect(retry.body).toBe('{}')
    expect(retry.headers.authorization).toBe('Bearer t')
    const [challenge, nonce] = retry.headers['x-pow'].split('~')
    expect(challenge).toBe('v1.test.challenge')
    const hash = createHash('sha256').update(`${challenge}~${nonce}`).digest()
    expect(leadingZeroBits(hash)).toBeGreaterThanOrEqual(8)
  })

  it('does not loop: a second 428 goes back to the caller', async () => {
    const again = reply({ reason: 'pow_required', bits: 10 }, 428)
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(reply({ reason: 'pow_required', bits: 8 }, 428))
      .mockResolvedValueOnce(reply({ challenge: 'c', bits: 4, ttl_secs: 120 }))
      .mockResolvedValueOnce(again)
    vi.stubGlobal('fetch', fetchMock)
    const response = await fetchWithPuzzle('/api/orders', { method: 'POST' })
    expect(response).toBe(again)
    expect(fetchMock).toHaveBeenCalledTimes(3)
  })

  it('leaves other 428 answers and a failing challenge request alone', async () => {
    const other = reply({ reason: 'something_else' }, 428)
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(other))
    expect(await fetchWithPuzzle('/x', {})).toBe(other)

    const asked = reply({ reason: 'pow_required', bits: 8 }, 428)
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(asked)
      .mockResolvedValueOnce(reply({ reason: 'pow_disabled' }, 404))
    vi.stubGlobal('fetch', fetchMock)
    expect(await fetchWithPuzzle('/x', {})).toBe(asked)
    expect(fetchMock).toHaveBeenCalledTimes(2)
  })
})