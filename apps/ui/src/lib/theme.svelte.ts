/**
 * Theme choice: `system` follows `prefers-color-scheme`; `light`/`dark` force
 * it via `data-theme` on <html>. Persisted per browser (convenience only).
 * `app.html` applies the stored value before first paint to avoid a flash.
 */

export type ThemeChoice = "system" | "light" | "dark";

const KEY = "kajiba-theme";

function read(): ThemeChoice {
  try {
    const v = localStorage.getItem(KEY);
    return v === "light" || v === "dark" ? v : "system";
  } catch {
    return "system";
  }
}

export const theme = $state<{ choice: ThemeChoice }>({ choice: read() });

export function setTheme(choice: ThemeChoice) {
  theme.choice = choice;
  const root = document.documentElement;
  if (choice === "system") root.removeAttribute("data-theme");
  else root.setAttribute("data-theme", choice);
  try {
    if (choice === "system") localStorage.removeItem(KEY);
    else localStorage.setItem(KEY, choice);
  } catch {
    /* storage unavailable: choice lasts for this page only */
  }
}
