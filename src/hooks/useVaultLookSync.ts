import { useEffect } from "react";
import { api } from "@/lib/tauri";
import { parseVaultPalette } from "@/lib/vaultLook";
import { ONYX_URL, useUiStore } from "@/stores/uiStore";

/** Onyx itself re-measures the vault on every Obsidian theme change and every
 * 30 s; cxmail and Meeting Copilot ask on the same minute. */
const POLL_MS = 60_000;

/**
 * Keep the vault palette current while anyone is looking at it — cxmail's hook
 * of the same name, ported.
 *
 * Asks Onyx now, whenever the window regains focus (the moment someone who just
 * changed their Obsidian theme comes back to look), and once a minute — but
 * only while the theme is `vault` or Settings is open. Otherwise CXTasks never
 * talks to Onyx at all, so a machine without it sees no traffic and no option
 * it cannot use.
 *
 * Every answer goes through `parseVaultPalette` and `setVaultLook`, which keeps
 * the last good palette when Onyx does not answer and repaints only when the
 * palette actually changed.
 */
export function useVaultLookSync(): void {
  const theme = useUiStore((s) => s.theme);
  const settingsOpen = useUiStore((s) => s.settingsOpen);
  const active = theme === "vault" || settingsOpen;

  useEffect(() => {
    if (!active) return;
    let cancelled = false;
    const check = () => {
      let pending: Promise<unknown>;
      try {
        pending = api.vaultLook.fetch(ONYX_URL);
      } catch {
        return; // No Tauri bridge (plain `vite dev`, tests). Nothing to ask.
      }
      pending
        .then((result) => {
          if (!cancelled) useUiStore.getState().setVaultLook(parseVaultPalette(result));
        })
        .catch(() => {
          // Only a non-loopback address rejects, and the address is a constant
          // here — so this is "Onyx did not answer", which keeps the palette.
          if (!cancelled) useUiStore.getState().setVaultLook(null);
        });
    };
    check();
    const timer = window.setInterval(check, POLL_MS);
    window.addEventListener("focus", check);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
      window.removeEventListener("focus", check);
    };
  }, [active]);
}
