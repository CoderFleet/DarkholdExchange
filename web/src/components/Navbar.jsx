import { NavLink } from 'react-router-dom'

// Top bar shown on every page.
export function Navbar() {
  return (
    <header className="shell-navbar">
      <span className="shell-brand">Darkhold Exchange</span>
      <nav aria-label="Main">
        <NavLink to="/market">Market</NavLink>
      </nav>
    </header>
  )
}