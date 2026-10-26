import { beforeEach, describe, expect, it, vi } from 'vitest'
import * as adminApi from '../adminApi'
import { useAdminStore } from './useAdminStore'

// Replace only the network calls. The error-text helper stays real.
vi.mock('../adminApi', async (importOriginal) => ({
  ...(await importOriginal()),
  getAdminEvents: vi.fn(),
  createEvent: vi.fn(),
  createTier: vi.fn(),
  publishTier: vi.fn(),
}))

const event = { event_id: 100, name: 'Webster Night', venue: null, tiers: [] }
const reasonError = (reason) => Object.assign(new Error(reason), { reason })

beforeEach(() => {
  vi.resetAllMocks()
  useAdminStore.setState({ events: [], status: 'loading', error: null, busy: false })
})

describe('useAdminStore', () => {
  it('loads the events', async () => {
    adminApi.getAdminEvents.mockResolvedValue([event])
    await useAdminStore.getState().load()
    const state = useAdminStore.getState()
    expect(state.events).toEqual([event])
    expect(state.status).toBe('ready')
    expect(state.error).toBeNull()
  })

  it('shows a readable message when loading fails', async () => {
    adminApi.getAdminEvents.mockRejectedValue(reasonError('forbidden'))
    await useAdminStore.getState().load()
    expect(useAdminStore.getState().error).toBe('Only admins can use this page.')
    expect(useAdminStore.getState().status).toBe('ready')
  })

  it('creates an event, then reloads the list', async () => {
    adminApi.createEvent.mockResolvedValue(event)
    adminApi.getAdminEvents.mockResolvedValue([event])
    const ok = await useAdminStore.getState().addEvent('Webster Night', '')
    expect(ok).toBe(true)
    expect(adminApi.createEvent).toHaveBeenCalledWith('Webster Night', undefined)
    expect(useAdminStore.getState().events).toEqual([event])
    expect(useAdminStore.getState().busy).toBe(false)
  })

  it('adds a tier with the face value in paise', async () => {
    adminApi.createTier.mockResolvedValue({})
    adminApi.getAdminEvents.mockResolvedValue([event])
    await useAdminStore.getState().addTier(100, 'General', 500_000)
    expect(adminApi.createTier).toHaveBeenCalledWith(100, { name: 'General', face_value_minor: 500_000 })
  })

  it('keeps the old list and reports the error when an action fails', async () => {
    useAdminStore.setState({ events: [event] })
    adminApi.createTier.mockRejectedValue(reasonError('tier_exists'))
    const ok = await useAdminStore.getState().addTier(100, 'General', 500_000)
    expect(ok).toBe(false)
    expect(useAdminStore.getState().events).toEqual([event])
    expect(useAdminStore.getState().error).toBe('This event already has a tier with that name.')
    expect(adminApi.getAdminEvents).not.toHaveBeenCalled()
  })

  it('publishes a tier', async () => {
    adminApi.publishTier.mockResolvedValue({})
    adminApi.getAdminEvents.mockResolvedValue([event])
    expect(await useAdminStore.getState().publish(100)).toBe(true)
    expect(adminApi.publishTier).toHaveBeenCalledWith(100)
  })
})

describe('adminErrorText', () => {
  it('falls back to a generic message', () => {
    expect(adminApi.adminErrorText(new Error('network down'))).toBe('Cannot reach the server. Please try again.')
  })
})