import { Link, NavLink, useLocation, useNavigate } from 'react-router-dom'
import { useEngineStatus } from '../lib/useEngineStatus'
import { useAuthStore } from '../stores/useAuthStore'

const ENGINE_LABEL = {
  checking: 'Engine check',
  online: 'Engine online',
  offline: 'Engine offline',
}

const utilityLink = ({ isActive }) =>
  `shell-nav-utility${isActive ? ' active' : ''}`

// Top bar shown on every page. The structure follows a trading venue: brand and
// health at the left, the product map centered, account actions at the right.
export function Navbar() {
  const user = useAuthStore((state) => state.user)
  const logout = useAuthStore((state) => state.logout)
  const engine = useEngineStatus()
  const navigate = useNavigate()
  const location = useLocation()

  const loginState = { from: location.pathname }

  const handleLogout = () => {
    logout()
    navigate('/login', { replace: true })
  }

  return (
    <header className="shell-navbar">
      <div className="shell-nav-left">
        <Link to="/" className="shell-brand" aria-label="Darkhold Exchange home">
          <span className="shell-brand-mark">DX</span>
          <span className="shell-brand-name">Darkhold Exchange</span>
        </Link>

        <span className={`shell-engine ${engine}`} title="Gateway health">
          <span className="dot" />
          <span>{ENGINE_LABEL[engine]}</span>
          <span className="shell-engine-meta">· i64 core</span>
        </span>
      </div>

      <nav className="shell-nav-center" aria-label="Primary">
        <NavLink to="/markets">Markets</NavLink>
        <NavLink to="/auctions">Auctions</NavLink>
        {user && (
          <>
            <NavLink to="/wallet" className={utilityLink}>
              Wallet
            </NavLink>
            <NavLink to="/history" className={utilityLink}>
              Activity
            </NavLink>
          </>
        )}
        <NavLink to="/speed" className={utilityLink}>
          Speed
        </NavLink>
        <NavLink to="/verify" className={utilityLink}>
          Proof
        </NavLink>
        {user?.role === 'admin' && (
          <>
            <NavLink to="/admin" className={utilityLink}>
              Admin
            </NavLink>
            <NavLink to="/admin/disputes" className={utilityLink}>
              Disputes
            </NavLink>
          </>
        )}
        {user?.role === 'issuer' && (
          <NavLink to="/issuer" className={utilityLink}>
            Issuer
          </NavLink>
        )}
      </nav>

      <div className="shell-user">
        {user ? (
          <div className="shell-user-cluster">
            <div className="shell-identity">
              <span className="shell-user-avatar">
                {user.username.slice(0, 1).toUpperCase()}
              </span>
              <span className="shell-user-name">{user.username}</span>
              <span className={`shell-role role-${user.role}`}>{user.role}</span>
            </div>
            <button
              type="button"
              className="shell-logout"
              onClick={handleLogout}
              title="Sign out of exchange session"
            >
              <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4" />
                <polyline points="16 17 21 12 16 7" />
                <line x1="21" y1="12" x2="9" y2="12" />
              </svg>
              <span>Log out</span>
            </button>
          </div>
        ) : (
          <>
            <Link to="/login" state={loginState} className="shell-login">
              Log in
            </Link>
            <Link to="/signup" className="shell-signup">
              Create account
            </Link>
          </>
        )}
      </div>
    </header>
  )
}

export default Navbar
