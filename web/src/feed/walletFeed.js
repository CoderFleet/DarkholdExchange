// @ts-check
/**
 * Pure private-data projection for the per-user feed.
 *
 * The gateway pushes this user's own ledger changes (`Balances`), their fills
 * (`FillSelf`) and escrow record transitions (`EscrowUpdate`) on a token-checked
 * `/ws` stream. The wallet store owns the socket; this module owns the state
 * transitions so they mirror the gateway's `/api/wallet` projection exactly and
 * can be tested without a browser or a live gateway.
 */

/** @typedef {import('../protocol/generated/Fill').Fill} Fill */
/** @typedef {import('../protocol/generated/WsServerMessage').WsServerMessage} WsServerMessage */
/** @typedef {import('../protocol/generated/EscrowRecord').EscrowRecord} EscrowRecord */

/** Frames that belong to one user's private stream. Everything else on the
 * `/ws` socket is public market data the wallet store must never touch. */
const PRIVATE_FRAMES = new Set(['Balances', 'FillSelf', 'EscrowUpdate'])

/**
 * @param {WsServerMessage} message
 * @returns {boolean}
 */
export const isPrivateFrame = (message) =>
  Boolean(message) && PRIVATE_FRAMES.has(/** @type {any} */ (message).type)

/**
 * @typedef {{
 *   market_id: number,
 *   event_name: string,
 *   tier_name: string,
 *   avail: number,
 *   locked: number,
 *   held: number,
 * }} PassRow */
/**
 * @typedef {{
 *   cashAvail: number,
 *   cashLocked: number,
 *   passes: PassRow[],
 *   escrowItems: EscrowRecord[],
 *   escrowCash: number,
 *   lastSelfFill: Fill | null,
 * }} WalletFragment */

/**
 * The cash the buyer has handed to escrow and will only get back via a
 * delivery, a refund, or an admin ruling. Mirrors the gateway's
 * `/api/wallet` `escrow_cash` sum.
 *
 * @param {EscrowRecord[]} items
 * @param {number} userId
 * @returns {number}
 */
export const sumEscrowCash = (items, userId) =>
  (items ?? [])
    .filter(
      (record) =>
        record.state === 'pending_delivery' && record.buyer_id === userId,
    )
    .reduce(
      (sum, record) =>
        sum + record.notional + (record.fee_payer === record.buyer_id ? record.fee : 0),
      0,
    )

/**
 * Insert or replace one escrow record, keyed by trade id. The server sends the
 * full record on every transition (and once more as part of a fresh baseline
 * after a reconnect), so a merge is exactly right for both cases.
 *
 * @param {EscrowRecord[]} items
 * @param {EscrowRecord} record
 * @returns {EscrowRecord[]}
 */
export const upsertEscrowRecord = (items, record) => {
  const existing = [...(items ?? [])]
  const index = existing.findIndex(
    (item) => item.trade_id === record.trade_id,
  )
  if (index === -1) existing.push(record)
  else existing[index] = { ...existing[index], ...record }
  return existing
}

/**
 * Apply a pushed balance snapshot onto the REST-shaped pass rows.
 *
 * A push carries only `avail`/`locked` numbers and, at best, market names from
 * the market list — so rows already on record keep their event/tier naming and
 * escrow holds, and unknown markets fall back to generic names.
 *
 * @param {PassRow[]} existing
 * @param {import('../protocol/generated/Balance').Balance[]} balances
 * @param {import('../protocol/generated/MarketConfig').MarketConfig[]} [markets]
 * @returns {PassRow[]}
 */
export const mergePasses = (existing, balances, markets = []) => {
  /** @type {Map<number, PassRow>} */
  const rows = new Map((existing ?? []).map((row) => [row.market_id, { ...row }]))
  for (const balance of balances ?? []) {
    const { asset } = balance
    if (asset.kind !== 'pass') continue
    const marketId = asset.market_id
    const market = markets.find((m) => m.id === marketId)
    const previous = rows.get(marketId)
    rows.set(marketId, {
      market_id: marketId,
      event_name: previous?.event_name ??
        market?.name ?? `Market #${marketId}`,
      tier_name: previous?.tier_name ?? 'General',
      avail: balance.available,
      locked: balance.held,
      held: previous?.held ?? 0,
    })
  }
  return [...rows.values()]
}

/**
 * Recount which passes sit in escrow, mirroring the gateway. Every pending
 * record the user is a party to pins `qty` passes of that market as `held`,
 * even in markets the user owns no balance row for (those rows are created, as
 * the REST projection does).
 *
 * @param {PassRow[]} passes
 * @param {EscrowRecord[]} escrowItems
 * @param {number} userId
 * @param {import('../protocol/generated/MarketConfig').MarketConfig[]} [markets]
 * @returns {PassRow[]}
 */
export const applyEscrowHolds = (passes, escrowItems, userId, markets = []) => {
  /** @type {Map<number, PassRow>} */
  const rows = new Map((passes ?? []).map((row) => [row.market_id, { ...row }]))
  for (const record of escrowItems ?? []) {
    if (record.state !== 'pending_delivery') continue
    if (record.buyer_id !== userId && record.seller_id !== userId) continue
    if (rows.has(record.market_id)) continue
    const market = markets.find((m) => m.id === record.market_id)
    rows.set(record.market_id, {
      market_id: record.market_id,
      event_name: market?.name ?? `Market #${record.market_id}`,
      tier_name: 'General',
      avail: 0,
      locked: 0,
      held: 0,
    })
  }
  const counted = passRowsWithHolds(rows, escrowItems, userId)
  return [...counted.values()]
}

/**
 * @param {Map<number, PassRow>} rows
 * @param {EscrowRecord[]} escrowItems
 * @param {number} userId
 * @returns {Map<number, PassRow>}
 */
const passRowsWithHolds = (rows, escrowItems, userId) => {
  const withHolds = new Map([...rows].map(([id, row]) => [id, { ...row, held: 0 }]))
  for (const record of escrowItems ?? []) {
    if (record.state !== 'pending_delivery') continue
    if (record.buyer_id !== userId && record.seller_id !== userId) continue
    const row = withHolds.get(record.market_id)
    if (row) row.held = row.held + record.qty
  }
  return withHolds
}

/**
 * Apply one private server frame to the wallet fragment.
 *
 * @param {WsServerMessage} message
 * @param {WalletFragment} current
 * @param {{ userId: number, markets?: import('../protocol/generated/MarketConfig').MarketConfig[] }} context
 * @returns {{ fragment: WalletFragment, needsEscrowRefresh: boolean }}
 */
export const applyPrivateFrame = (message, current, { userId, markets = [] }) => {
  if (!isPrivateFrame(message)) return { fragment: current, needsEscrowRefresh: false }

  if (message.type === 'Balances') {
    // A frame naming someone else can never rewrite this store's numbers.
    if (message.user_id !== userId) {
      return { fragment: current, needsEscrowRefresh: false }
    }
    const cash = message.balances.find((b) => b.asset.kind === 'cash')
    return {
      fragment: {
        ...current,
        cashAvail: cash?.available ?? 0,
        cashLocked: cash?.held ?? 0,
        passes: mergePasses(current.passes, message.balances, markets),
        escrowCash: sumEscrowCash(current.escrowItems, userId),
      },
      needsEscrowRefresh: false,
    }
  }

  if (message.type === 'EscrowUpdate') {
    const escrowItems = upsertEscrowRecord(current.escrowItems, message.record)
    return {
      fragment: {
        ...current,
        escrowItems,
        escrowCash: sumEscrowCash(escrowItems, userId),
        passes: applyEscrowHolds(current.passes, escrowItems, userId, markets),
      },
      needsEscrowRefresh: false,
    }
  }

  // FillSelf: the accompanying Balances frame updates the ledger numbers; a
  // fill settled into escrow also creates a brand-new record that the server
  // does not push, so the store should pick it up over REST once.
  if (message.type !== 'FillSelf') return { fragment: current, needsEscrowRefresh: false }
  return {
    fragment: { ...current, lastSelfFill: message.fill },
    needsEscrowRefresh: message.fill.escrow_id != null,
  }
}

/**
 * @returns {WalletFragment}
 */
export const createWalletFragment = () => ({
  cashAvail: 0,
  cashLocked: 0,
  passes: [],
  escrowItems: [],
  escrowCash: 0,
  lastSelfFill: null,
})
