// Sets up demo traders through the real endpoints, so no dev endpoint is needed:
//   1. log in as the issuer,
//   2. for each demo trader: sign up (or log in if the account already exists),
//   3. top the trader's wallet up to a target amount,
//   4. credit a few passes of every published market through POST /api/issuer/issue.
//
// Safe to run again: the wallet is only topped up to the target (never past it), and the passes
// use fixed request numbers, so the server counts them once. To give everybody more passes, run
// it again with a higher DX_SEED_ROUND (see docs/AUTH.md section 7).
//
// Usage:  node scripts/seed.mjs        (or: just seed)

import { createHash } from 'node:crypto'

const API = (process.env.DX_API ?? 'http://localhost:8080').replace(/\/+$/, '')
// The same names the gateway reads. The default only matches a dev run (DX_DEV_AUTH on).
const ISSUER_USER = process.env.DX_ISSUER_USER ?? 'issuer'
const ISSUER_PASSWORD = process.env.DX_ISSUER_PASSWORD ?? 'issuer-pass-1'
const DEMO_PASSWORD = process.env.DX_DEMO_PASSWORD ?? 'demo-pass-123'
const TRADERS = (process.env.DX_SEED_USERS ?? 'demo1,demo2,demo3')
  .split(',')
  .map((name) => name.trim().toLowerCase())
  .filter(Boolean)
const PASSES = Number(process.env.DX_SEED_PASSES ?? 2)
const CASH_RUPEES = Number(process.env.DX_SEED_CASH ?? 10000)
const ROUND = Number(process.env.DX_SEED_ROUND ?? 1)

// Request numbers for the passes. Far above the ones the console makes (it uses the clock), so
// the two never collide. One number per market and round: the same round never credits twice.
const REQUEST_BASE = 7_000_000_000

const fail = (message) => {
  console.error(`seed: ${message}`)
  process.exit(1)
}

const whole = (value, min, max) => Number.isInteger(value) && value >= min && value <= max
if (TRADERS.length === 0) fail('DX_SEED_USERS has no names.')
if (!whole(PASSES, 1, 1000)) fail('DX_SEED_PASSES must be a whole number from 1 to 1000.')
if (!whole(CASH_RUPEES, 0, 100_000)) fail('DX_SEED_CASH must be a whole number of rupees from 0 to 100000.')
if (!whole(ROUND, 1, 1_000_000)) fail('DX_SEED_ROUND must be a whole number from 1 up.')

/** Sends one request. Returns { status, body }. Stops the script if the gateway cannot be reached. */
async function call(path, options = {}) {
  // The gateway rate-limits. On "429 wait N seconds" the script waits and tries again; on
  // "428 solve a puzzle first" it solves one (docs/AUTH.md section 9) and tries again.
  let puzzle
  for (let attempt = 0; ; attempt += 1) {
    const answer = await send(path, { ...options, puzzle })
    puzzle = undefined
    if (attempt >= 5) return answer
    if (answer.status === 428 && answer.body?.reason === 'pow_required') {
      puzzle = await solvePuzzle()
    } else if (answer.status === 429) {
      await new Promise((resolve) => setTimeout(resolve, Math.max(1, answer.retryAfter) * 1000))
    } else {
      return answer
    }
  }
}

/** Asks for a puzzle and solves it: the SHA-256 of "<challenge>~<nonce>" must start with enough zero bits. */
async function solvePuzzle() {
  const offer = await send('/api/pow/challenge', {})
  if (offer.status !== 200) fail(`cannot get a puzzle: HTTP ${offer.status}`)
  const { challenge, bits } = offer.body
  for (let nonce = 0; ; nonce += 1) {
    const hash = createHash('sha256').update(`${challenge}~${nonce}`).digest()
    let zeros = 0
    for (const byte of hash) {
      if (byte === 0) zeros += 8
      else {
        zeros += Math.clz32(byte) - 24
        break
      }
    }
    if (zeros >= bits) return `${challenge}~${nonce}`
  }
}

async function send(path, { method = 'GET', token, body, puzzle } = {}) {
  const headers = {}
  if (puzzle) headers['x-pow'] = puzzle
  if (body !== undefined) headers['content-type'] = 'application/json'
  if (token) headers.authorization = `Bearer ${token}`
  let response
  try {
    response = await fetch(`${API}${path}`, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
    })
  } catch {
    fail(`cannot reach the gateway at ${API}. Is it running (just api)?`)
  }
  const text = await response.text()
  let parsed = null
  try {
    parsed = text ? JSON.parse(text) : null
  } catch {
    // Not JSON: keep null, the status is enough.
  }
  const retryAfter = Number(response.headers.get('retry-after')) || 1
  return { status: response.status, body: parsed, retryAfter }
}

/** Fails with a readable line unless the answer was a success. */
const must = (answer, what) => {
  if (answer.status < 200 || answer.status > 299) {
    fail(`${what} failed: HTTP ${answer.status} ${answer.body?.reason ?? ''}`.trim())
  }
  return answer.body
}

const rupees = (minor) =>
  `₹${(Number(minor) / 100).toLocaleString('en-IN', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`

async function issuerToken() {
  const answer = await call('/api/auth/login', {
    method: 'POST',
    body: { username: ISSUER_USER, password: ISSUER_PASSWORD },
  })
  if (answer.status === 401) {
    fail(`the issuer login was refused. Set DX_ISSUER_USER and DX_ISSUER_PASSWORD to the real ones.`)
  }
  const body = must(answer, 'issuer login')
  if (body.user.role !== 'issuer') fail(`"${ISSUER_USER}" is not an issuer account.`)
  return body.token
}

/** Signs the trader up, or logs in when the account already exists. Returns { token, created }. */
async function ensureTrader(username) {
  const credentials = { username, password: DEMO_PASSWORD }
  const signup = await call('/api/auth/signup', { method: 'POST', body: credentials })
  if (signup.status === 201) return { token: signup.body.token, created: true }
  if (signup.status !== 409) must(signup, `sign-up of ${username}`)
  const login = await call('/api/auth/login', { method: 'POST', body: credentials })
  if (login.status === 401) {
    fail(`"${username}" already exists with a different password. Set DX_DEMO_PASSWORD, or pick other names with DX_SEED_USERS.`)
  }
  return { token: must(login, `login of ${username}`).token, created: false }
}

async function main() {
  console.log(`seed: using ${API}`)
  const issuer = await issuerToken()

  const supply = must(await call('/api/issuer/supply', { token: issuer }), 'reading the markets')
  if (supply.length === 0) {
    fail('there are no markets yet. Publish a tier in the admin console first, then run this again.')
  }

  const target = CASH_RUPEES * 100
  for (const username of TRADERS) {
    const { token, created } = await ensureTrader(username)

    // Top up only to the target, so running again never piles up money.
    const wallet = must(await call('/api/wallet', { token }), `reading ${username}'s wallet`)
    if (wallet.cash_avail < target) {
      must(
        await call('/api/wallet/topup', { method: 'POST', token, body: { amount: target - wallet.cash_avail } }),
        `top-up of ${username}`,
      )
    }

    for (const market of supply) {
      must(
        await call('/api/issuer/issue', {
          method: 'POST',
          token: issuer,
          body: {
            username,
            market_id: market.market_id,
            qty: PASSES,
            client_request_id: REQUEST_BASE + ROUND * 1000 + market.market_id,
          },
        }),
        `issuing ${market.name} to ${username}`,
      )
    }

    const after = must(await call('/api/wallet', { token }), `reading ${username}'s wallet`)
    const holdings = after.passes.map((pass) => `${pass.event_name} ${Number(pass.avail) + Number(pass.locked)}`)
    console.log(
      `seed: ${username} (${created ? 'created' : 'existing'}) cash ${rupees(after.cash_avail)}, passes: ${holdings.join(', ')}`,
    )
  }
  console.log('seed: done.')
}

main()