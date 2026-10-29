import { describe, expect, it } from 'vitest'
import {
  applyEscrowHolds,
  applyPrivateFrame,
  createWalletFragment,
  isPrivateFrame,
  mergePasses,
  sumEscrowCash,
  upsertEscrowRecord,
} from './walletFeed.js'

const market = (id, name, eventId = 1) => ({ id, name, event_id: eventId })

const passBalance = (marketId, available, held) => ({
  asset: { kind: 'pass', market_id: marketId },
  available,
  held,
})

const record = (overrides = {}) => ({
  trade_id: 31,
  market_id: 1,
  buyer_id: 9,
  seller_id: 2,
  qty: 2,
  notional: 100_000,
  fee: 1_500,
  fee_payer: 9,
  state: 'pending_delivery',
  created_at_ms: 1_000,
  deadline_ms: 301_000,
  delivered_at_ms: null,
  seller_public_key: [26],
  ...overrides,
})

const fill = (overrides = {}) => ({
  trade_id: 31,
  market_id: 1,
  maker_order_id: 11,
  taker_order_id: 12,
  maker_user_id: 2,
  taker_user_id: 9,
  maker_side: 'Sell',
  price_ticks: 5_000,
  qty: 2,
  seq: 77,
  ...overrides,
})

const fragment = (overrides = {}) => ({
  ...createWalletFragment(),
  ...overrides,
})

describe('private frame classification', () => {
  it('accepts only the per-user ledger frames', () => {
    expect(isPrivateFrame({ type: 'Balances', user_id: 9, balances: [] })).toBe(true)
    expect(isPrivateFrame({ type: 'FillSelf', fill: fill() })).toBe(true)
    expect(isPrivateFrame({ type: 'EscrowUpdate', record: record() })).toBe(true)
    expect(isPrivateFrame({ type: 'Trade', fill: fill() })).toBe(false)
    expect(isPrivateFrame({ type: 'Snapshot' })).toBe(false)
    expect(isPrivateFrame({ type: 'Pong' })).toBe(false)
  })
})

describe('sumEscrowCash', () => {
  it('sums the pending escrow of the buyer, including their own fee share', () => {
    const items = [
      record(),
      record({ trade_id: 32, fee_payer: 2 }), // seller pays the fee here
      record({ trade_id: 33, state: 'delivered' }), // no longer pending
      record({ trade_id: 34, buyer_id: 8 }), // someone else's escrow
    ]
    expect(sumEscrowCash(items, 9)).toBe(201_500)
  })

  it('is zero without any pending escrow', () => {
    expect(sumEscrowCash([], 9)).toBe(0)
    expect(sumEscrowCash([record({ trade_id: 35, state: 'refunded' })], 9)).toBe(0)
  })
})

describe('upsertEscrowRecord', () => {
  it('replaces a known trade in place and appends new ones', () => {
    const items = upsertEscrowRecord([record()], record({ state: 'refunded' }))
    expect(items).toHaveLength(1)
    expect(items[0].state).toBe('refunded')

    const two = upsertEscrowRecord(items, record({ trade_id: 32 }))
    expect(two).toHaveLength(2)
    expect(two.map((item) => item.trade_id)).toEqual([31, 32])
  })
})

describe('mergePasses', () => {
  it('overwrites avail and locked, and keeps the row naming and escrow holds', () => {
    const rows = [
      {
        market_id: 1,
        event_name: 'Day 1 General',
        tier_name: 'General',
        avail: 10,
        locked: 2,
        held: 3,
      },
    ]
    const next = mergePasses(rows, [
      { asset: { kind: 'cash' }, available: 5_000, held: 0 },
      passBalance(1, 8, 6),
      passBalance(2, 4, 0),
    ], [market(2, 'Obsidian Sigil', 12)])

    expect(next).toEqual([
      {
        market_id: 1,
        event_name: 'Day 1 General',
        tier_name: 'General',
        avail: 8,
        locked: 6,
        held: 3,
      },
      {
        market_id: 2,
        event_name: 'Obsidian Sigil',
        tier_name: 'General',
        avail: 4,
        locked: 0,
        held: 0,
      },
    ])
  })

  it('falls back to a generic name for an unknown market', () => {
    const rows = mergePasses([], [passBalance(7, 1, 0)], [])
    expect(rows[0].event_name).toBe('Market #7')
  })
})

describe('applyEscrowHolds', () => {
  it('counts pending records as held pass rows for both sides', () => {
    const rows = applyEscrowHolds(
      [
        { market_id: 1, event_name: 'Day 1 General', tier_name: 'General', avail: 0, locked: 0, held: 9 },
      ],
      [record(), record({ trade_id: 32, qty: 1, state: 'delivered' })],
      2,
    )
    expect(rows[0].held).toBe(2)
  })

  it('creates a row for an escrow market the user holds no balance in', () => {
    const rows = applyEscrowHolds([], [record({ market_id: 3 })], 9, [market(3, 'Relic')])
    expect(rows).toEqual([
      {
        market_id: 3,
        event_name: 'Relic',
        tier_name: 'General',
        avail: 0,
        locked: 0,
        held: 2,
      },
    ])
  })

  it('ignores stale-hold noise by recomputing from zero every time', () => {
    const once = applyEscrowHolds([], [record({ qty: 5 })], 9)
    const twice = applyEscrowHolds(once, [record({ qty: 5 })], 9)
    expect(twice[0].held).toBe(5)
  })
})

describe('applyPrivateFrame', () => {
  const context = { userId: 9, markets: [market(2, 'Obsidian Sigil', 12)] }

  it('ignores public market frames entirely', () => {
    const current = fragment()
    const result = applyPrivateFrame({ type: 'Trade', fill: fill() }, current, context)
    expect(result.fragment).toBe(current)
    expect(result.needsEscrowRefresh).toBe(false)
  })

  it('ignores a balances frame addressed to another user', () => {
    const current = fragment({ cashAvail: 12 })
    const result = applyPrivateFrame(
      { type: 'Balances', user_id: 5, balances: [] },
      current,
      context,
    )
    expect(result.fragment).toBe(current)
  })

  it('applies the cash and pass ledger from a balances frame', () => {
    const result = applyPrivateFrame(
      {
        type: 'Balances',
        user_id: 9,
        balances: [
          { asset: { kind: 'cash' }, available: 250_000, held: 4_000 },
          passBalance(2, 3, 1),
        ],
      },
      fragment({ escrowItems: [record()] }),
      context,
    )

    expect(result.fragment.cashAvail).toBe(250_000)
    expect(result.fragment.cashLocked).toBe(4_000)
    expect(result.fragment.escrowCash).toBe(101_500)
    expect(result.fragment.passes).toHaveLength(1)
    expect(result.fragment.passes[0]).toMatchObject({ market_id: 2, avail: 3, locked: 1 })
    expect(result.needsEscrowRefresh).toBe(false)
  })

  it('upserts an escrow record and recounts holds and escrow cash', () => {
    const base = fragment({
      passes: [{ market_id: 1, event_name: 'Day 1 General', tier_name: 'General', avail: 4, locked: 0, held: 0 }],
    })
    const created = applyPrivateFrame({ type: 'EscrowUpdate', record: record() }, base, context)
    expect(created.fragment.escrowItems).toHaveLength(1)
    expect(created.fragment.escrowCash).toBe(101_500)
    expect(created.fragment.passes[0].held).toBe(2)

    // A transition to delivered both reverts the holds and empties the cash.
    const delivered = applyPrivateFrame(
      { type: 'EscrowUpdate', record: record({ state: 'delivered', delivered_at_ms: 5_000 }) },
      created.fragment,
      context,
    )
    expect(delivered.fragment.escrowCash).toBe(0)
    expect(delivered.fragment.passes[0].held).toBe(0)
    expect(delivered.needsEscrowRefresh).toBe(false)
  })

  it('records an own fill and asks for escrow refresh only when escrowed', () => {
    const spot = applyPrivateFrame({ type: 'FillSelf', fill: fill() }, fragment(), context)
    expect(spot.fragment.lastSelfFill).toEqual(fill())
    expect(spot.needsEscrowRefresh).toBe(false)

    const escrowed = applyPrivateFrame(
      { type: 'FillSelf', fill: fill({ trade_id: 32, escrow_id: 5 }) },
      spot.fragment,
      context,
    )
    expect(escrowed.fragment.lastSelfFill.trade_id).toBe(32)
    expect(escrowed.needsEscrowRefresh).toBe(true)
  })
})
