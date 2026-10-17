import { useEffect, useState } from "react";
import {
  Bot,
  Check,
  ChevronDown,
  ChevronRight,
  Circle,
  Code2,
  Copy,
  Eye,
  FileText,
  FolderGit2,
  GitBranch,
  Lock,
  Moon,
  Play,
  Shield,
  Sparkles,
  Sun,
  Terminal,
  WandSparkles,
  Zap,
} from "lucide-react";
import { cn } from "@/lib/utils";

/* ────────────────────────────────────────────────────────────────
   Mock exploration page — execution-context configurator.
   Reached only via `?mock=execution`; all data and actions are static.
   ──────────────────────────────────────────────────────────────── */

type MockVariant = "A" | "B" | "C";
type Agent = "claude" | "codex";
type Access = "inspect" | "command" | "edit";

const REPO = "~/Projects/cxtasks";
const PROMPT =
  "Redesign the execution context so agent, access, and workspace feel like one clear launch decision.";

const ACCESS = {
  inspect: {
    label: "Inspect",
    short: "Read only",
    detail: "Search and understand the repo",
    icon: Eye,
  },
  command: {
    label: "Run",
    short: "Shell access",
    detail: "Inspect, run tests and use git",
    icon: Terminal,
  },
  edit: {
    label: "Build",
    short: "Workspace access",
    detail: "Run commands and edit files",
    icon: WandSparkles,
  },
} satisfies Record<Access, { label: string; short: string; detail: string; icon: typeof Eye }>;

function AgentMark({ agent, active = false }: { agent: Agent; active?: boolean }) {
  return (
    <span
      className={cn(
        "flex size-7 shrink-0 items-center justify-center rounded-lg border",
        active
          ? "border-accent/35 bg-accent/15 text-accent"
          : "border-border-subtle bg-content/5 text-content-muted"
      )}
    >
      {agent === "claude" ? <Sparkles size={14} /> : <Code2 size={14} />}
    </span>
  );
}

function Toggle({ enabled, onToggle }: { enabled: boolean; onToggle: () => void }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={enabled}
      aria-label="Allow background runs"
      onClick={onToggle}
      className={cn(
        "relative h-[18px] w-8 rounded-full border transition-colors",
        enabled ? "border-accent/40 bg-accent" : "border-border bg-content/8"
      )}
    >
      <span
        className={cn(
          "absolute top-[2px] size-3 rounded-full bg-white shadow-sm transition-transform",
          enabled ? "left-[16px]" : "left-[2px]"
        )}
      />
    </button>
  );
}

function RepoChip() {
  return (
    <div className="flex min-w-0 items-center gap-2 rounded-lg border border-border bg-input px-2.5 py-2">
      <FolderGit2 size={13} className="shrink-0 text-accent" />
      <span className="min-w-0 flex-1 truncate font-mono text-[11px] text-content-secondary">
        {REPO}
      </span>
      <span className="rounded bg-content/7 px-1.5 py-0.5 font-mono text-[9px] text-content-faint">
        main
      </span>
      <ChevronDown size={11} className="text-content-faint" />
    </div>
  );
}

function PromptBox({ compact = false }: { compact?: boolean }) {
  return (
    <div className="rounded-lg border border-border bg-input p-2.5">
      <div className="flex items-start gap-2">
        <FileText size={12} className="mt-0.5 shrink-0 text-content-faint" />
        <p
          className={cn(
            "flex-1 text-[11px] leading-relaxed text-content-secondary",
            compact && "line-clamp-2"
          )}
        >
          {PROMPT}
        </p>
        <button className="rounded p-0.5 text-content-faint hover:bg-content/8 hover:text-content">
          <WandSparkles size={11} />
        </button>
      </div>
      {!compact && (
        <div className="mt-2 flex items-center border-t border-border-subtle pt-1.5 text-[9.5px] text-content-faint">
          <span>Custom seed · 16 words</span>
          <span className="ml-auto">⌘↵ to launch</span>
        </div>
      )}
    </div>
  );
}

/* ── A — Guided stack ───────────────────────────────────────────
   Familiar form anatomy, but grouped into four deliberate decisions
   with a sticky readiness footer. Lowest-risk production direction. */
function MockA() {
  const [agent, setAgent] = useState<Agent>("claude");
  const [access, setAccess] = useState<Access>("inspect");
  const [background, setBackground] = useState(true);

  return (
    <PreviewFrame caption="A · Guided stack">
      <div className="overflow-hidden rounded-xl border border-border bg-surface shadow-macos">
        <div className="flex items-start gap-3 border-b border-border-subtle px-4 py-3.5">
          <div className="flex size-8 items-center justify-center rounded-lg bg-accent/12 text-accent">
            <Zap size={15} />
          </div>
          <div>
            <h2 className="text-[13px] font-semibold">Execution setup</h2>
            <p className="mt-0.5 text-[10.5px] text-content-muted">
              Choose where the task runs and what it can do.
            </p>
          </div>
          <span className="ml-auto flex items-center gap-1 rounded-full bg-success/10 px-2 py-1 text-[9.5px] font-medium text-success">
            <Check size={10} /> Ready
          </span>
        </div>

        <div className="space-y-4 p-4">
          <Decision index="01" label="Workspace" hint="Required for background runs">
            <RepoChip />
          </Decision>

          <Decision index="02" label="Brief" hint="Overrides title + notes">
            <PromptBox compact />
          </Decision>

          <Decision index="03" label="Agent" hint="Who should pick this up?">
            <div className="grid grid-cols-2 gap-2">
              {(["claude", "codex"] as const).map((item) => (
                <button
                  key={item}
                  onClick={() => setAgent(item)}
                  className={cn(
                    "flex items-center gap-2 rounded-lg border p-2 text-left transition-colors",
                    agent === item
                      ? "border-accent/40 bg-accent/10"
                      : "border-border-subtle bg-content/3 hover:bg-content/6"
                  )}
                >
                  <AgentMark agent={item} active={agent === item} />
                  <span>
                    <span className="block text-[11.5px] font-medium capitalize">
                      {item === "claude" ? "Claude Code" : "Codex"}
                    </span>
                    <span className="block text-[9.5px] text-content-faint">
                      {item === "claude" ? "Flexible tool grants" : "OS sandboxed"}
                    </span>
                  </span>
                  {agent === item && <Check size={11} className="ml-auto text-accent" />}
                </button>
              ))}
            </div>
          </Decision>

          <Decision index="04" label="Access" hint="Start narrow; widen when needed">
            <div className="grid grid-cols-3 gap-1 rounded-lg bg-input p-1">
              {(Object.keys(ACCESS) as Access[]).map((item) => {
                const Icon = ACCESS[item].icon;
                return (
                  <button
                    key={item}
                    onClick={() => setAccess(item)}
                    className={cn(
                      "rounded-md px-2 py-2 text-left transition-all",
                      access === item
                        ? "bg-surface text-content shadow-macos"
                        : "text-content-muted hover:text-content-secondary"
                    )}
                  >
                    <Icon size={12} className={access === item ? "text-accent" : undefined} />
                    <span className="mt-1.5 block text-[10.5px] font-medium">
                      {ACCESS[item].label}
                    </span>
                    <span className="mt-0.5 block text-[8.5px] leading-tight text-content-faint">
                      {ACCESS[item].short}
                    </span>
                  </button>
                );
              })}
            </div>
          </Decision>
        </div>

        <div className="flex items-center gap-3 border-t border-border-subtle bg-content/3 px-4 py-3">
          <Toggle enabled={background} onToggle={() => setBackground((v) => !v)} />
          <div>
            <div className="text-[10.5px] font-medium">Run while I’m away</div>
            <div className="text-[9px] text-content-faint">Queue this task for unattended work</div>
          </div>
          <button className="ml-auto flex items-center gap-1.5 rounded-lg bg-accent px-3 py-2 text-[11px] font-medium text-white shadow-sm hover:bg-accent-hover">
            <Play size={11} fill="currentColor" /> Open in Claude
          </button>
        </div>
      </div>
    </PreviewFrame>
  );
}

function Decision({
  index,
  label,
  hint,
  children,
}: {
  index: string;
  label: string;
  hint: string;
  children: React.ReactNode;
}) {
  return (
    <div>
      <div className="mb-1.5 flex items-center gap-2">
        <span className="font-mono text-[9px] text-accent">{index}</span>
        <span className="text-[10px] font-semibold uppercase tracking-[0.12em] text-content-muted">
          {label}
        </span>
        <span className="ml-auto text-[9px] text-content-faint">{hint}</span>
      </div>
      {children}
    </div>
  );
}

/* ── B — Launch console ─────────────────────────────────────────
   Turns the form into a small agent launchpad. The connected route
   makes repo → agent → access legible as one executable plan. */
function MockB() {
  const [agent, setAgent] = useState<Agent>("claude");
  const [access, setAccess] = useState<Access>("command");

  return (
    <PreviewFrame caption="B · Launch console">
      <div className="mx-auto w-full max-w-[348px]">
        <div
          data-testid="launch-console-preview"
          className="relative overflow-hidden rounded-xl border border-accent/20 bg-surface shadow-macos"
        >
          <div className="relative flex items-start gap-2.5 border-b border-border-subtle px-3.5 py-3">
            <span className="flex size-7 shrink-0 items-center justify-center rounded-lg bg-accent/12 text-accent">
              <Zap size={13} />
            </span>
            <div>
              <h2 className="text-[12px] font-semibold">Launch console</h2>
              <p className="mt-0.5 text-[9.5px] text-content-muted">
                Workspace → runner → capability
              </p>
            </div>
            <span className="ml-auto flex items-center gap-1 rounded-full bg-success/10 px-2 py-1 text-[9px] font-medium text-success">
              <span className="size-1.5 rounded-full bg-success" /> Ready
            </span>
          </div>

          <div className="space-y-3.5 p-3.5">
            <div className="relative px-1">
              <div className="absolute left-[16.5%] right-[16.5%] top-[21px] h-px bg-border" />
              <div className="mock-route absolute left-[16.5%] top-[21px] h-px bg-accent" />
              <div className="relative grid grid-cols-3 gap-1">
                <RouteNode
                  icon={FolderGit2}
                  eyebrow="Workspace"
                  title="cxtasks"
                  meta="main"
                  active
                />
                <RouteNode
                  icon={agent === "claude" ? Sparkles : Code2}
                  eyebrow="Runner"
                  title={agent === "claude" ? "Claude Code" : "Codex"}
                  meta="background"
                  active
                />
                <RouteNode
                  icon={ACCESS[access].icon}
                  eyebrow="Capability"
                  title={ACCESS[access].label}
                  meta={ACCESS[access].short}
                  active
                />
              </div>
            </div>

            <div>
              <div className="mb-1.5 flex items-center gap-1.5 text-[9.5px] font-semibold uppercase tracking-wider text-content-muted">
                <FolderGit2 size={10} /> Workspace
                <span className="ml-auto font-normal normal-case tracking-normal text-content-faint">required</span>
              </div>
              <RepoChip />
            </div>

            <div>
              <div className="mb-1.5 flex items-center gap-1.5 text-[9.5px] font-semibold uppercase tracking-wider text-content-muted">
                <Sparkles size={10} /> Runner
                <span className="ml-auto font-normal normal-case tracking-normal text-content-faint">background agent</span>
              </div>
              <div className="grid grid-cols-2 gap-1.5">
                {(["claude", "codex"] as const).map((item) => (
                  <button
                    key={item}
                    onClick={() => setAgent(item)}
                    className={cn(
                      "flex items-center gap-2 rounded-lg border p-2 text-left",
                      agent === item
                        ? "border-accent/40 bg-accent/10"
                        : "border-border-subtle bg-content/3"
                    )}
                  >
                    <AgentMark agent={item} active={agent === item} />
                    <span className="min-w-0">
                      <span className="block truncate text-[10px] font-medium">
                        {item === "claude" ? "Claude Code" : "Codex"}
                      </span>
                      <span className="block text-[8.5px] text-content-faint">
                        {item === "claude" ? "Tool grants" : "OS sandbox"}
                      </span>
                    </span>
                    {agent === item && <Check size={9} className="ml-auto text-accent" />}
                  </button>
                ))}
              </div>
            </div>

            <div>
              <div className="mb-1.5 flex items-center gap-1.5 text-[9.5px] font-semibold uppercase tracking-wider text-content-muted">
                <FileText size={10} /> Run brief
                <span className="ml-auto font-normal normal-case tracking-normal text-content-faint">custom seed</span>
              </div>
              <PromptBox compact />
            </div>

            <div>
              <div className="mb-1.5 flex items-center gap-1.5 text-[9.5px] font-semibold uppercase tracking-wider text-content-muted">
                <Shield size={10} /> Access envelope
                <span className="ml-auto font-normal normal-case tracking-normal text-content-faint">start narrow</span>
              </div>
              <div className="grid grid-cols-3 gap-1.5">
                {(Object.keys(ACCESS) as Access[]).map((item) => {
                  const Icon = ACCESS[item].icon;
                  const selected = access === item;
                  return (
                    <button
                      key={item}
                      onClick={() => setAccess(item)}
                      className={cn(
                        "relative rounded-lg border px-2 py-2 text-left",
                        selected
                          ? "border-accent/40 bg-accent/10"
                          : "border-border-subtle bg-content/3"
                      )}
                    >
                      <Icon size={12} className={selected ? "text-accent" : "text-content-faint"} />
                      <span className="mt-1.5 block text-[10px] font-medium">{ACCESS[item].label}</span>
                      <span className="mt-0.5 block text-[8.5px] text-content-faint">{ACCESS[item].short}</span>
                      {selected && <Check size={9} className="absolute right-2 top-2 text-accent" />}
                    </button>
                  );
                })}
              </div>
              <p className="mt-2 text-[9px] leading-relaxed text-content-faint">
                {ACCESS[access].detail}
              </p>
            </div>
          </div>

          <div className="relative border-t border-accent/15 bg-accent/7 px-3.5 py-3">
            <div className="mb-2 flex items-center gap-1.5 text-[9px] text-content-faint">
              <GitBranch size={10} className="text-accent" /> Snapshot on launch · changes can be rolled back
            </div>
            <div className="flex items-center gap-1.5">
              <button className="flex min-w-0 flex-1 items-center justify-center gap-1.5 whitespace-nowrap rounded-lg bg-accent px-2 py-2 text-[10px] font-semibold text-white">
                <Play size={11} fill="currentColor" /> Queue for {agent === "claude" ? "Claude" : "Codex"}
              </button>
              <button className="shrink-0 rounded-lg border border-border bg-surface px-2 py-2 text-[10px] text-content-secondary">
                Open in Claude
              </button>
              <button className="shrink-0 rounded-lg border border-border bg-surface p-2 text-content-muted">
                <Copy size={11} />
              </button>
            </div>
          </div>
        </div>
      </div>
    </PreviewFrame>
  );
}

function RouteNode({
  icon: Icon,
  eyebrow,
  title,
  meta,
  active,
}: {
  icon: typeof Eye;
  eyebrow: string;
  title: string;
  meta: string;
  active?: boolean;
}) {
  return (
    <div className="relative flex flex-col items-center text-center">
      <span
        className={cn(
          "z-10 flex size-[42px] items-center justify-center rounded-xl border shadow-macos",
          active
            ? "border-accent/40 bg-surface text-accent"
            : "border-border bg-input text-content-muted"
        )}
      >
        <Icon size={16} />
      </span>
      <span className="mt-2 text-[8px] font-semibold uppercase tracking-[0.14em] text-content-faint">
        {eyebrow}
      </span>
      <span className="mt-0.5 text-[11px] font-medium">{title}</span>
      <span className="text-[8.5px] text-content-faint">{meta}</span>
    </div>
  );
}

/* ── C — Command capsule ────────────────────────────────────────
   Progressive disclosure: the entire configuration reads as one
   command. Click a token to open only the editor relevant to it. */
function MockC() {
  const [active, setActive] = useState<"repo" | "agent" | "access" | "brief" | null>(
    "access"
  );
  const [agent, setAgent] = useState<Agent>("claude");
  const [access, setAccess] = useState<Access>("inspect");

  return (
    <PreviewFrame caption="C · Command capsule">
      <div className="rounded-2xl border border-border bg-surface p-4 shadow-macos">
        <div className="flex items-center gap-2">
          <span
            className="flex size-8 items-center justify-center rounded-xl bg-content"
            style={{ color: "rgb(var(--bg-primary))" }}
          >
            <Terminal size={14} />
          </span>
          <div>
            <h2 className="text-[13px] font-semibold">Compose a run</h2>
            <p className="text-[9.5px] text-content-faint">Click any token to change the plan</p>
          </div>
          <span className="ml-auto flex items-center gap-1 text-[9.5px] text-success">
            <span className="size-1.5 rounded-full bg-success" /> valid configuration
          </span>
        </div>

        <div className="mt-4 flex flex-wrap items-center gap-1.5 rounded-xl border border-border bg-base/70 p-2.5 font-mono text-[11px]">
          <span className="select-none text-content-faint">$</span>
          <span className="select-none font-semibold text-content">dispatch</span>
          <CommandToken
            active={active === "repo"}
            onClick={() => setActive(active === "repo" ? null : "repo")}
            icon={FolderGit2}
            label="cxtasks"
          />
          <ChevronRight size={10} className="text-content-faint" />
          <CommandToken
            active={active === "agent"}
            onClick={() => setActive(active === "agent" ? null : "agent")}
            icon={agent === "claude" ? Sparkles : Code2}
            label={agent === "claude" ? "claude" : "codex"}
          />
          <ChevronRight size={10} className="text-content-faint" />
          <CommandToken
            active={active === "access"}
            onClick={() => setActive(active === "access" ? null : "access")}
            icon={ACCESS[access].icon}
            label={ACCESS[access].label.toLowerCase()}
          />
          <button
            onClick={() => setActive(active === "brief" ? null : "brief")}
            className={cn(
              "ml-auto flex items-center gap-1 rounded-md px-1.5 py-1 text-[9.5px]",
              active === "brief"
                ? "bg-accent/15 text-accent"
                : "text-content-faint hover:bg-content/6 hover:text-content"
            )}
          >
            <FileText size={10} /> brief
          </button>
        </div>

        <div className="relative min-h-[174px] pt-3">
          <div className="mock-capsule-panel h-full rounded-xl border border-border-subtle bg-input/70 p-3">
            {active === "repo" && (
              <CapsulePanel icon={FolderGit2} title="Workspace" meta="The run starts here">
                <RepoChip />
                <div className="mt-2 flex items-center gap-1.5 text-[9px] text-success">
                  <Check size={10} /> Git repo detected · tree will be snapshotted
                </div>
              </CapsulePanel>
            )}
            {active === "agent" && (
              <CapsulePanel icon={Bot} title="Choose a runner" meta="Same task, different execution model">
                <div className="grid grid-cols-2 gap-2">
                  {(["claude", "codex"] as const).map((item) => (
                    <button
                      key={item}
                      onClick={() => setAgent(item)}
                      className={cn(
                        "flex items-center gap-2 rounded-lg border p-2.5 text-left",
                        agent === item
                          ? "border-accent/35 bg-accent/10"
                          : "border-border-subtle bg-surface"
                      )}
                    >
                      <AgentMark agent={item} active={agent === item} />
                      <span className="text-[10.5px] font-medium">
                        {item === "claude" ? "Claude Code" : "Codex"}
                      </span>
                      {agent === item && <Check size={11} className="ml-auto text-accent" />}
                    </button>
                  ))}
                </div>
              </CapsulePanel>
            )}
            {active === "access" && (
              <CapsulePanel icon={Shield} title="Set the access envelope" meta="Pick the smallest useful boundary">
                <div className="grid grid-cols-3 gap-2">
                  {(Object.keys(ACCESS) as Access[]).map((item) => {
                    const Icon = ACCESS[item].icon;
                    return (
                      <button
                        key={item}
                        onClick={() => setAccess(item)}
                        className={cn(
                          "relative rounded-xl border p-2.5 text-left transition-all",
                          access === item
                            ? "border-accent/40 bg-accent/10"
                            : "border-border-subtle bg-surface hover:border-border"
                        )}
                      >
                        {access === item && (
                          <span className="absolute right-2 top-2 flex size-4 items-center justify-center rounded-full bg-accent text-white">
                            <Check size={9} />
                          </span>
                        )}
                        <Icon size={14} className={access === item ? "text-accent" : "text-content-faint"} />
                        <span className="mt-2 block text-[10.5px] font-medium">{ACCESS[item].label}</span>
                        <span className="mt-0.5 block text-[8.5px] leading-snug text-content-faint">
                          {ACCESS[item].detail}
                        </span>
                      </button>
                    );
                  })}
                </div>
              </CapsulePanel>
            )}
            {active === "brief" && (
              <CapsulePanel icon={FileText} title="Run brief" meta="What should the agent do first?">
                <PromptBox />
              </CapsulePanel>
            )}
            {active === null && (
              <div className="flex h-[148px] flex-col items-center justify-center text-center">
                <Circle size={18} className="text-content-faint" />
                <p className="mt-2 text-[10.5px] text-content-muted">Select a token to edit the run</p>
              </div>
            )}
          </div>
        </div>

        <div className="mt-3 flex items-center gap-2">
          <div className="flex items-center gap-1.5 text-[9.5px] text-content-faint">
            <Lock size={10} /> foreground only
            <button className="ml-1 text-accent hover:underline">enable background</button>
          </div>
          <button className="ml-auto rounded-lg px-2.5 py-2 text-[10.5px] text-content-secondary hover:bg-content/6">
            <Copy size={11} className="mr-1.5 inline" /> Copy prompt
          </button>
          <button
            className="flex items-center gap-1.5 rounded-lg bg-content px-3.5 py-2 text-[10.5px] font-semibold hover:opacity-90"
            style={{ color: "rgb(var(--bg-primary))" }}
          >
            Run now <ChevronRight size={11} />
          </button>
        </div>
      </div>
    </PreviewFrame>
  );
}

function CommandToken({
  active,
  onClick,
  icon: Icon,
  label,
}: {
  active: boolean;
  onClick: () => void;
  icon: typeof Eye;
  label: string;
}) {
  return (
    <button
      onClick={onClick}
      className={cn(
        "flex items-center gap-1 rounded-md border px-1.5 py-1 transition-all",
        active
          ? "border-accent/40 bg-accent/15 text-accent"
          : "border-border-subtle bg-content/5 text-content-secondary hover:border-border"
      )}
    >
      <Icon size={10} /> {label}
    </button>
  );
}

function CapsulePanel({
  icon: Icon,
  title,
  meta,
  children,
}: {
  icon: typeof Eye;
  title: string;
  meta: string;
  children: React.ReactNode;
}) {
  return (
    <div>
      <div className="mb-3 flex items-center gap-2">
        <span className="flex size-6 items-center justify-center rounded-lg bg-content/6 text-content-muted">
          <Icon size={12} />
        </span>
        <div>
          <div className="text-[10.5px] font-semibold">{title}</div>
          <div className="text-[8.5px] text-content-faint">{meta}</div>
        </div>
      </div>
      {children}
    </div>
  );
}

function PreviewFrame({
  caption,
  children,
}: {
  caption: string;
  children: React.ReactNode;
}) {
  return (
    <div className="mx-auto w-full max-w-[700px]">
      <div className="mb-2 flex items-center gap-2 px-1">
        <span className="font-mono text-[9px] uppercase tracking-[0.14em] text-content-faint">
          {caption}
        </span>
        <span className="h-px flex-1 bg-border-subtle" />
        <span className="text-[9px] text-content-faint">real panel width</span>
      </div>
      {children}
    </div>
  );
}

const VARIANTS: Record<
  MockVariant,
  { name: string; blurb: string; component: () => React.ReactElement }
> = {
  A: {
    name: "Guided stack",
    blurb: "Four calm decisions with a clear readiness footer",
    component: MockA,
  },
  B: {
    name: "Launch console",
    blurb: "A connected route from workspace → runner → capability",
    component: MockB,
  },
  C: {
    name: "Command capsule",
    blurb: "One compact command with click-to-edit tokens",
    component: MockC,
  },
};

export default function ExecutionContextMock() {
  const [variant, setVariant] = useState<MockVariant>("A");
  const [dark, setDark] = useState(
    () => document.documentElement.getAttribute("data-theme") !== "light"
  );

  useEffect(() => {
    document.documentElement.setAttribute("data-theme", dark ? "dark" : "light");
  }, [dark]);

  const Active = VARIANTS[variant].component;

  return (
    <div className="flex h-full flex-col bg-base text-content">
      <style>{`
        .mock-route {
          width: 66%;
          transform-origin: left;
          animation: routeIn 1.1s cubic-bezier(.22,.9,.28,1);
        }
        @keyframes routeIn { from { transform: scaleX(0); opacity: .2; } }
        .mock-capsule-panel { animation: capsuleIn .16s ease-out; }
        @keyframes capsuleIn { from { opacity: 0; transform: translateY(-2px) scale(.995); } }
      `}</style>

      <div className="flex items-center gap-3 border-b border-border-subtle bg-sidebar px-5 py-2.5">
        <div>
          <div className="text-[12px] font-semibold">Execution context — design lab</div>
          <div className="text-[9px] text-content-faint">A focused redesign of the agent handoff</div>
        </div>
        <div className="ml-3 flex overflow-hidden rounded-lg border border-border">
          {(Object.keys(VARIANTS) as MockVariant[]).map((item) => (
            <button
              key={item}
              onClick={() => setVariant(item)}
              aria-pressed={variant === item}
              className={cn(
                "px-3 py-1.5 text-[10.5px] font-medium transition-colors",
                variant === item
                  ? "bg-accent text-white"
                  : "bg-surface text-content-secondary hover:bg-elevated"
              )}
            >
              {item} · {VARIANTS[item].name}
            </button>
          ))}
        </div>
        <span className="text-[10px] text-content-muted">{VARIANTS[variant].blurb}</span>
        <button
          onClick={() => setDark((value) => !value)}
          aria-label="Toggle theme"
          className="ml-auto rounded-md border border-border p-1.5 text-content-muted hover:text-content"
        >
          {dark ? <Sun size={13} /> : <Moon size={13} />}
        </button>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto p-6">
        <Active />
      </div>

      <div className="border-t border-border-subtle px-5 py-1.5 text-center text-[10px] text-content-faint">
        Mock exploration page — data is static.
      </div>
    </div>
  );
}
