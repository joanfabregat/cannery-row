// Applies the saved theme before the first paint, so a dark page never flashes
// light. A separate file rather than an inline script, so a strict CSP can apply.
try {
  const saved = localStorage.getItem("cannery-row.theme");
  const dark =
    saved === "dark" || (saved === "system" && matchMedia("(prefers-color-scheme: dark)").matches);
  document.documentElement.classList.toggle("dark", dark);
} catch {
  // Storage unavailable: the light default applies.
}
