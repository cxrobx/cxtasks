import { useCallback, useEffect, useRef } from "react";

type PasteEvent = Pick<ClipboardEvent, "clipboardData" | "preventDefault" | "stopPropagation">;
type PasteKeyEvent = Pick<KeyboardEvent, "key" | "metaKey" | "ctrlKey" | "altKey" | "shiftKey" | "repeat" | "stopPropagation">;

/** Native fallback for WKWebView, which can omit both image items and the paste event. */
export function useImagePaste(onImage?: (quiet?: boolean) => void | Promise<unknown>) {
  const callback = useRef(onImage);
  callback.current = onImage;
  const pending = useRef(false);
  const keyboardPaste = useRef<{ handled: boolean } | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);

  useEffect(() => () => clearTimeout(timer.current), []);

  const paste = useCallback((quiet: boolean) => {
    if (!callback.current || pending.current) return;
    pending.current = true;
    void Promise.resolve(callback.current(quiet)).finally(() => { pending.current = false; });
  }, []);

  const onPaste = useCallback((event: PasteEvent) => {
    if (!callback.current) return;
    event.stopPropagation();
    if (keyboardPaste.current) keyboardPaste.current.handled = true;
    const items = Array.from(event.clipboardData?.items ?? []);
    if (items.some((item) => item.type.startsWith("image/"))) {
      event.preventDefault();
      paste(false);
    } else if (!items.some((item) => item.type.startsWith("text/")) &&
      !event.clipboardData?.getData?.("text/plain")) {
      // Empty or Files-only paste data still commonly carries a native screenshot.
      paste(true);
    }
  }, [paste]);

  const onKeyDown = useCallback((event: PasteKeyEvent) => {
    if (!callback.current || !(event.metaKey || event.ctrlKey) || event.altKey || event.shiftKey ||
      event.repeat || event.key.toLowerCase() !== "v") return;
    event.stopPropagation();
    const attempt = { handled: false };
    keyboardPaste.current = attempt;
    clearTimeout(timer.current);
    // Let the normal paste event handle text, preserving native editing and undo.
    timer.current = setTimeout(() => {
      if (!attempt.handled) paste(true);
      if (keyboardPaste.current === attempt) keyboardPaste.current = null;
    }, 0);
  }, [paste]);

  return { onPaste, onKeyDown };
}
