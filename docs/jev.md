# Jev integration

CXTasks can use TypeSafe's Jev model for bounded judgments while leaving its
deterministic rules in Rust and SQL. The integration is off by default because
the relevant task text is sent to TypeSafe — or, for capture, to OpenAI.

Configure it under **Settings → Jev**. The API key is read from
`TYPESAFE_API_KEY` when present; otherwise the app stores it in the host-local
`typesafe-api-key` file under the CXTasks data directory with owner-only Unix
permissions. It never enters SQLite, sync snapshots, or task prompts.

When enabled, the individual switches provide:

- **Capture suggestions:** project, priority, inbox/actionable state, and known
  tags. Explicit `#project`, `!priority`, due-date syntax, and the selected
  project always win. Answered by the **capture engine** (below), not
  necessarily Jev.
- **Pre-dispatch check:** checks outcome clarity, context, unresolved user
  decisions, and scope. A concerning result makes the first click advisory;
  a second click delegates anyway. Existing runner guards remain authoritative.
- **Post-run review:** adds a `jev` comment only when a successful handoff may
  miss the request, lack verification, or leave material questions.
- **Recall reranking:** reorders local FTS search candidates and cited
  retrospective evidence. Retrieval and citations remain local and unchanged.

## Jev tripwires

`jev:<condition>` evaluates the condition against the task fields and its eight
most recent comments. It does not browse, run commands, or fetch evidence. Use
`script:` for machine-readable external state and `agent:` when investigation
inside a repository is required.

Jev tripwires have a five-minute cadence floor and run in the runner's cheap
tripwire lane. A probability of 0.75 or higher trips the task. Provider errors
use the same backoff and eventual surfacing rules as script failures.

## Capture engine: OpenAI Decisions by default

Capture alone is answered by OpenAI's Decisions API (`gpt-6-luna`) unless
**Settings → Jev → Capture engine** says Jev (`app_meta.capture_engine`,
`decisions` | `jev`). Readiness, review, `jev:` tripwires and reranking stay on
Jev: capture is the only judgment measured against both, and the others have no
labels to measure on.

Why: on 270 of Chris's own tasks (2026-10-06, `crates/cxtasks-core/examples/jev_vs_decisions.rs`,
leave-one-out) Decisions matched Jev on project (49.3% vs 44.8% top pick), beat
it on priority (42.6% vs 28.5% exact; Jev called 39 tasks P0 where 7 are), and
is under OpenAI's API terms (30-day abuse logs, no stored application state) —
TypeSafe states no deletion window, and no privacy decision was ever recorded
for it. Cost is ~$0.50 per 1,000 captures against Jev's ~$0.10; p50 is ~130 ms.

**The key** is `OPENAI_API_KEY`: from the environment, else the login Keychain
(secrets-kit service `cx-secret`, read by the process itself, as for
`TYPESAFE_API_KEY`). There is no local-file fallback. Settings shows where it
was found.

**It never breaks capture.** No key, an HTTP error, a timeout (6 s) or a reply
with no answers → Jev is asked instead → if Jev fails too, the deterministic
route files it. `CaptureAssessment.engine` says who answered.

**Per-question degradation, both engines.** A `"type":"refusal"` answer (11 of
274 Decisions calls had one), a missing answer, a choice outside the offered
keys or a probability outside 0–1 is *no judgment* for that one question: no
project, P3 (the create default), `inbox`, no tag, not vague. The rest of the
assessment stands.

**Bars are per engine** (`jev::JEV_BARS`, `jev::DECISIONS_BARS`), tuned on half
the labeled tasks and checked on the other half:

| bar | Jev | Decisions | held-out effect (Decisions) |
|---|---|---|---|
| project confidence | 0.55 | 0.70 | precision 81.0% at 48.3% coverage (Jev 73.9% at 57.5%); unlabeled tasks given a project 36%, same as Jev |
| tag | 0.72 | 0.96 | 0.98 tags per task, same as Jev (2.6 at 0.72) |
| too vague | 0.90 | off | Decisions' clarification answer does not separate non-briefs among the capture questions ("do the thing" 0.98, "fix the pocketbuddy budget sync bug" 1.00), so the advisory is not raised from it |
| todo needs clarification below | 0.65 | 0.65 | not retuned |

`too_vague` is computed against the answering engine's bar and carried on the
assessment; `intake` reads that flag rather than comparing the raw number.

