import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { applyDensity, applyTheme, readDensity, readTheme, startGlass } from "@/stores/uiStore";
import "./styles/globals.css";

// Apply the saved theme BEFORE the first render, not from an effect. An effect
// runs after the initial paint, so a user on light would see one dark frame at
// every launch. This also makes the one native `set_native_appearance` call that
// corrects the window's dark boot default (see `lib.rs`'s setup comment).
// `applyTheme` also applies the saved transparency — the pane alphas depend on
// which theme is active, so the two are set together and there is no separate
// call to forget here.
applyTheme(readTheme());
// Before the first paint for the same reason: the attribute drives row
// padding, so applying it later makes the list re-flow on every launch.
applyDensity(readDensity());

// WKWebView's default context menu is the developer one ("Reload", "Inspect
// Element") — useless in a shipped app and jarring on right-click. Suppress it
// globally, but leave it alone over editable fields so the native
// cut/copy/paste/spelling menu still works there.
document.addEventListener("contextmenu", (e) => {
  const el = e.target as HTMLElement | null;
  const editable =
    el?.closest("input, textarea, [contenteditable='true']") != null;
  if (!editable) e.preventDefault();
});

// The window sets `dragDropEnabled: false` so that HTML5 drag-and-drop works at
// all inside the page: Tauri's native file-drop handler returns `true` for every
// drag, and wry then never forwards `draggingEntered`/`performDragOperation` to
// WKWebView — so with it on, no `dragover`/`drop` event ever reaches the DOM and
// every in-app drag is silently inert.
//
// Turning it off hands the OS drag back to WebKit, whose default for a file
// dragged in from Finder is to NAVIGATE to it — the app blanks and only comes
// back on relaunch. So the guard Tauri was providing moves here, scoped to file
// drags only: an internal drag (a task row, a project row) or text dropped into
// a field carries no "Files" type and is left completely alone.
//
// `document`-level and non-capturing on purpose — our own handlers sit on inner
// elements and have already run by the time this fires, and `preventDefault`
// cancels only the browser's default action, never a handler.
const isFileDrag = (e: DragEvent) =>
  Array.from(e.dataTransfer?.types ?? []).includes("Files");
document.addEventListener("dragover", (e) => {
  if (isFileDrag(e)) e.preventDefault();
});
document.addEventListener("drop", (e) => {
  if (isFileDrag(e)) e.preventDefault();
});

// `?mock` swaps in the design-exploration page (src/mocks/) — lazy, so the
// chunk never loads in a normal launch.
const RedesignMock = React.lazy(() => import("./mocks/RedesignMock"));
const ExecutionContextMock = React.lazy(
  () => import("./mocks/ExecutionContextMock")
);
const ExecutionPanelMock = React.lazy(
  () => import("./mocks/ExecutionPanelMock")
);
const LaunchConsoleMock = React.lazy(() => import("./mocks/LaunchConsoleMock"));
const IosNavMock = React.lazy(() => import("./mocks/IosNavMock"));
const DetailDisclosureMock = React.lazy(
  () => import("./mocks/DetailDisclosureMock")
);
const BelowNotesMock = React.lazy(() => import("./mocks/BelowNotesMock"));
const searchParams = new URLSearchParams(window.location.search);
const isMock = searchParams.has("mock");
const mockName = searchParams.get("mock");
const MockPage =
  mockName === "execution"
    ? ExecutionContextMock
    : mockName === "execution-panel"
      ? ExecutionPanelMock
      : mockName === "launch"
        ? LaunchConsoleMock
        : mockName === "ios-nav"
          ? IosNavMock
          : mockName === "disclosure"
            ? DetailDisclosureMock
            : mockName === "below-notes"
              ? BelowNotesMock
              : RedesignMock;

// The window boots OPAQUE (see `lib.rs`) and only becomes glass once there is
// something painted inside it — a transparent window wrapped around an empty
// webview is bare wallpaper with three traffic lights floating on it. Two
// frames: the first gets the render scheduled, the second lands after it has
// been composited. One frame fires early often enough to show the flash this
// exists to prevent.
requestAnimationFrame(() => requestAnimationFrame(startGlass));

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    {isMock ? (
      <React.Suspense fallback={null}>
        <MockPage />
      </React.Suspense>
    ) : (
      <App />
    )}
  </React.StrictMode>
);
