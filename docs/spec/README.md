# Source specification

Copied from the Webster 2026 submission PDF so the whole team works from the same files.

| File | What it is |
|---|---|
| `darkhold-spec.pdf` | 10-page submission: problem, process flow, features, stack, architecture, verification plan |
| `architecture.png` | Infrastructure layout and write-path data flow |
| `class-diagram.png` | Initial class diagram (marked "open to changes") |

## Product in one paragraph

Darkhold Exchange is a secondary marketplace for passes and relics, built like an exchange: an
in-memory FIFO limit order book, price collars tied to face value, an atomic double-entry ledger,
Dutch-auction liquidation, frequent batch auctions with commit-reveal, escrow with signed delivery
receipts, and a Merkle-sum proof of solvency. Fairness and safety are enforced by the architecture,
not by policy.

## Hard requirements from the spec

- **Matching:** FIFO price-time priority, limit and market orders, cancel, self-trade prevention.
- **Fairness:** server-side collars derived from face value, per-identity holding limits, per-user and
  per-IP rate limits, adaptive proof-of-work, batch auctions with commit-reveal.
- **Settlement:** single-writer core, reservations before matching, balanced double-entry journal per
  fill, integer minor units, idempotency keys, conservation invariants.
- **Market data:** L2 snapshot + sequenced diffs over WebSockets, 20–50 ms batching, gap detection,
  resync from a cached snapshot, client-side book rebuild in a Web Worker.
- **Trust:** Merkle-sum solvency root verifiable in the browser, hash-chained audit log, escrow state
  machine with signed delivery receipts and timeout refunds.
- **Operations:** 1,000+ orders/s sustained, engine p99 in microseconds, end-to-end p99 < 10 ms,
  k6/Locust benchmarks, crash recovery from snapshot + log tail.
- **Demo features:** bot-vs-human simulator, live metrics/latency panel, order-book replay.

## Known wording/scope gaps to resolve in design

- The spec names TypeScript for the frontend; the scaffold is JavaScript. **Decision: stay JS** with a
  generated-type + contract-test guardrail (see [the decision log](../DECISIONS.md)).
- The spec names msgpack; the scaffold contracts are JSON. **Decision: msgpack on the wire, JSON
  fixtures as the reviewable schema.**
- The spec says "verified identity" but real KYC is out of scope for the demo. **Decision: simulated
  verified identities with per-identity holding limits.**
- The spec references `class.png`; the class diagram is included here as `class-diagram.png`.
