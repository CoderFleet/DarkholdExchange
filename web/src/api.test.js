import { afterEach, describe, expect, it, vi } from 'vitest'
import { cancelOrderRequest, postCommand } from './api.js'

const jsonResponse = (body, status = 200) => ({
  ok: status >= 200 && status < 300,
  status,
  json: async () => body,
})

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('order commands', () => {
  it('cancels through the canonical DELETE endpoint', async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse({
        status: 'Cancelled',
        order_id: 42,
        client_order_id: 99,
        remaining_qty: 2,
      }),
    )
    vi.stubGlobal('fetch', fetchMock)

    const response = await cancelOrderRequest(42, 99)

    expect(fetchMock).toHaveBeenCalledWith(
      '/api/orders/42?client_order_id=99',
      {
        method: 'DELETE',
      },
    )
    expect(response.status).toBe('Cancelled')
  })

  it('falls back to the legacy route when /api/orders returns 405', async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse({ reason: 'cancel_not_allowed' }, 405))
      .mockResolvedValueOnce(jsonResponse({ status: 'Cancelled' }))
    vi.stubGlobal('fetch', fetchMock)

    const command = { type: 'Cancel', order_id: 42, client_order_id: 99 }
    const response = await postCommand(command)

    expect(fetchMock).toHaveBeenCalledTimes(2)
    expect(fetchMock.mock.calls[1][0]).toBe('/api/commands')
    expect(response.status).toBe('Cancelled')
  })

  it('does not fall back on unrelated command rejections', async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse({ reason: 'insufficient_cash' }, 422))
    vi.stubGlobal('fetch', fetchMock)

    await expect(postCommand({ type: 'Place' })).rejects.toThrow(
      'insufficient cash',
    )
    expect(fetchMock).toHaveBeenCalledTimes(1)
  })
})
