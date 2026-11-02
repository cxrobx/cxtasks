import { useEffect, useState } from "react";
import { Loader2 } from "lucide-react";
import { api } from "@/lib/tauri";

/** Where Open in Claude starts its session. Saved per Mac and never synced, so
 *  one Mac can point at the other while that one stays on its own Herdr. */
export function HerdrHostSetting() {
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    api.dispatch
      .herdrHost()
      // A slow read must not wipe what was typed while it was in flight.
      .then((host) => live && setDraft((typed) => typed || (host ?? "")))
      .catch((e) => live && setMessage(String(e)));
    return () => {
      live = false;
    };
  }, []);

  const save = async () => {
    setBusy(true);
    setMessage(null);
    try {
      const host = await api.dispatch.saveHerdrHost(draft);
      setDraft(host ?? "");
      setMessage(host ? "Saved on this Mac." : "Using this Mac's Herdr.");
    } catch (e) {
      setMessage(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="space-y-2">
      <label className="block text-[11px] text-content-faint">
        Herdr host
        <input
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          placeholder="This Mac"
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
          autoComplete="off"
          className="mt-1 w-full rounded-md border border-border bg-input px-2 py-1 text-[12px] text-content"
        />
      </label>
      <p className="text-[11px] leading-snug text-content-faint">
        Leave blank for this Mac, or enter an SSH target such as user@host to open
        Claude in that Mac’s Herdr.
      </p>
      <div className="flex items-center gap-2">
        <button
          type="button"
          disabled={busy}
          onClick={() => void save()}
          className="shrink-0 whitespace-nowrap rounded-md bg-accent px-2.5 py-1 text-[11.5px] font-medium text-white disabled:opacity-50"
        >
          {busy ? <Loader2 size={12} className="animate-spin" /> : "Save Herdr host"}
        </button>
        {message && <span className="text-[11px] text-content-faint">{message}</span>}
      </div>
    </div>
  );
}
