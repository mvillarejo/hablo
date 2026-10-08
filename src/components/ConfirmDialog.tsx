import { useEffect, useRef, type ReactNode } from "react";

/** Modal confirmation, same pattern and styling as `ConfigResetDialog`. */
export function ConfirmDialog({
  open,
  title,
  children,
  confirmLabel,
  busyLabel,
  isBusy,
  error,
  onCancel,
  onConfirm,
}: {
  open: boolean;
  title: string;
  children: ReactNode;
  confirmLabel: string;
  busyLabel: string;
  isBusy: boolean;
  error: string | null;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const dialogRef = useRef<HTMLDialogElement>(null);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    if (open && !dialog.open) dialog.showModal();
    return () => {
      if (dialog.open) dialog.close();
    };
  }, [open]);

  if (!open) return null;

  return (
    <dialog
      ref={dialogRef}
      className="config-reset-dialog"
      aria-labelledby="confirm-dialog-title"
      onCancel={(event) => {
        event.preventDefault();
        if (!isBusy) onCancel();
      }}
      onMouseDown={(event) => {
        if (isBusy) return;
        const bounds = event.currentTarget.getBoundingClientRect();
        const clickedBackdrop =
          event.clientX < bounds.left ||
          event.clientX > bounds.right ||
          event.clientY < bounds.top ||
          event.clientY > bounds.bottom;
        if (clickedBackdrop) onCancel();
      }}
    >
      <h3 id="confirm-dialog-title">{title}</h3>
      {children}
      {error && (
        <p className="text-danger" role="alert">
          {error}
        </p>
      )}
      <div className="config-reset-dialog-actions">
        <button
          type="button"
          className="btn btn-ghost"
          onClick={onCancel}
          disabled={isBusy}
        >
          Cancel
        </button>
        <button
          type="button"
          className="btn btn-danger"
          onClick={onConfirm}
          disabled={isBusy}
        >
          {isBusy ? busyLabel : confirmLabel}
        </button>
      </div>
    </dialog>
  );
}
