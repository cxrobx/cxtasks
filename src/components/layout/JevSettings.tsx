import { useEffect, useState } from "react";
import { Loader2 } from "lucide-react";
import { api } from "@/lib/tauri";
import type { CaptureEngine, JevSettings, JevStatus } from "@/types/task";

const EMPTY: JevSettings = {
  enabled: false,
  model: "jev-1.13.0",
  capture: true,
  readiness: true,
  review: true,
  recall: true,
  capture_engine: "decisions",
};

export function JevSettingsPanel() {
  const [status, setStatus] = useState<JevStatus | null>(null);
  const [draft, setDraft] = useState<JevSettings>(EMPTY);
  const [key, setKey] = useState("");
  const [keyDirty, setKeyDirty] = useState(false);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    api.jev.status().then((next) => {
      if (!live) return;
      setStatus(next);
      setDraft(next.settings);
    }).catch((e) => live && setMessage(String(e)));
    return () => { live = false; };
  }, []);

  const save = async () => {
    setBusy(true);
    setMessage(null);
    try {
      const next = await api.jev.save({
        ...draft,
        ...(keyDirty ? { api_key: key } : {}),
      });
      setStatus(next);
      setDraft(next.settings);
      setKey("");
      setKeyDirty(false);
      setMessage("Saved locally.");
    } catch (e) {
      setMessage(String(e));
    } finally {
      setBusy(false);
    }
  };

  const toggle = (field: keyof Pick<JevSettings, "capture" | "readiness" | "review" | "recall">, label: string) => (
    <label className="flex items-center justify-between gap-3 py-1 text-[11.5px] text-content-secondary">
      <span>{label}</span>
      <input
        type="checkbox"
        checked={draft[field]}
        onChange={(e) => setDraft((s) => ({ ...s, [field]: e.target.checked }))}
        className="accent-[var(--accent)]"
      />
    </label>
  );

  return (
    <div className="space-y-2.5">
      <label className="flex items-center justify-between gap-3 text-[12px] font-medium text-content">
        <span>Use Jev decisions</span>
        <input
          type="checkbox"
          checked={draft.enabled}
          onChange={(e) => setDraft((s) => ({ ...s, enabled: e.target.checked }))}
          className="accent-[var(--accent)]"
        />
      </label>
      <p className="text-[11px] leading-snug text-content-faint">
        Sends the relevant task text to TypeSafe, or for capture to OpenAI. Rules, dates, permissions, and routing stay local.
      </p>
      <div className="grid grid-cols-2 gap-x-4">
        {toggle("capture", "Capture suggestions")}
        {toggle("readiness", "Pre-dispatch check")}
        {toggle("review", "Post-run review")}
        {toggle("recall", "Search reranking")}
      </div>
      <label className="flex items-center justify-between gap-3 text-[11px] text-content-faint">
        <span>Capture engine</span>
        <span className="flex gap-1">
          {(["decisions", "jev"] as CaptureEngine[]).map((engine) => (
            <button
              key={engine}
              type="button"
              aria-pressed={draft.capture_engine === engine}
              onClick={() => setDraft((s) => ({ ...s, capture_engine: engine }))}
              className={`rounded-md border px-2 py-0.5 text-[11px] ${draft.capture_engine === engine ? "border-accent text-content" : "border-border text-content-faint"}`}
            >
              {engine === "decisions" ? "OpenAI Decisions" : "Jev"}
            </button>
          ))}
        </span>
      </label>
      {draft.capture_engine === "decisions" && (
        <p className="text-[11px] leading-snug text-content-faint">
          {status?.decisions_configured
            ? `OpenAI key via ${status.decisions_credential_source}. Falls back to Jev, then to local rules.`
            : "No OPENAI_API_KEY in the environment or login Keychain — capture will use Jev."}
        </p>
      )}
      <label className="block text-[11px] text-content-faint">
        Jev model
        <input
          value={draft.model}
          onChange={(e) => setDraft((s) => ({ ...s, model: e.target.value }))}
          className="mt-1 w-full rounded-md border border-border bg-input px-2 py-1 text-[12px] text-content"
        />
      </label>
      <label className="block text-[11px] text-content-faint">
        TypeSafe API key
        <input
          type="password"
          value={key}
          placeholder={status?.configured ? `Configured via ${status.credential_source}` : "Paste a key"}
          onChange={(e) => { setKey(e.target.value); setKeyDirty(true); }}
          className="mt-1 w-full rounded-md border border-border bg-input px-2 py-1 text-[12px] text-content"
          autoComplete="off"
        />
      </label>
      <div className="flex items-center gap-2">
        <button
          type="button"
          disabled={busy}
          onClick={() => void save()}
          className="rounded-md bg-accent px-2.5 py-1 text-[11.5px] font-medium text-white disabled:opacity-50"
        >
          {busy ? <Loader2 size={12} className="animate-spin" /> : "Save Jev settings"}
        </button>
        {status?.credential_source === "local file" && (
          <button
            type="button"
            onClick={() => { setKey(""); setKeyDirty(true); setMessage("Save to remove the local key."); }}
            className="text-[11px] text-content-faint hover:text-content"
          >
            Remove key
          </button>
        )}
        {message && <span className="text-[11px] text-content-faint">{message}</span>}
      </div>
    </div>
  );
}
