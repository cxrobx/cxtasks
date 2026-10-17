import { open } from "@tauri-apps/plugin-shell";

/** Schemes we will hand to the OS. Everything else is ignored, silently. */
const ALLOWED = new Set(["http:", "https:", "mailto:"]);

/**
 * Open a link from rendered markdown in the user's browser.
 *
 * Two guards, both because this text is agent-authored:
 *
 * 1. **Scheme allow-list.** `shell:allow-open` will hand almost anything to
 *    `open(1)`, which on macOS happily launches applications and local files.
 *    An unattended run that emits a `file://` link into its handoff would
 *    otherwise be one click from opening arbitrary local paths. Only the three
 *    schemes a prose link legitimately uses get through; a rejected link stays
 *    inert rather than erroring, since the visible text is still readable.
 * 2. **Never throws.** `invoke` throws *synchronously* when the Tauri IPC
 *    bridge isn't on `window` — any plain `vite dev` tab or jsdom test — and a
 *    click handler that throws is an unhandled rejection in the pane (see the
 *    `applyTheme` note in gotcha #8). Wrapped so a link is dead outside the
 *    app instead of noisy.
 */
export async function openUrl(href: string): Promise<void> {
  let scheme: string;
  try {
    scheme = new URL(href).protocol;
  } catch {
    return; // Relative or malformed — nothing sensible to open.
  }
  if (!ALLOWED.has(scheme)) return;
  try {
    await open(href);
  } catch {
    // No IPC bridge (browser/test), or the OS refused. Not worth surfacing.
  }
}
