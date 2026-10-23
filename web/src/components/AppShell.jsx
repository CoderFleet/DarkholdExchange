import { Link, Navigate, Route, Routes } from 'react-router-dom'
import App from '../App'
import { Navbar } from './Navbar'
import '../shell.css'

function NotFound() {
  return (
    <div className="loading-panel">
      <p>Page not found.</p>
      <Link to="/market">Go to the market</Link>
    </div>
  )
}

export function AppShell() {
  return (
    <>
      <Navbar />
      <Routes>
        <Route path="/" element={<Navigate to="/market" replace />} />
        <Route path="/market" element={<App />} />
        <Route path="*" element={<NotFound />} />
      </Routes>
    </>
  )
}