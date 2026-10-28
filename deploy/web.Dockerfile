# The website, served by Caddy, which also forwards /api and /ws to the gateway.
# Build from the repository root:  docker build -f deploy/web.Dockerfile .

# ---- stage 1: build the static files -----------------------------------------------------------
FROM node:22-bookworm-slim AS build
WORKDIR /web
# The lock file first, so the install step is reused when only the code changes.
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
RUN npm run build

# ---- stage 2: serve ----------------------------------------------------------------------------
FROM caddy:2-alpine
COPY deploy/Caddyfile /etc/caddy/Caddyfile
COPY --from=build /web/dist /srv