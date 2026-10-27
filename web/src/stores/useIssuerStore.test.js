import { beforeEach, describe, expect, it, vi } from 'vitest'
import * as issuerApi from '../issuerApi'
import { useIssuerStore } from './useIssuerStore'

// Replace only the network calls. The error-text helper stays real.
vi.mock('../issuerApi', async (importOriginal) => ({
  ...(await importOriginal()),
  getSupply: vi.fn(),
  issuePasses: vi.fn(),
}))

const rows = [{ market_id: 1, name: 'Day 1 General', passes_held: 12 }]
const refusal = (reason, status) => Object.assign(new Error(reason), { reason, status })
const lost = () => new Error('Failed to fetch')

beforeEach(() => {
  vi.resetAllMocks()
  useIssuerStore.setState({
    supply: rows,
    status: 'loading',
    error: null,
    notice: null,
    busy: false,
    pending: null,
  })
})

describe('useIssuerStore', () => {
  it('loads the supply', async () => {
    issuerApi.getSupply.mockResolvedValue(rows)
    await useIssuerStore.getState().load()
    expect(useIssuerStore.getState().supply).toEqual(rows)
    expect(useIssuerStore.getState().status).toBe('ready')
  })

  it('shows a readable message when loading fails', async () => {
    issuerApi.getSupply.mockRejectedValue(refusal('forbidden', 403))
    await useIssuerStore.getState().load()
    expect(useIssuerStore.getState().error).toBe('Only the issuer can use this page.')
    expect(useIssuerStore.getState().status).toBe('ready')
  })

  it('issues passes, says so, and reloads the supply', async () => {
    issuerApi.issuePasses.mockResolvedValue({ journal_id: 5 })
    issuerApi.getSupply.mockResolvedValue([{ ...rows[0], passes_held: 15 }])
    const ok = await useIssuerStore.getState().issue(' dana ', 1, 3)
    expect(ok).toBe(true)
    expect(issuerApi.issuePasses).toHaveBeenCalledWith(
      expect.objectContaining({ username: 'dana', marketId: 1, qty: 3 }),
    )
    const state = useIssuerStore.getState()
    expect(state.notice).toBe('Credited 3 passes of Day 1 General to dana.')
    expect(state.supply[0].passes_held).toBe(15)
    expect(state.pending).toBeNull()
    expect(state.busy).toBe(false)
  })

  it('reports a refusal and sends a fresh number next time', async () => {
    issuerApi.issuePasses.mockRejectedValue(refusal('unknown_user', 404))
    expect(await useIssuerStore.getState().issue('ghost', 1, 1)).toBe(false)
    expect(useIssuerStore.getState().error).toBe('No user has that username. Ask them to sign up first.')
    expect(useIssuerStore.getState().pending).toBeNull()
    expect(issuerApi.getSupply).not.toHaveBeenCalled()
  })

  it('reuses the request number after a dropped connection, so a retry cannot double-credit', async () => {
    issuerApi.issuePasses.mockRejectedValueOnce(lost())
    expect(await useIssuerStore.getState().issue('dana', 1, 3)).toBe(false)
    issuerApi.issuePasses.mockResolvedValue({ journal_id: 5 })
    issuerApi.getSupply.mockResolvedValue(rows)
    expect(await useIssuerStore.getState().issue('Dana', 1, 3)).toBe(true)
    const [first, second] = issuerApi.issuePasses.mock.calls.map(([args]) => args.requestId)
    expect(second).toBe(first)
  })

  it('uses a new request number when the form changed', async () => {
    issuerApi.issuePasses.mockRejectedValueOnce(lost())
    await useIssuerStore.getState().issue('dana', 1, 3)
    issuerApi.issuePasses.mockResolvedValue({ journal_id: 6 })
    issuerApi.getSupply.mockResolvedValue(rows)
    await useIssuerStore.getState().issue('dana', 1, 4)
    const [first, second] = issuerApi.issuePasses.mock.calls.map(([args]) => args.requestId)
    expect(second).not.toBe(first)
  })
})