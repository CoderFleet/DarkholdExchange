import { afterEach, describe, expect, it, vi } from 'vitest'
import { getMarkets, getOpenOrders } from './api.js'
import { setAuthToken } from './authToken.js'

const jsonResponse = (body, status = 200, headers = {}) => ({
  ok: status >= 200 && status < 300,
  status,
  headers: { get: (name) => headers[name.toLowerCase()] ?? null },
  json: async () => body,
})

afterEach(() => {
  setAuthToken(null)
  vi.unstubAllGlobals()
})

describe('request helper', () => {
  it('sends the request unchanged when nobody is logged in', async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse([]))
    vi.stubGlobal('fetch', fetchMock)

    await getMarkets()

    expect(fetchMock).toHaveBeenCalledWith('/api/markets', {})
  })

  it('adds the bearer token once there is one', async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse([]))
    vi.stubGlobal('fetch', fetchMock)
    setAuthToken('abc.def')

    await getMarkets()

    expect(fetchMock).toHaveBeenCalledWith('/api/markets', {
      headers: { authorization: 'Bearer abc.def' },
    })
  })

  it('keeps the existing headers when it adds the token', async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse([]))
    vi.stubGlobal('fetch', fetchMock)
    setAuthToken('abc.def')

    await getOpenOrders(3)

    expect(fetchMock).toHaveBeenCalledWith('/api/orders/open', {
      headers: { 'x-dev-user': '3', authorization: 'Bearer abc.def' },
    })
  })

  it('reports how long to wait when the server says try again later', async () => {
    const reply = jsonResponse({ reason: 'queue_full' }, 429, { 'retry-after': '2' })
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(reply))

    await expect(getMarkets()).rejects.toMatchObject({
      status: 429,
      reason: 'queue_full',
      retryAfter: 2,
    })
  })

  it('leaves retryAfter unset when the reply has no Retry-After header', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(jsonResponse({ reason: 'unknown_market' }, 404)))

    const error = await getMarkets().catch((caught) => caught)

    expect(error.status).toBe(404)
    expect(error).not.toHaveProperty('retryAfter')
  })
})