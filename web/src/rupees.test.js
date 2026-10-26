import { describe, expect, it } from 'vitest'
import { rupeesToMinor } from './rupees'

describe('rupeesToMinor', () => {
  it('converts whole rupees', () => {
    expect(rupeesToMinor('5000')).toBe(500_000)
  })

  it('converts rupees and paise exactly', () => {
    expect(rupeesToMinor('5000.5')).toBe(500_050)
    expect(rupeesToMinor('5000.50')).toBe(500_050)
    expect(rupeesToMinor('0.07')).toBe(7)
    expect(rupeesToMinor('19.99')).toBe(1_999)
  })

  it('ignores spaces around the number', () => {
    expect(rupeesToMinor('  250 ')).toBe(25_000)
  })

  it('refuses anything that is not a positive amount', () => {
    for (const text of ['', ' ', 'abc', '-5', '0', '0.00', '5.', '.5', '1.234', '1,000', '1e3', '99999999999']) {
      expect(rupeesToMinor(text), text).toBeNull()
    }
  })
})