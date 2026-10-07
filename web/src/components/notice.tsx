/** A confirmation after an action, announced politely to screen readers. */
export function Notice({ children }: { children: string | null }) {
  if (children === null) return null;
  return (
    <p
      role="status"
      className="mb-6 rounded-md border border-status-success/40 bg-status-success-bg px-4 py-3 text-sm text-status-success"
    >
      {children}
    </p>
  );
}
