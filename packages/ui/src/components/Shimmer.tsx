import { cx } from "./cx";

/**
 * Status text with a moving sheen ("Correlating…"); static under
 * prefers-reduced-motion, and announced politely to screen readers.
 */
export function Shimmer({ children, className }: { children: string; className?: string }) {
  return (
    <span
      role="status"
      aria-live="polite"
      className={cx(
        "bg-[linear-gradient(110deg,var(--color-text-secondary)_35%,var(--color-vivid-verdigris)_50%,var(--color-text-secondary)_65%)]",
        "bg-[length:250%_100%] bg-clip-text text-transparent",
        "animate-[wyrd-shimmer_1.6s_linear_infinite] motion-reduce:animate-none motion-reduce:text-text-secondary",
        className,
      )}
    >
      {children}
    </span>
  );
}
