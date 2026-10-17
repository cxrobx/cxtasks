import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { Moon, PanelRight, Radar, Sun } from "lucide-react";
import { FilterStrip } from "@/components/layout/FilterStrip";
import { SearchBox } from "@/components/layout/SearchBox";
import { Sidebar } from "@/components/layout/Sidebar";
import { GsdBoard } from "@/components/gsd/GsdBoard";
import { MissionControl } from "@/components/mission/MissionControl";
import { TaskList } from "@/components/tasks/TaskList";
import { TaskDetail } from "@/components/tasks/TaskDetail";
import { QuickAdd } from "@/components/tasks/QuickAdd";
import { UndoToast } from "@/components/shared/UndoToast";
import { SettingsDialog } from "@/components/layout/SettingsDialog";
import { useKeyboardShortcuts } from "@/hooks/useKeyboardShortcuts";
import { useMcpActivity } from "@/hooks/useMcpActivity";
import { useVaultLookSync } from "@/hooks/useVaultLookSync";
import { cn } from "@/lib/utils";
import { useTaskStore } from "@/stores/taskStore";
import { resolveTheme, useUiStore } from "@/stores/uiStore";

/**
 * Detail pane width when open. The pane gets a little more room at the normal
 * desktop window size, but gives it back before the task list becomes unusably
 * narrow. Keeping this as one CSS value for both the grid track and its clipped
 * inner pane prevents reflow during the collapse animation.
 */
const DETAIL_WIDTH = "clamp(340px, 30vw, 420px)";

export function AppLayout() {
  const refresh = useTaskStore((s) => s.refresh);
  const loadRepos = useTaskStore((s) => s.loadRepos);
  const error = useTaskStore((s) => s.error);
  const tasks = useTaskStore((s) => s.tasks);
  const board = useTaskStore((s) => s.board);
  const view = useTaskStore((s) => s.view);
  const activeProject = useTaskStore((s) => s.activeProject);
  const collapsed = useUiStore((s) => s.detailCollapsed);
  const toggleDetail = useUiStore((s) => s.toggleDetail);
  const mission = useUiStore((s) => s.missionControl);
  const toggleMission = useUiStore((s) => s.toggleMissionControl);
  // The PAINTED theme, not the preference: under `vault` the icon must follow
  // the palette's own mode. Subscribing to `vaultLook` is what re-renders this
  // when Obsidian flips and the preference does not change.
  const theme = useUiStore((s) => resolveTheme(s.theme, s.vaultLook));
  const toggleTheme = useUiStore((s) => s.toggleTheme);

  const quickAddRef = useRef<HTMLTextAreaElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  useKeyboardShortcuts(quickAddRef, searchRef);
  useMcpActivity();
  // The Obsidian vault's palette, via Onyx — asked only while the theme is
  // `vault` or Settings is open. A change repaints through `setVaultLook`.
  useVaultLookSync();

  useEffect(() => {
    void refresh();
    void loadRepos();
  }, [refresh, loadRepos]);

  useEffect(() => {
    const unlisten = listen<string>("dock-action", (event) => {
      switch (event.payload) {
        case "quick-add":
          useUiStore.getState().setSettingsOpen(false);
          if (quickAddRef.current) requestAnimationFrame(() => quickAddRef.current?.focus());
          else void useTaskStore.getState().setView("today").then(() => {
            requestAnimationFrame(() => quickAddRef.current?.focus());
          });
          break;
        case "settings":
          useUiStore.getState().setSettingsOpen(true);
          break;
        case "open":
          break;
        case "today":
        case "inbox":
          useUiStore.getState().setSettingsOpen(false);
          void useTaskStore.getState().setView(event.payload);
          break;
      }
    });
    return () => { void unlisten.then((stop) => stop()); };
  }, []);

  // The GSD view renders projects, not tasks — but only at its top level.
  // Clicking a project name from the board drills into that project's task
  // list, which is a normal list and needs the normal chrome.
  const showingBoard = view === "gsd" && !activeProject;
  const heading = activeProject ?? VIEW_TITLES[view];

  // NO background on the root element below. It used to carry `bg-base`, which
  // quietly capped how transparent any pane could be: a full-window opaque
  // layer sat under everything, so the sidebar's own alpha composited over THAT
  // rather than over the vibrancy behind the window. The sidebar could then
  // only ever get *darker* than the content by adding more of its colour on
  // top — turning its alpha down moved it toward the content instead of toward
  // the desktop, so "more transparent" rendered as "less contrast", which reads
  // as the setting doing nothing at all. Each pane paints its own background
  // now, and the gaps between them are meant to be the blurred desktop.
  return (
    <div className="flex h-full flex-col text-content">
      {error && (
        // Opaque-backed: an error is the one thing that must stay readable
        // whatever the wallpaper is doing behind it.
        <div role="alert" className="bg-elevated px-3 py-1.5 text-error">
          {error}
        </div>
      )}

      {/* CRITICAL: every grid child carries `min-h-0`. Grid and flex items
          default to `min-height: auto`, which refuses to shrink below content
          height — so a tall pane overflows the row and the `overflow-y-auto`
          inside it never becomes scrollable. Symptom is content cropped at the
          window's bottom edge with no scrollbar. */}
      {/* The last track is `auto` so it follows the collapsing detail pane's own
          width. That's deliberate: `width` interpolates everywhere, while
          animating `grid-template-columns` directly depends on WebKit's
          grid-track interpolation and would snap where it isn't supported. */}
      <div className="grid min-h-0 flex-1 grid-cols-[200px_minmax(260px,1fr)_auto]">
        {/* No border here — it lives on `Sidebar`'s own `<nav>`, beside the
            `bg-sidebar` it has to composite over. `--border-subtle` is
            `rgb(255 255 255 / 0.05)`, so a border on THIS element (which paints
            nothing) is 5% white over raw blurred desktop: a 1px window straight
            through the app, brighter than either pane it divides. An element's
            background paints under its own border and no further, so the two
            have to sit together. */}
        <div className="min-h-0 overflow-hidden">
          <Sidebar />
        </div>

        {/* `bg-base` lives HERE, not on the root — see the note there. This is
            the content pane, and it is the only thing that should be painting
            the content colour. */}
        <main className="flex min-h-0 min-w-0 flex-col bg-base">
          {/* `deep`, not a bare attribute: bare means "only a DIRECT click on
              this exact element drags" (drag.js compares against
              `composedPath[0]`), which would leave the heading and the task
              count as dead spots. Buttons and inputs inside still block the
              drag — Tauri's `isClickableElement` excludes them automatically,
              so they need no opt-out of their own. */}
          <header
            data-tauri-drag-region="deep"
            className="flex shrink-0 items-center gap-3 border-b border-border-subtle px-3 pb-2"
            style={{ paddingTop: "var(--titlebar-height)" }}
          >
            {/* In Mission Control the secondary slot carries the SCOPE rather
                than a count — the dashboard's "Your queue" is the selected
                view's tasks, and without saying so the panel reads as if it
                were showing everything. */}
            <h1 className="shrink-0 text-[15px] font-semibold">
              {mission ? "Mission Control" : heading}
            </h1>
            <span className="shrink-0 tabular-nums text-content-faint">
              {mission ? heading : showingBoard ? board.length : tasks.length}
            </span>
            <SearchBox searchRef={searchRef} />

            {/* A lens, not a destination — so it's a toolbar toggle next to the
                other two, not a sidebar entry. Its active state is filled
                rather than merely tinted: the pane it swaps in is unlike
                anything else in the app, and "why is my list gone" should be
                answerable from the chrome. */}
            <button
              type="button"
              aria-label={mission ? "Hide Mission Control" : "Show Mission Control"}
              aria-pressed={mission}
              title="Mission Control — live agent runs and spend"
              onClick={toggleMission}
              className={cn(
                "shrink-0 rounded-md p-1 transition-colors duration-75",
                mission
                  ? "bg-accent/15 text-accent"
                  : "text-content-faint hover:bg-content/10 hover:text-content"
              )}
            >
              <Radar size={14} />
            </button>

            {/* Shows the theme you'd get by clicking, not the one you're in —
                a sun while dark reads as "go light". The icon is the whole
                affordance, so the label has to carry the direction too.

                Stays in the header rather than moving into the settings sheet
                with the other appearance controls: this one gets toggled, not
                configured, and a switch you flip through the day does not
                belong two clicks deep. Transparency is the opposite — set once
                and lived with — which is what the gear beside it is for. */}
            <button
              type="button"
              aria-label={theme === "dark" ? "Switch to light mode" : "Switch to dark mode"}
              title={theme === "dark" ? "Light mode" : "Dark mode"}
              onClick={toggleTheme}
              className={cn(
                "shrink-0 rounded-md p-1 transition-colors duration-75",
                "text-content-faint hover:bg-content/10 hover:text-content"
              )}
            >
              {theme === "dark" ? <Sun size={14} /> : <Moon size={14} />}
            </button>

            {/* Sits at the far right of the header, directly above the pane it
                opens and closes, and stays put in both states — a close button
                inside the pane would vanish along with the way back. */}
            <button
              type="button"
              aria-label={collapsed ? "Show details" : "Hide details"}
              aria-expanded={!collapsed}
              title={`${collapsed ? "Show" : "Hide"} details (⌘\\)`}
              onClick={toggleDetail}
              className={cn(
                "-mr-1 shrink-0 rounded-md p-1 transition-colors duration-75",
                "hover:bg-content/10",
                collapsed
                  ? "text-content-secondary"
                  : "text-content-faint hover:text-content"
              )}
            >
              <PanelRight size={14} />
            </button>
          </header>

          {/* Everything the list is applying, said out loud (invariant 21).
              Not on the board, which renders projects rather than tasks, and
              not under Mission Control, whose panel is not this list. */}
          {!showingBoard && !mission && <FilterStrip />}

          {/* No quick-add on the board: there is no task list to add to, and a
              capture field that files into whatever project happens to be
              selected would be a trap. */}
          {!showingBoard && (
            <div className="shrink-0">
              <QuickAdd ref={quickAddRef} />
            </div>
          )}

          <div className="min-h-0 flex-1">
            {mission ? (
              <MissionControl />
            ) : showingBoard ? (
              <GsdBoard />
            ) : (
              <TaskList />
            )}
          </div>
        </main>

        {/* The pane keeps its full width inside a clipping wrapper, so its
            fields slide out intact instead of reflowing as it narrows. The
            border goes with it — a 1px rule on a 0-width pane would otherwise
            stay behind as a stray line. */}
        <div
          className={cn(
            // `bg-surface` sits HERE, on the element carrying the border, for the
            // reason the sidebar note above gives — an alpha border over no
            // background is a hairline of desktop. It moved up from the inner
            // column rather than the border moving down, because the border is
            // conditional on `collapsed` (a 1px rule on a 0-width pane would
            // stay behind as a stray line) and that logic is better left alone.
            // This element still contains the drag headroom, so it keeps
            // covering both of `TaskDetail`'s branches.
            "min-h-0 min-w-0 overflow-hidden bg-surface transition-[width] duration-200 ease-out",
            collapsed ? "border-l-0" : "border-l border-border-subtle"
          )}
          style={{ width: collapsed ? 0 : DETAIL_WIDTH }}
        >
          {/* The traffic-light headroom lives here rather than inside
              `TaskDetail` so it covers both of that component's branches (form
              and the "No task selected" placeholder) and, critically, sits
              OUTSIDE its scroller — a drag region inside `overflow-y-auto`
              scrolls away, leaving the pane draggable only at scroll-top. */}
          {/* `bg-surface` belongs HERE, on the column that holds both the
              headroom and the pane — not on `TaskDetail`'s scroller, where it
              used to be. There it was a SIBLING of the drag strip rather than
              its parent, so the strip painted nothing and the top ~28px of the
              pane was raw blurred desktop with the traffic lights over it.
              The empty state was the same bug, larger: the "No task selected"
              branch carries no background either, so with nothing selected the
              whole pane went transparent.

              Same shape as `Sidebar`, whose `bg-sidebar` sits on the `<nav>`
              that contains its headroom — and the same reason the headroom
              lives out here at all (see below): a thing that must cover both
              of `TaskDetail`'s branches cannot live inside either one.

              It must MOVE rather than be added in both places. Two layers of
              the same alpha composite twice, which at Glass would make the
              detail pane quietly denser than the surface token asks for —
              gotcha #22's "no pane may sit on another pane's background". */}
          <div
            className="flex h-full min-h-0 flex-col"
            style={{ width: DETAIL_WIDTH }}
          >
            <div
              data-tauri-drag-region="deep"
              className="shrink-0"
              style={{ height: "var(--titlebar-height)" }}
            />
            <div className="min-h-0 flex-1">
              <TaskDetail />
            </div>
          </div>
        </div>
      </div>

      {/* Fixed-position overlay, so it sits outside the grid rather than
          competing with it for space. */}
      <UndoToast />
      <SettingsDialog />
    </div>
  );
}

const VIEW_TITLES: Record<string, string> = {
  gsd: "GSD",
  inbox: "Inbox",
  today: "Today",
  waiting: "Waiting",
  in_review: "In Review",
  open: "Open",
  // Kept although no sidebar row selects it anymore — `listView` still maps
  // the GSD drill-down onto "all", and a title has to exist for that state.
  all: "All Open",
  done: "Done",
  trash: "Trash",
  archive: "Archive",
};
