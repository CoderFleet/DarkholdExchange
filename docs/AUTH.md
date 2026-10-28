# T3 Auth Contract — v1 (draft)

**Status:** draft, not frozen. Freeze it once T1 and T2 have read it.
**Owner:** T3 (gateway, login, roles)
**Consumers:** T1 (order endpoints), T2 (wallet, private feed), web app

This document defines login, sign-up and "who am I" for the gateway, and how a request proves who
it comes from. It replaces the temporary `x-dev-user` header described in
[`PROTOCOL.md`](PROTOCOL.md) §1, **without changing any command body**. Commands still never
contain a `user_id`; the gateway derives identity from the request.

---

## 1. Decisions

| # | Decision | Why |
|---|---|---|
| A1 | Identity is the existing `UserId` (`u64`). There is no separate identity id | `dx-types` already defines `UserId` as "a verified identity"; holding limits attach to it |
| A2 | Three roles: `user`, `admin`, `issuer` (lowercase strings) | Matches the spec; lowercase `snake_case` matches `ReasonCode` |
| A3 | Sign-up always creates role `user`. `admin` and `issuer` accounts exist only via the seed script | Otherwise anyone could register as admin |
| A4 | Username + password. The server stores only an Argon2 hash, never the password | Spec requires password hashing; real KYC/email stays out of scope (DECISIONS #11) |
| A5 | Session token is a JWT sent as `Authorization: Bearer <token>` | Standard; works for every REST call |
| A6 | JWT claims: `sub` (user id), `role`, `iat`, `exp`. Lifetime 2 hours (`DX_JWT_TTL_SECS` overrides it) | Short enough to limit a stolen token, long enough for a session |
| A7 | WebSocket carries the token as `?token=<jwt>` | Browsers cannot set headers on a WebSocket |
| A8 | Wrong username and wrong password give the same error | Does not reveal which usernames exist |
| A9 | Money, ids and quantities keep the existing rules: integers, `<= Number.MAX_SAFE_INTEGER`, no floats | DECISIONS #8, #9 |

---

## 2. Endpoints

### 2.1 Sign up

```http
POST /api/auth/signup
Content-Type: application/json
```

```json
{ "username": "stuti", "password": "correct horse battery" }
```

Rules: `username` is 3 to 32 characters of `a-z`, `0-9`, `_`; stored lowercase. `password` is 8 to
128 characters.

Response `201`:

```json
{
  "token": "<jwt>",
  "user": { "user_id": 7, "username": "stuti", "role": "user" }
}
```

### 2.2 Log in

```http
POST /api/auth/login
Content-Type: application/json
```

```json
{ "username": "stuti", "password": "correct horse battery" }
```

Response `200`: same body as sign-up.

### 2.3 Current user

```http
GET /api/me
Authorization: Bearer <jwt>
```

Response `200`:

```json
{ "user_id": 7, "username": "stuti", "role": "user" }
```

The web app calls this on load to check that a saved token is still valid.

### 2.4 Authenticated requests

Every endpoint except sign-up, login, `GET /api/health`, `GET /api/time` and the public market
reads needs the `Authorization` header. Role-restricted endpoints (`/api/admin/*`,
`/api/issuer/*`) return `403` for the wrong role.

---

## 3. Errors

Same shape as `PROTOCOL.md` §3: `{ "reason": "<ReasonCode>" }`.

| HTTP | `reason` | When |
|---|---|---|
| `400` | `invalid_request` | Malformed body, bad username or password format |
| `401` | `unauthorized` | Missing, malformed, expired or tampered token |
| `401` | `invalid_credentials` | Login with a wrong username or password |
| `403` | `forbidden` | Valid token, but the role may not use this endpoint |
| `409` | `username_taken` | Sign-up with an existing username |
| `429` | `rate_limited` | Too many requests; includes `Retry-After: <seconds>` |

**Shared error list:** `unauthorized`, `invalid_credentials`, `forbidden` and `username_taken` are
added to `ReasonCode` in `dx-types`, following the repo's contract-change rule: the Rust enum, the
regenerated `web/src/protocol/generated/ReasonCode.ts` (`just types`) and a fixture change go in
the same PR. The change only adds values, so existing code keeps compiling and existing codes keep
their meaning. `rate_limited` already exists.

**Status:** not done yet. Until it is, the gateway sends `unauthorized` and `forbidden` as plain
text in the same `{"reason": ...}` shape, so the website reads them the same way.

---

## 4. Rollout without breaking anyone

Rule: follow the work-distribution document. Where it assigns a file or feature to T3, T3 changes
it. Each change is small, backward compatible, in its own PR, and passes CI before it merges.

1. **Auth endpoints:** a new crate, added to the gateway router with one `.merge(...)` line.
2. **`dev_user()` in the gateway (done):** it is now `identify()` in `crates/gateway/src/main.rs`.
   If an `Authorization: Bearer` header is present it must hold a valid token, otherwise the answer
   is 401 and the dev header is never used instead. With no token, an **explicit** `x-dev-user`
   header works while `DX_DEV_AUTH=1` (the default); there is no default dev user. The load
   simulator sends the header explicitly. Endpoints that name a user in the address
   (`/api/users/{id}/balances` and `/api/users/{id}/orders`) let a logged-in user read only their
   own data (admins may read anyone's). `/api/dev/deposit` and `/api/dev/issue` answer 403 when
   `DX_DEV_AUTH=0`.
3. **`web/src/api.js` `request()`:** every call carries `Authorization: Bearer <token>` when a
   session exists. The browser app never sends `x-dev-user`, so it cannot impersonate a trader.
4. **Login, signup, role-protected routes, auth store:** new files.
5. **Switch-over (done):** `/markets/:id`, `/wallet` and `/history` are behind `RequireRole`; an
   anonymous visitor is redirected to `/login` and returned to the page after signing in. The
   terminal has no Identity dropdown and trades only as the logged-in user. The navbar hides
   private links while anonymous; `/auctions` stays readable but its create/accept controls
   require a login. Real runs set `DX_DEV_AUTH=0`.
   Still open: the public market-data WebSocket does not verify a token (row 7 of the checklist);
   it only carries market data, not account data.


## 5. Saving accounts, events and tiers

**Why.** The engine already survives a restart through the write-ahead log (its money and
markets come back). Accounts, events and tiers lived only in memory. After a restart the user ids
started again at 200, so a new sign-up could be given the id of an old account and inherit its
money from the log. Admin market ids also restarted at 100, so publishing a new tier failed. Now
they are saved in PostgreSQL.

**How it is switched on.** Set `DATABASE_URL`, for example
`postgres://darkhold:darkhold@localhost:5432/darkhold` (`docker compose up -d db` starts that
database). At start the gateway connects, creates three tables if they are missing
(`app_users`, `admin_events`, `admin_tiers`, see `crates/store/migrations`), loads everything,
and continues the ids from the highest saved one.

- **Real run (`DX_DEV_AUTH=0`):** the gateway refuses to start without `DATABASE_URL`, and also
  refuses if the database cannot be reached.
- **Dev run without `DATABASE_URL`:** everything works in memory as before, and a warning says
  the data is lost on restart. If the write-ahead log is on, delete the `data` folder before
  restarting, or new sign-ups will be given the old accounts' ids and money.

**What is saved.**
- Only the Argon2id password **hash** is saved, never a password.
- The demo accounts (`trader1`, `dummy_user` and the other seeded traders) are never saved.
- The admin and issuer accounts are saved only when their password comes from the environment
  (`DX_ADMIN_PASSWORD`, `DX_ISSUER_PASSWORD`). If the environment sets one, it wins over a saved
  row on every start, so changing the password and restarting takes effect.

**Order of work.** The database is written first and the in-memory copy changes only after the
write succeeded. If the write fails, the request answers 500 and nothing is created, so memory
and database never disagree. The database's unique rules are the final judge of duplicates
(username, and tier name per event ignoring capitals), so two requests at the same moment cannot
both win.

**Known gap.** Publishing a tier does two things: it creates the market in the engine, then
saves the tier as published. If the server crashes between the two, the tier is saved as a draft
while the engine already has the market, and publishing it again fails. This needs a manual fix
for now.

**Resetting in development.** The write-ahead log (the `data` folder) and the database describe the
same world: the log holds the engine's money and markets, the database holds who the accounts are
and which tiers are published. Reset them together. Deleting only one leaves them disagreeing (for
example a tier saved as published whose market is no longer in the engine). A log written by an
older version of the engine can also fail its state-hash check at start; the fix in development is
to clear both.

## 6. Issuer: creating passes

Passes are only ever created by issuing. Before this, the only way was the dev endpoint
`/api/dev/issue`, which exists only while `DX_DEV_AUTH=1`. The issuer endpoints work in a real run.

Both need a login token whose role is `issuer`. No token (the `x-dev-user` header does not count,
even in dev mode) answers `401 unauthorized`, any other role answers `403 forbidden`.

### 6.1 Issue passes

```http
POST /api/issuer/issue
Authorization: Bearer <issuer token>
Content-Type: application/json
```

```json
{ "username": "dana", "market_id": 1, "qty": 3, "client_request_id": 5001 }
```

Response `200`: `{ "journal_id": 26 }`.

- `username` is matched the same way as at login (capitals and spaces around it do not matter).
- `qty` is 1 to 1,000 per request. A bigger batch takes several requests, so a typo cannot flood a
  market. Otherwise `400 invalid_quantity`.
- Sending the same `client_request_id` again for the same user and market returns the same
  `journal_id` and creates nothing, so a retry after a timeout is safe.
- Errors: `404 unknown_user`, `404 unknown_market`.
- Each issue is written to the write-ahead log like any other engine change, and the gateway
  prints one line (issuer id, quantity, market, user, journal id).

### 6.2 Supply

```http
GET /api/issuer/supply
Authorization: Bearer <issuer token>
```

Response `200`: a list, one row per market, sorted by market id:

```json
[{ "market_id": 1, "name": "Day 1 General", "passes_held": 15 }]
```

`passes_held` is how many passes users hold now (available and reserved). Passes are never
destroyed, so this is also how many were issued so far.

### 6.3 Console

The issuer console is the `/issuer` page (only the `issuer` role can open it). It has a form
(username, market, quantity) and a table of the supply. If the connection drops while a request is
in flight, pressing the button again for the same user, market and quantity reuses the same
`client_request_id`, so the passes cannot be credited twice.

## 7. Seed script

`node scripts/seed.mjs` (or `just seed`) sets up demo traders through the real endpoints, so a real
run does not need `/api/dev/*`. The gateway must be running and at least one market must be
published (an admin publishes a tier first).

In order, it:
1. logs in as the issuer;
2. for each trader, signs up (or logs in if the account already exists);
3. tops the trader's wallet up to a target amount (the trader's own `/api/wallet/topup`; the issuer
   never creates cash);
4. credits passes of every market through `POST /api/issuer/issue`.

Settings are environment variables. The defaults suit a dev run:

| Variable | Default | Meaning |
|---|---|---|
| `DX_API` | `http://localhost:8080` | Where the gateway is |
| `DX_ISSUER_USER` / `DX_ISSUER_PASSWORD` | `issuer` / `issuer-pass-1` | Issuer login. The same variable the gateway reads: set it to the real one in a real run |
| `DX_SEED_USERS` | `demo1,demo2,demo3` | Trader usernames, comma separated |
| `DX_DEMO_PASSWORD` | `demo-pass-123` | Their password (a demo value: change it for anything shared) |
| `DX_SEED_CASH` | `10000` | Wallet target in rupees (0 to 100000) |
| `DX_SEED_PASSES` | `2` | Passes per market per trader (1 to 1000) |
| `DX_SEED_ROUND` | `1` | Raise it to give everyone more passes (see below) |

**Safe to run again.** The wallet is only topped up to the target, never past it, and the passes use
a fixed request number per market and round, so the server credits them once. Running it again
changes nothing. To give everybody more passes, run it with the next round, for example
`DX_SEED_ROUND=2`.

If it stops, it says why: the gateway is not running, the issuer login was refused, a trader
already exists with another password, or there are no markets yet.

## 8. Rate limits

Every request passes through one layer (`crates/gateway/src/rate.rs`) before any handler. It uses
token buckets (`crates/limits`): each key has a bucket that holds up to a *burst* of tokens and
refills at a steady rate per second. A request takes one token; with none left it is refused.

| Bucket | Key | Applies to | Default (burst / per second) |
|---|---|---|---|
| IP | source address | every request except health checks | 120 / 60 |
| Login | source address | `POST /api/auth/login` and `/signup` | 10 / 0.2 (one every 5 s) |
| User | user id | `POST`/`DELETE` of orders, commands, auctions, escrow, wallet top-up, public key | 30 / 15 |

A refused request gets **429**, `{"reason":"rate_limited"}` and a `Retry-After` header (whole
seconds). The web app already shows that message. A refused request costs nothing, so waiting the
stated time always works.

Notes:
- The user is taken from the validated token (or `x-dev-user` in dev mode). A bad token never
  touches a user's bucket, so nobody can use up someone else's allowance.
- `X-Forwarded-For` is ignored unless `DX_TRUST_PROXY=1`. Then only the last entry (added by our own
  proxy) counts; anything earlier was written by the client.
- At most 100 000 keys are kept per bucket. Idle ones are dropped; if the table is still full,
  new keys are refused rather than letting memory grow.
- Limits live in memory: a restart gives everyone a fresh allowance, and several gateway copies
  would each count on their own.

| Variable | Default | Meaning |
|---|---|---|
| `DX_RATE_LIMIT` | on | `off` (or `0`) disables all limits. The load test sets it (`DX_LOAD_RATE_LIMIT` overrides) |
| `DX_RATE_IP` / `DX_RATE_AUTH` / `DX_RATE_USER` | see table | `BURST/PER_SECOND`, for example `30/15` |
| `DX_TRUST_PROXY` | off | `1` behind a reverse proxy that sets `X-Forwarded-For` |

Not covered here: the WebSocket feeds are limited only by the IP bucket at connection time. A
client that keeps hitting these limits is asked to solve puzzles (section 9).

## 9. Proof-of-work

Rate limits stop a client from going too fast, but a bot can still keep trying forever. Proof-of-work
makes every attempt cost it computing time, and it makes the cost grow with bad behaviour. The
checking side is `crates/pow`; it is used by `crates/gateway/src/rate.rs`.

**Strikes.** Every time the rate limiter refuses a request (any 429), the client's address gets a
*strike*. Strikes are forgiven after 120 seconds without a new one (each new strike restarts the
wait).

**The rule.** While an address has strikes, its `POST /api/auth/login`, `/signup` and its commands
(the same list as the user bucket in section 8) must carry a solved puzzle in the `x-pow` header.
Without one the answer is **428**, `{"reason":"pow_required","bits":N}`. Reads, health checks and
addresses without strikes are never asked.

**The puzzle.** Find a number `nonce` so that the SHA-256 hash of the text `"<challenge>~<nonce>"`
starts with at least `bits` zero bits. Each extra bit doubles the work: 16 bits is about 65 000
hashes (a fraction of a second in a browser), 22 bits about 4 million (a few seconds).

| Strikes | Puzzle (defaults) |
|---|---|
| 0 | none |
| 1 | 16 bits |
| 2 | 18 bits |
| 3 | 20 bits |
| 4 or more | 22 bits (the cap) |

**The flow.**
1. The client sends a request and gets `428`.
2. It calls `GET /api/pow/challenge` and receives `{"challenge": "...", "bits": N, "ttl_secs": 120}`.
3. It solves the puzzle and repeats the request with `x-pow: <challenge>~<nonce>`.

**What a challenge is.** `v1.<expiry>.<bits>.<random>.<signature>`, signed with HMAC-SHA-256 under a
key that exists only in the server's memory. So the server stores nothing per challenge, and:
- it cannot be forged or made easier (changing the bits breaks the signature);
- it is tied to one client address, so it cannot be passed on;
- it expires after 120 seconds;
- it works once: used signatures are remembered until they expire (at most 100 000; if that table is
  full the server answers 428 until some expire).

**Solving a puzzle does not skip the rate limit.** The puzzle check comes first, then the usual
bucket. A client that is still too fast gets 429 again, which is another strike and a harder puzzle.
The puzzle is checked before the user bucket, so unsolved requests never touch anybody's allowance.

**In the browser.** `web/src/pow/` has the solver (its own SHA-256, because `crypto.subtle` is
asynchronous and needs HTTPS) and a Web Worker, so the page does not freeze. `fetchWithPuzzle`
wraps `fetch`: on a 428 it fetches a puzzle, solves it, and repeats the request once. It never
loops. `scripts/seed.mjs` does the same with Node's crypto.

| Variable | Default | Meaning |
|---|---|---|
| `DX_POW` | on | `off` (or `0`) turns puzzles off (rate limits stay). `DX_RATE_LIMIT=off` turns both off |
| `DX_POW_BITS` | `16/22` | `BASE/MAX`: difficulty after the first strike, and the cap (at most 30) |
| `DX_POW_WINDOW_SECS` | `120` | Seconds without a refusal before an address is forgiven |

Limits to know about: strikes and used challenges live in memory (a restart forgives everyone and
cancels open puzzles, and several gateway copies would each keep their own). Behind a proxy the
address is the one from `X-Forwarded-For` (see `DX_TRUST_PROXY`), so everyone behind one shared
address (a school, an office) shares the strikes. Proof-of-work slows a bot down; it does not make
it impossible, and a client with a lot of computing power pays less per attempt than a phone does.