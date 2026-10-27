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
   is 401 and the dev header is never used instead. With no token, `x-dev-user` still works while
   `DX_DEV_AUTH=1` (the default), so the load simulator, the k6 scripts and every existing request
   keep working unchanged. Endpoints that name a user in the address (`/api/users/{id}/balances`
   and `/api/users/{id}/orders`) let a logged-in user read only their own data (admins may read
   anyone's). `/api/dev/deposit` and `/api/dev/issue` answer 403 when `DX_DEV_AUTH=0`.
3. **`web/src/api.js` `request()`:** if a saved token exists, add `Authorization: Bearer <token>`;
   otherwise behave exactly as today. The existing `x-dev-user` headers stay until the switch-over.
4. **Login, signup, role-protected routes, auth store:** new files.
5. **Switch-over (done):** `/market`, the wallet and the history pages use the logged-in user. The
   Identity dropdown shows only while nobody is logged in (dev). Real runs set `DX_DEV_AUTH=0`.
   Still open: the WebSocket does not check the token yet, and in a real run `/market` shows
   errors, instead of redirecting to the login page, when nobody is logged in.


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