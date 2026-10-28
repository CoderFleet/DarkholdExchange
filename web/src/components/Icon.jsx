/**
 * Small inline icon set so the interface keeps the terminal's thin-weight
 * visual language without pulling in an icon font.
 */

/** @param {{ size?: number, className?: string }} props */
export function ArrowRight({ size = 14, className = '' }) {
  return (
    <svg
      className={className}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.7"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      <path d="M4 12h15" />
      <path d="m13 6 6 6-6 6" />
    </svg>
  )
}

/** @param {{ size?: number, className?: string }} props */
export function DataObjectIcon({ size = 16, className = '' }) {
  return (
    <svg
      className={className}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      <path d="M9 4H7a3 3 0 0 0-3 3v2.5a2.5 2.5 0 0 1-2 2.5 2.5 2.5 0 0 1 2 2.5V17a3 3 0 0 0 3 3h2" />
      <path d="M15 4h2a3 3 0 0 1 3 3v2.5a2.5 2.5 0 0 0 2 2.5 2.5 2.5 0 0 0-2 2.5V17a3 3 0 0 1-3 3h-2" />
    </svg>
  )
}

/** @param {{ size?: number, className?: string }} props */
export function MemoryIcon({ size = 16, className = '' }) {
  return (
    <svg
      className={className}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      <rect x="7" y="7" width="10" height="10" rx="1.5" />
      <rect x="10.5" y="10.5" width="3" height="3" rx="0.5" />
      <path d="M9 3v4M15 3v4M9 17v4M15 17v4M3 9h4M3 15h4M17 9h4M17 15h4" />
    </svg>
  )
}

/** @param {{ size?: number, className?: string }} props */
export function ShieldCheckIcon({ size = 16, className = '' }) {
  return (
    <svg
      className={className}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      <path d="M12 3 5 6v5c0 4.2 3 7.4 7 9 4-1.6 7-4.8 7-9V6l-7-3Z" />
      <path d="m9 12 2 2 4-4" />
    </svg>
  )
}
