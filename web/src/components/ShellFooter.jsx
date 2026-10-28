import { Link } from 'react-router-dom'
import { useEngineStatus } from '../lib/useEngineStatus'
import { useAuthStore } from '../stores/useAuthStore'

const REPO = 'https://github.com/CoderFleet/DarkholdExchange'

const STATUS_TEXT = {
  checking: 'Checking gateway',
  online: 'All systems operational',
  offline: 'Gateway offline',
}

export function ShellFooter() {
  const engine = useEngineStatus()
  const user = useAuthStore((state) => state.user)

  return (
    <footer className="shell-footer">
      <div className="shell-footer-main">
        <div className="shell-footer-brand">
          <Link to="/" className="shell-footer-logo">
            <span className="shell-brand-mark">DX</span>
            <strong>Darkhold Exchange</strong>
          </Link>
          <p>
            A deterministic secondary market for event passes. Integer ticks,
            bounded books, and proofs you can check.
          </p>
          <span className={`shell-footer-status ${engine}`}>
            <span className="dot" />
            {STATUS_TEXT[engine]}
          </span>
        </div>

        <nav className="shell-footer-column" aria-label="Product">
          <h3>Product</h3>
          <Link to="/markets">Markets</Link>
          <Link to="/auctions">Auctions</Link>
          {user && (
            <>
              <Link to="/wallet">Wallet</Link>
              <Link to="/history">Activity</Link>
            </>
          )}
        </nav>

        <nav className="shell-footer-column" aria-label="Trust and verification">
          <h3>Trust &amp; verification</h3>
          <Link to="/verify">Proof of solvency</Link>
          <Link to="/verify">Verify a balance</Link>
          {user && <Link to="/wallet">Escrow receipts</Link>}
          <Link to="/speed">Engine metrics</Link>
        </nav>

        <nav className="shell-footer-column" aria-label="Build and developers">
          <h3>Build &amp; developers</h3>
          <a
            href={`${REPO}/blob/main/docs/PROTOCOL.md`}
            target="_blank"
            rel="noreferrer"
          >
            Protocol
          </a>
          <a href={REPO} target="_blank" rel="noreferrer">
            GitHub
          </a>
          <a
            href={`${REPO}/blob/main/docs/SPEC_CHECKLIST.md`}
            target="_blank"
            rel="noreferrer"
          >
            Checklist
          </a>
        </nav>
      </div>

      <div className="shell-footer-bottom">
        <div className="shell-footer-meta">
          <span>Darkhold Exchange · Webster 2026</span>
          <span>Single-writer core · i64 minor units · no floats</span>
        </div>
        <div className="shell-footer-legal">
          <span className="shell-version">v0.1-prod</span>
          <span>© 2026 Darkhold Exchange</span>
        </div>
      </div>
    </footer>
  )
}
