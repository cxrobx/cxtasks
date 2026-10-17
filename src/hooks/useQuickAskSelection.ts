import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTaskStore } from "@/stores/taskStore";

/** Publish only selection coordinates. Rust resolves labels from its database.
 * Writes are serialized so a slower earlier invoke cannot replace a new selection. */
export function useQuickAskSelection() {
  useEffect(() => {
    let stopped = false;
    let previous = "";
    let queue: Promise<unknown> = Promise.resolve();
    const publish = () => {
      if (stopped) return;
      const state = useTaskStore.getState();
      const selection = {
        task_id: state.checkedIds.length > 1 ? null : state.selectedId,
        project: state.activeProject,
        view: state.view,
      };
      const key = JSON.stringify(selection);
      if (key === previous) return;
      previous = key;
      queue = queue.then(() => stopped ? undefined : invoke("publish_quickask_selection", { selection })).catch(() => { previous = ""; });
    };
    publish();
    const unsubscribe = useTaskStore.subscribe(publish);
    const interval = setInterval(publish, 2000); // retry unavailable startup IPC, no screen capture
    return () => { stopped = true; unsubscribe(); clearInterval(interval); };
  }, []);
}
