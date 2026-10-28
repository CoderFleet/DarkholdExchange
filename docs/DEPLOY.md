# Deployment (Docker, Caddy)

One command runs the whole exchange on your own computer:

    docker compose up --build

Then open <http://localhost:8000>. The first build takes several minutes (it compiles the Rust
gateway and builds the website). Later builds reuse what did not change.

## 1. What runs

```
browser ──► web (Caddy, port 8000) ─┬─ /api/*, /ws, /health ─► gateway (Rust, port 8080, not published)
                                    │                              │
                                    │                              ├─► db (PostgreSQL 16)
                                    │                              └─► wal volume (engine log)
                                    └─ everything else ──────► the website's files
```

| Service | Image built from | Job |
|---|---|---|
| `db` | `postgres:16` | accounts, events and tiers (docs/AUTH.md section 5) |
| `gateway` | `deploy/gateway.Dockerfile` | engine, ledger, REST and WebSocket API |
| `web` | `deploy/web.Dockerfile` + `deploy/Caddyfile` | serves the website, forwards `/api` and `/ws` to the gateway |

Only `web` is reachable from outside, and only from this computer (`127.0.0.1`). The gateway has
no published port, so nobody can skip the proxy.

## 2. Settings

The compose file runs the gateway as a real run: `DX_DEV_AUTH=0`, a database, a fixed signing
secret. It has demo values so the one command works; **they are for your own computer only.**
To use real ones, copy `.env.example` to `.env` and set:

| Variable | Meaning |
|---|---|
| `DX_JWT_SECRET` | signs login tokens, 32+ characters (docs/AUTH.md) |
| `DX_ADMIN_PASSWORD`, `DX_ISSUER_PASSWORD` | passwords of the `admin` and `issuer` accounts |
| `DX_HTTP_PORT` | port on this computer (default 8000) |

`DX_TRUST_PROXY=1` is set, so the rate limits (docs/AUTH.md section 8) use the visitor address
that Caddy puts last in `X-Forwarded-For`. Caddy replaces any `X-Forwarded-For` the visitor sent,
so it cannot be faked.

## 3. First use

The first start has an admin, an issuer and the demo markets, but no traders and no money. Create
demo traders with the seed script, pointed at the proxy:

    DX_API=http://localhost:8000 DX_ISSUER_PASSWORD=demo-issuer-password node scripts/seed.mjs

(PowerShell: `$env:DX_API="http://localhost:8000"; $env:DX_ISSUER_PASSWORD="demo-issuer-password"; node scripts/seed.mjs`)

Then log in as `demo1` / `demo-pass-123`.

## 4. Starting over and keeping data

- `docker compose down` stops everything and **keeps** the data (two volumes: `pgdata` for the
  database, `wal` for the engine's log).
- `docker compose down -v` also **deletes both volumes**. They must always be reset together:
  the engine log and the database both remember accounts, and half a reset hands out old ids
  again (README, Persistence).
- `docker compose up -d db` (`just db-up`) still starts only the database, for development.

## 5. Why Caddy

- WebSockets and long-lived connections pass through with no extra setting.
- It compresses text (gzip and zstd) and caches the hashed files under `/assets` for a year. The
  home page is never cached, so a new release shows up at once. A missing asset is a 404.
- It adds `X-Content-Type-Options`, `X-Frame-Options` and `Referrer-Policy`.
- No access log is switched on, on purpose: the feed's address carries the login token
  (docs/AUTH.md section 2.5) and a log would write it down.
- For a real domain, change `:80` in `deploy/Caddyfile` to the domain name and publish ports 80
  and 443. Caddy then gets and renews an HTTPS certificate by itself.

## 6. What was checked

- The Caddyfile (`caddy validate`) and `docker compose config`.
- The whole stack behind a real Caddy with real PostgreSQL, run directly: pages, the `/markets/1`
  link fallback, asset 404 and caching headers, gzip, login with the admin password (and not with
  the demo one), the dev header refused, the WebSocket checks through the proxy, a faked
  `X-Forwarded-For` not dodging a rate limit, the seed script through the proxy.
- The real containers (2026-10-28, Docker Desktop on Windows): `docker compose up --build` built
  both images, all three services came up healthy, the seed script ran against
  `http://localhost:8000`, and `demo1` logged in, saw the live book with the status **Live**, and
  had 1 WebSocket client connected.

Not checked: HTTPS with a real domain; running behind another reverse proxy.

## 7. Known limits

- The image build copies the whole source, so any code change recompiles the gateway's own
  crates (the cache mounts keep the dependencies).
- Behind Docker Desktop, every visitor can look like the same address to the gateway, so the
  per-address limits are shared. Fine for a demo.
- Nothing here backs up the volumes.