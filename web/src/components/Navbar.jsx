import { NavLink, useNavigate } from 'react-router-dom'
import { useAuthStore } from '../stores/useAuthStore'

// Top bar shown on every page. NavLink works like a normal link, but it also
// knows whether it points at the page you are on, so it can highlight itself.
export function Navbar() {
  const user = useAuthStore((state) => state.user)
  const logout = useAuthStore((state) => state.logout)
  const navigate = useNavigate()

  // Log out and go to a clean login page, so the next person to log in is not sent to
  // the page the previous person was on.
  const handleLogout = () => {
    logout()
    navigate('/login', { replace: true })
  }

  return (
    <header className="shell-navbar">
      <span className="shell-brand">Darkhold Exchange</span>
      <nav aria-label="Main">
        <NavLink to="/market">Market</NavLink>
        <NavLink to="/wallet">Wallet</NavLink>
        <NavLink to="/history">History</NavLink>
        <NavLink to="/verify">Verify</NavLink>
        {user?.role === 'admin' && <NavLink to="/admin">Admin</NavLink>}
        {user?.role === 'issuer' && <NavLink to="/issuer">Issuer</NavLink>}
      </nav>
      <div className="shell-user">
        {user ? (
          <>
            <span>
              {user.username} · {user.role}
            </span>
            <button type="button" className="shell-logout" onClick={handleLogout}>
              Log out
            </button>
          </>
        ) : (
          <NavLink to="/login">Log in</NavLink>
        )}
      </div>
    </header>
  )
}
export default Navbar