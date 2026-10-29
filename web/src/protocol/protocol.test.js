import { readFileSync } from 'node:fs'
import { expect, it } from 'vitest'
import {
  cancelOrder,
  decodeServerMessage,
  encodeClientMessage,
  placeLimit,
  placeMarket,
  Side,
  subscribe,
} from './index.js'

const fixture = (name) =>
  JSON.parse(readFileSync(new URL(`../../../contracts/${name}`, import.meta.url), 'utf8'))

const goldFrame = (name) =>
  new Uint8Array(
    readFileSync(new URL(`../../../contracts/msgpack/${name}`, import.meta.url)),
  )

it('placeLimit matches the shared contract fixture', () => {
  expect(
    placeLimit({ clientOrderId: 1, marketId: 1, side: Side.Buy, priceTicks: 100, qty: 1 }),
  ).toEqual(fixture('place_limit.json'))
})

it('builds market and cancel commands', () => {
  expect(
    placeMarket({
      clientOrderId: 2,
      marketId: 1,
      side: Side.Sell,
      qty: 3,
      protectPriceTicks: 2500,
    }),
  ).toEqual({
    type: 'PlaceMarket',
    client_order_id: 2,
    market_id: 1,
    side: 'Sell',
    qty: 3,
    protect_price_ticks: 2500,
  })
  expect(cancelOrder({ clientOrderId: 3, orderId: 42 })).toEqual({
    type: 'Cancel',
    client_order_id: 3,
    order_id: 42,
  })
})

it('roundtrips the MessagePack subscription frame', () => {
  const frame = subscribe({ marketIds: [1, 3], depth: 20 })
  const encoded = encodeClientMessage(frame)
  expect(encoded).toBeInstanceOf(Uint8Array)
  expect(decodeServerMessage(encoded)).toEqual(frame)
})

it('decodes the golden MessagePack snapshot frame', () => {
  expect(decodeServerMessage(goldFrame('snapshot.msgpack'))).toEqual({
    type: 'Snapshot',
    snapshot: {
      market_id: 1,
      book_seq: 7,
      bids: [{ price_ticks: 5000, qty: 2 }],
      asks: [{ price_ticks: 5100, qty: 1 }],
      last_trade_price: 5050,
    },
  })
})

it('decodes the golden MessagePack subscribe frame', () => {
  expect(decodeServerMessage(goldFrame('subscribe.msgpack'))).toEqual({
    type: 'Subscribe',
    market_ids: [1, 3],
    depth: 20,
  })
})

it('decodes the golden MessagePack delta frame', () => {
  expect(decodeServerMessage(goldFrame('delta.msgpack'))).toEqual(fixture('delta.json'))
})

it('decodes the golden MessagePack resync frame', () => {
  expect(decodeServerMessage(goldFrame('resync_required.msgpack'))).toEqual(
    fixture('resync_required.json'),
  )
})

it('decodes the golden MessagePack metrics frame', () => {
  expect(decodeServerMessage(goldFrame('metrics.msgpack'))).toEqual(fixture('metrics.json'))
})

it('decodes the golden MessagePack balances push frame', () => {
  expect(decodeServerMessage(goldFrame('balances.msgpack'))).toEqual(fixture('balances.json'))
})

it('decodes the golden MessagePack private fill frame', () => {
  expect(decodeServerMessage(goldFrame('fill_self.msgpack'))).toEqual(fixture('fill_self.json'))
})

it('decodes the golden MessagePack escrow push frame', () => {
  expect(decodeServerMessage(goldFrame('escrow_update.msgpack'))).toEqual(
    fixture('escrow_update.json'),
  )
})
