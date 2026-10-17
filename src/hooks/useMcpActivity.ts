import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "@/lib/tauri";
import { useTaskStore } from "@/stores/taskStore";
import { useUiStore } from "@/stores/uiStore";
import type { McpActivity } from "@/types/task";

/**
 * Live refresh from the `cxtasks-mcp` / runner processes.
 *
 * They share only the SQLite DB with this app, so a task filed by `/task` in a
 * terminal would otherwise not appear until a manual reload. `mcp/bridge.rs`
 * writes one JSON envelope per mutation to a Unix socket; `lib.rs` re-emits it
 * as the `mcp-activity` Tauri event. The payload is a **hint to refetch**, never
 * content — we always re-read from the DB rather than trusting the envelope.
 */
export function useMcpActivity() {
  useEffect(() => {
    let disposed = false;

    const unlistenActivity = listen<McpActivity>("mcp-activity", (event) => {
      if (disposed) return;
      if (
        event.payload.kind === "run-started" ||
        event.payload.kind === "run-progress" ||
        event.payload.kind === "run-finished"
      ) {
        useTaskStore.setState((state) => ({
          runActivity: {
            key: (state.runActivity?.key ?? 0) + 1,
            kind: event.payload.kind as
              | "run-started"
              | "run-progress"
              | "run-finished",
            taskId: event.payload.task_id,
            runId: event.payload.run_id ?? null,
          },
        }));
        // Progress rows are already committed and consumed through a narrow
        // run-event query. A full task refresh here would refetch the board and
        // flash the same task once per second for no user-visible benefit.
        if (event.payload.kind === "run-progress") return;
      }
      const store = useTaskStore.getState();
      void store.refresh().then(() => {
        if (event.payload.task_id) store.flash(event.payload.task_id);
      });
    });

    // The reminder scan fires this after it marks tasks reminded, so a due
    // task's row updates without waiting for the next user action.
    const unlistenChanged = listen("tasks-changed", () => {
      if (disposed) return;
      void useTaskStore.getState().refresh();
    });

    /**
     * Collect whichever task a notification click asked for and show it.
     *
     * `reveal`, not `select`: the task is very often not in the current view —
     * a finished background run lands in `in_review` — and selecting a row the
     * view never loaded leaves the detail pane blank, which is what made
     * clicking a banner look like it did nothing.
     */
    const drainPending = async () => {
      if (disposed) return;
      const taskId = await api.notifications.takePending().catch(() => null);
      if (!taskId || disposed) return;
      useUiStore.getState().setSettingsOpen(false);
      await useTaskStore.getState().reveal(taskId);
    };

    // The event is a bare ping; the id comes from the latch (see
    // `notify_macos::PENDING_TASK`). This is the fast path for a click while
    // the app is already running.
    const unlistenNotification = listen("open-task-from-notification", () => {
      void drainPending();
    });

    // ...and this is the cold-launch path. Clicking a banner while CXTasks is
    // CLOSED launches it, and Rust emits from `.setup()` — before this hook has
    // mounted and there is anything to hear it. Without this drain that click,
    // the one where the app wasn't already in front of you, is simply lost.
    void drainPending();

    return () => {
      disposed = true;
      for (const p of [
        unlistenActivity,
        unlistenChanged,
        unlistenNotification,
      ]) {
        void p.then((unlisten) => unlisten());
      }
    };
  }, []);
}
