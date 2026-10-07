/** A full-page navigation away from the app (the identity provider's logout, for example). */
export function leaveApp(url: string): void {
  window.location.assign(url);
}
