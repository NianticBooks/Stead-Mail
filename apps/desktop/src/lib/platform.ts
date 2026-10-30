/** The modifier key shown in shortcut hints: Cmd on macOS, Ctrl elsewhere. Handlers accept both. */
export const isMac = typeof navigator !== "undefined" && /Mac/i.test(navigator.platform);
export const modKey = isMac ? "⌘" : "Ctrl";
