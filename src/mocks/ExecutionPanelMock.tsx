import { TaskDetail } from "@/components/tasks/TaskDetail";
import { useTaskStore } from "@/stores/taskStore";
import type { Task } from "@/types/task";

// An exact production TaskDetail inside the exact responsive pane track. This
// is intentionally not a second hand-built picture of the console: it catches
// flex shrinking, internal scroll, and container-query bugs in the component
// that ships.
const TASK: Task = {
  id: "execution-panel-mock",
  title: "Ship a dynamic launch console",
  body: "Make unattended agent work feel deliberate, bounded, and easy to inspect before launch.",
  status: "todo",
  blocked_reason: null,
  priority: 1,
  project: "CXTasks",
  due_at: null,
  defer_until: null,
  recurrence: null,
  parent_id: null,
  sort_order: 0,
  created_at: "2026-08-01 00:00:00",
  updated_at: "2026-08-01 00:00:00",
  completed_at: null,
  repo_path: "/Users/me/Projects/cxtasks",
  prompt: "Verify the launch console at every supported panel size.",
  bg_allowed: false,
  bg_executor: "claude",
  bg_intent: "work",
  bg_writes_allowed: false,
  bg_shell_allowed: true,
  bg_remote_allowed: false,
  bg_mcp_servers: null,
  reminded_at: null,
  model: null,
  section_id: null,
  deleted_at: null,
  archived_at: null,
  short_id: 42,
  tripwire: null,
  tripwire_every_minutes: null,
  tripwire_on_trip: null,
  tripwire_state: null,
  tripwire_tripped_at: null,
};

useTaskStore.setState({
  tasks: [TASK],
  selectedId: TASK.id,
  subtasks: [],
  runs: [],
  repos: [{ name: "cxtasks", path: "/Users/me/Projects/cxtasks" }],
  sections: [],
  tags: [],
  comments: [],
  attachments: [],
  relations: {
    blocked_by: [],
    blocking: [],
    is_blocked: false,
    supersedes: [],
    superseded_by: [],
    related: [],
    is_superseded: false,
  },
});

export default function ExecutionPanelMock() {
  return (
    <main className="flex h-full min-h-0 justify-end overflow-hidden bg-base text-content">
      <div className="min-w-0 flex-1 bg-sidebar/60" />
      <aside
        aria-label="Production detail panel preview"
        className="flex h-full min-h-0 flex-col border-l border-border-subtle bg-surface"
        style={{ width: "clamp(340px, 30vw, 420px)" }}
      >
        <div className="flex h-[38px] shrink-0 items-center border-b border-border-subtle px-4 text-[10px] uppercase tracking-wider text-content-faint">
          Exact production pane · resize the window
        </div>
        <div className="min-h-0 flex-1">
          <TaskDetail />
        </div>
      </aside>
    </main>
  );
}
