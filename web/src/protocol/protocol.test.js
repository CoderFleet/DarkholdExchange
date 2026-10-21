import { readFileSync } from 'node:fs'
import { expect, it } from 'vitest'
import { placeLimit, Side } from './index.js'

const fixture = (name) =>
  JSON.parse(readFileSync(new URL(`../../../contracts/${name}`, import.meta.url), 'utf8'))

it('placeLimit matches the shared contract fixture', () => {
  expect(
    placeLimit({ userId: 1, marketId: 1, side: Side.Buy, priceTicks: 100, qty: 1 }),
  ).toEqual(fixture('place_limit.json'))
})
