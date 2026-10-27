import { describe, expect, it } from 'vitest'
import { bytesToHex, receiptMessage } from './escrowReceipt'

describe('escrow receipt message', () => {
  it('matches the Rust canonical framing', () => {
    const message = receiptMessage({
      trade_id: 1,
      market_id: 3,
      qty: 2,
      buyer_id: 10,
      seller_id: 20,
      delivered_at_ms: 30,
    })
    expect(bytesToHex(message)).toBe(
      '6461726b686f6c642d657363726f772d726563656970742d7631' +
        '0000000000000001' +
        '0000000000000003' +
        '0000000000000002' +
        '000000000000000a' +
        '0000000000000014' +
        '000000000000001e',
    )
  })
})
