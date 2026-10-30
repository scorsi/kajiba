/** Keep the enrollment secret in memory, removing it from the current history entry. */
export function takeEnrollmentToken(): string {
  const token = location.hash.slice(1);
  history.replaceState(history.state, "", location.pathname + location.search);
  return token;
}
