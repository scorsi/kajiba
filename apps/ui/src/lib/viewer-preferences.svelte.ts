/** Browser-local display preferences, isolated by signed-in viewer. */
export const preferences = $state({ hideFixedFailures: true });
let key = "";

export function loadPreferences(viewer: string) {
  key = `kajiba-viewer:${viewer}:hide-fixed-failures`;
  try {
    preferences.hideFixedFailures = localStorage.getItem(key) !== "false";
  } catch {
    preferences.hideFixedFailures = true;
  }
}

export function setHideFixedFailures(hide: boolean) {
  preferences.hideFixedFailures = hide;
  try {
    localStorage.setItem(key, String(hide));
  } catch {
    /* storage unavailable: choice lasts for this page only */
  }
}
