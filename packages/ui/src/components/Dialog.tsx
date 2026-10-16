import { X } from "lucide-react";
import { type ReactNode, useEffect, useId, useRef } from "react";

import { cx } from "./cx";

export interface DialogProps {
  open: boolean;
  onClose: () => void;
  title: string;
  description?: ReactNode;
  children?: ReactNode;
  /** Buttons along the bottom. */
  footer?: ReactNode;
  className?: string;
}

/**
 * A modal on the native `<dialog>`: focus is trapped and Escape closes it,
 * as the platform provides; the title labels it for assistive technology.
 */
export function Dialog({
  open,
  onClose,
  title,
  description,
  children,
  footer,
  className,
}: DialogProps) {
  const ref = useRef<HTMLDialogElement>(null);
  const titleId = useId();
  const descId = useId();

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    if (open && !el.open) el.showModal();
    if (!open && el.open) el.close();
  }, [open]);

  return (
    <dialog
      ref={ref}
      aria-labelledby={titleId}
      aria-describedby={description ? descId : undefined}
      onClose={onClose}
      onCancel={(e) => {
        e.preventDefault();
        onClose();
      }}
      className={cx(
        "m-auto w-[min(32rem,calc(100vw-2rem))] rounded-lg border border-border bg-surface-1 p-0 text-text-primary shadow-2",
        "backdrop:bg-[rgba(28,24,48,0.32)] backdrop:backdrop-blur-[2px]",
        className,
      )}
    >
      {open && (
        <div className="flex flex-col gap-4 p-5">
          <header className="flex items-start justify-between gap-4">
            <div>
              <h2 id={titleId} className="text-lg font-semibold tracking-tight">
                {title}
              </h2>
              {description && (
                <div id={descId} className="mt-1 text-sm text-text-secondary">
                  {description}
                </div>
              )}
            </div>
            <button
              type="button"
              onClick={onClose}
              aria-label="Close"
              className="rounded-sm p-1 text-text-muted hover:bg-surface-2 hover:text-text-primary"
            >
              <X className="size-4" aria-hidden="true" />
            </button>
          </header>
          {children}
          {footer && <footer className="flex justify-end gap-2">{footer}</footer>}
        </div>
      )}
    </dialog>
  );
}
