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
        <NavLink to="/markets">Events</NavLink>
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
          <>
            <span className="shell-identity">
              {user.username}
              <span className="shell-role">{user.role}</span>
            </span>
            <button type="button" className="shell-logout" onClick={handleLogout}>
              Log out
            </button>
          </>
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
