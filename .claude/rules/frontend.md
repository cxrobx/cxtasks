---
paths:
  - "src/**"
---

# Frontend Rules (React / WKWebView)

Component map and every UI-side gotcha. Invariant and gotcha numbers are
**global across the rules files** — the index in `CLAUDE.md` maps every number
to its file. Three cross-layer rules that bite here: invariant 9 (P0 is the
HIGHEST priority — `PRIORITY_COLORS` in `src/lib/format.ts` is indexed so 0 is
red, and "worth showing" thresholds are `priority < PRIORITY_LOWEST`, full text
in `database.md`); invariant 21 (the filter strip's chips REFINE the view and
there is ONE query builder, full text in `database.md`); gotcha #8 (every theme change goes through
`uiStore::applyTheme`, which sets the CSS attribute AND the native appearance —
full text in `macos-native.md`).

## Component map (the load-bearing details)

- **`components/tasks/TaskList.tsx`** renders **one** grouped board over a
  `Group[]` descriptor — sections and projects are two builders feeding the same
  loop, so the sticky headers, collapse keys, drop targets and (the one that
  bites) the render order a shift-click range is measured against cannot drift
  apart. The mode is `uiStore.groupBy` (`none` · `section` · `project`, default
  `section` = the pre-grouping behaviour exactly), and `groupBy` is a
  *preference* while the applied `mode` is resolved at render — grouping by
  project inside a scope holding no projects, or anywhere in the Trash, falls
  back without rewriting what's stored. **"Holds no projects" is `nested`, not
  `activeProject !== null`**: an AREA's list rolls its projects up
  (`db::projects::expand_scope`), so grouping inside one carves the roll-up back
  into the piles it was made of, and the option is live there. The disabled
  segment and the `mode` fallback read that ONE value — the two disagreeing is a
  live-looking control that silently renders sections.
- **`TaskList.tsx`'s two derived groupings** (2026-08-25): the **Waiting**
  view groups by WHY — Until a date · Until a condition · Behind another task ·
  Marked blocked — in the order the runner judges the gates (date first,
  invariant 26), ignores `groupBy`, hides the group-by control, and prints
  under each row the one fact the group can't say (`waitingSince` → the
  tripwire's `last_note`, the start date, the blocker). **Today** puts a
  `Decide` group first when any row carries `DECISION_TAG` (`isDecision` over
  `tagsByTask`); its rows render `DecideLine`, an input whose Enter calls
  `complete(id, answer)` so the line becomes the handoff (invariant 18). Every
  event stops at that input — the row selects on click and drags on mousedown,
  and a line you type into must not do either (gotcha #16). The section and
  project builders take `rows` so the strip can subtract its tasks first.
  Person tags render through `tagLabel` (`@sam`, not `#@sam`).
- **Below Notes is `components/tasks/DetailTabs.tsx`** (2026-09-28): the
  launch console as ONE line, then **Activity · Subtasks · Links**. It
  replaced seven headed sections (Tags, Subtasks, Blocked by, Continuity,
  Runs, Comments, the Archive/Trash footer) that rendered whether or not they
  held anything — ~600px below the note on a bare task, three of them saying
  only that they were empty. Tags moved into `PropertyStrip` (`#tag` / `@name`
  chips, `+ → Tag`), Archive / Move to Trash into `TaskMenu` (⋯ beside the
  reference; a task already set aside keeps its visible "Put back"). Four
  things in it are findings, not styling:
  **(1) A closed tab's COUNT is what keeps it honest** — the Links count is red
  while a blocker is open (warning while superseded), because a blocker is the
  one link that changes what you can do and must not hide on a tab you didn't
  pick.
  **(2) The default tab is DERIVED until you pick** (first tab with content):
  runs, comments and relations load after the pane mounts, so a default fixed
  at mount is decided against empty lists. Per-task, like the console's
  collapse — the form is keyed on `task.id`.
  **(3) `TaskActivity` interleaves runs and comments, newest first**, composer
  on top. The two sources arrive in OPPOSITE orders (comments chronological,
  runs newest-first), so `activityEntries` sorts by parsed time with each
  source's own position as the tiebreak; `db::comments::for_task` stays
  chronological for the agent-facing readers (the old `TaskComments` comment,
  kept on `TaskActivity`).
  **(4) `TaskLinks` pickers are `Combobox`, never `<select>`** (gotcha #6). A
  pick is recognised by the field's text EXACTLY matching an option
  (`T188 · title`), since `Combobox` only sets the value; typing part of a
  title links nothing.
- **`TaskDetail.tsx`'s `ExecutionConsole`** is the **launch console**, where the
  four-node rail (workspace · runner · capability · brief) IS the control
  surface — see gotcha #24. Its header is one line: collapsed, the line IS the
  plan (`repo · runner · access` + Ready); with no workspace it is a faint
  "Hand to an agent…" instead of a warning card, since for an errand no repo
  is the normal state.
- **`components/tasks/NotesField.tsx`** is Notes: **rendered markdown until you
  click into it**, through the one `Markdown` surface (gotcha #28). Three things
  in it are structural rather than styling. The **wrapper owns the height** —
  both modes render inside one element that survives the swap, so the grip you
  drag in preview is the same grip once you are typing; giving each mode its own
  resizable box means every click into a note resets a height you set. The
  height is **pinned, never capped**, for `RunResult`'s reason. And a **pointer
  delta guards the click**: the resize grip sits in the box's own bottom-right
  corner, so letting go of a drag fires a click on the preview, and without the
  guard every resize opens the editor and throws away the render you were
  resizing to read. It is deliberately NOT wrapped in a `<label>` — a label
  whose control is only sometimes mounted forwards a click to nothing. A long
  note opens **down to the visible bottom of the pane** (`openingHeight`,
  measured against the nearest scrolling ancestor, floor 260px, 48px left for
  the line under it) — measured rather than a bigger constant, which would
  overshoot a short window and undershoot a tall one.
- **`components/tasks/PropertyStrip.tsx`** replaced the detail pane's nine-field
  grid (2026-09-27): only what the task HOLDS gets a chip, in the row's own
  vocabulary, and `+` adds the rest. The grid had Notes starting 580–710px down
  on a laptop window, mostly under empty controls and values the row already
  printed. Five things in it are findings, not styling:
  **(1) Closing a popover FLUSHES its draft** — `ChipPopover` blurs the focused
  element inside the panel before the close, because every editor in there
  commits on blur and an unmounted input never blurs (gotcha #24). Escape
  skips the flush and means cancel.
  **(2) The `+` menu is `modal={false}`.** A modal menu's focus trap is still
  live in the commit that mounts the popover the pick opens, so the editor's
  autofocus is pulled back, the input blurs, and `HoldField` drops out of
  editing before it is seen. Only a test caught it.
  **(3) A held task still reads as held** — `HoldField`'s contract moved here:
  start date, tripwire and written reason keep the row's warning/error tints,
  and an open blocker gets a Waiting chip even with no reason written.
  **(4) The status hints live in the status MENU**, where choosing happens —
  still the defence against status drift, just not a resting grey line.
  **(5) The editors are the existing components**, moved not rewritten
  (`DateField`, `TripwireField`, `HoldField`, `Combobox`), so each keeps its own
  draft discipline and tests. Adding a date seeds it at once (the WKWebView
  empty-`datetime-local` trap); any other added property renders a pending chip
  to anchor its popover, cleared 400ms after the popover closes so the chip
  doesn't blink out during the commit's `patch` → `refresh` round trip.
- **`components/shared/UndoToast.tsx`** is the undo affordance — since v14 it
  renders the top of `taskStore.undoStack` (and, for a moment after a ⌘Z, the
  confirmation), not an undo it owns — see gotcha #29.
- **`components/gsd/GsdBoard.tsx`** is v6's project board, **derived from
  activity since the Open split** — the row's marker is `BoardRow.activity`
  (Active / Idle *N*d / Dormant / Empty), resolved in Rust by
  `ActivityState::from_idle_days` and only *styled* here. Don't re-derive it
  from `idle_days` in TypeScript: the MCP prints the same state, and a second
  copy of the thresholds is how the two surfaces start disagreeing about one
  project. The health dot and the staleness warning are **gone** — `health`
  still renders when it was deliberately reported, but nothing is flagged for
  the absence of a written update (that measured a reporting habit, not the
  project; `isUpdateStale`/`STALE_UPDATE_DAYS` were deleted with a note in
  `format.ts` saying not to bring them back). `ActivityStrip` scales each
  project's 14 bars against **its own** busiest day, or one busy project
  flattens every other row's rhythm to nothing;
  **`components/mission/MissionControl.tsx`** is the agent dashboard.
- **`components/layout/SettingsDialog.tsx`** is the Appearance sheet holding the
  transparency slider (gotcha #22).
- **`components/layout/ProjectReposDialog.tsx`** is v11's repo mapping, reached
  from the sidebar project's right-click menu — the **only** way into the table,
  and without an entry point a mapping nobody can set resolves nothing.
- **`components/layout/FilterStrip.tsx`** is the row under the header that says
  what the list is filtering (invariant 21). Chips left the search box because a
  view's identity is a *sentence* ("Due later, deferred, or on a tripwire"), and
  a sentence plus two filters plus the controls wrapped a 30rem field to a
  second row at the third chip — which is an area, the common case. **LOCKED**
  chips label `db::tasks::view_where` and are never sent anywhere (the `view`
  field carries them, so this file cannot drift into a second, quieter
  definition); **default** chips are real `ActiveFilter`s and come off. One pill
  per (facet, op) with the values joined, because `Status is not Done, Blocked`
  as two pills says half a thing twice. The two controls are ICONS — a bookmark
  beside `+ Filter`, where the thing it saves is, and sort arrows at the right —
  since a text button is precisely what forced the second row; state is colour
  (accent = actionable) and the detail is in the `title`. Its popovers are
  dismissed in the CAPTURE phase (gotcha #15): the strip is not a drag region
  but the header above it is, and every dismissing click crosses it.
  **`components/layout/SearchBox.tsx`** is what is left in the header — text
  only, refining the current view rather than escaping it — and it keeps
  `.cx-filterbar` because the global `input:focus-visible` ring would otherwise
  draw a second box inside its border.
  **`src/lib/viewScopes.ts`** holds what each view IS and what it defaults to,
  plus `defaultsFor` (a project with no saved filters inherits its AREA's — the
  direction `primary_repo` already falls back in) and `sameFilters`, which is
  order-insensitive because the chips are a set.
- **`components/layout/Sidebar.tsx`** splits `VIEWS` (drag-reorderable, ordered
  by `uiStore.viewOrder`) from `PINNED_VIEWS` (Archive, anchored below the
  divider with **no drag handlers at all** — the divider claims work lives above
  it, so a row draggable across it turns a boundary into a line above one
  particular row), and both render through one shared `viewButton` so a pinned
  row can't drift into looking disabled. Below them, the **Areas** list is a TREE
  (invariant 22 — areas nest to any depth since 2026-09-27; `renderArea` walks
  it, 16px of `marginLeft` per level, and a between-rows drop moves the row to
  that row's level, which is how a node comes back out to the top; no zone is
  offered inside the dragged row's own subtree): `projectRow` renders an area or a nested project
  from ONE renderer — they are the same affordance (click to scope, drag to
  reorder, right-click to manage) and splitting them is how the drag handling,
  the rename hook-up and the selected styling end up subtly different for the
  half you touched last. Only the menu and the marker differ (folder vs target
  icon, `pl-2` vs `pl-6`). An area that HOLDS projects trades its folder for a
  **disclosure chevron** — `uiStore.collapsedAreas`, persisted, keyed by **id**
  so a rename can't spring it open (the same exception `parent_id` makes: this
  is a note about a row, not one of the four text joins invariant 17 moves). One
  glyph, not two: a chevron says "holds things" as well as a folder does, and an
  area holding nothing keeps the folder rather than growing a control that would
  change nothing. That chevron is the reason the row's PAINT lives on the
  `MenuTrigger` and not on the button — a button inside a button is invalid and
  would swallow the row's own click, so the selected background has to belong to
  the element spanning both, and the marker only leaves the row button when
  there is a chevron to draw (hoisting it always would put a 30px dead zone in
  front of every project name). Two things collapse must never hide, both
  derived at render rather than written back: the project you are **standing
  in** (the sidebar's job is to answer "where am I") and the inline "new project"
  field, whose menu item calls `expandArea` first. The children are rendered BY
  the area rather than filtered out of a flat map, so a project is drawn exactly
  once. A project
  belonging to no area is kept visible under its own heading at the bottom —
  a real, usually temporary state that should read as an open question, not as
  one more area. **Only a node with open work is drawn** — `visibleTags`' rule,
  with its exception: the node you are **standing in** stays (and so does the
  area over it, so the nested row has a parent). The sidebar is a filter over
  open work, and an area at zero has nothing open anywhere beneath it because
  its count rolls up. `areas` itself stays UNFILTERED — it feeds the "move
  to…" menu, and an empty area is exactly what a project gets moved into. It
  is also why `taskStore.createProject` **selects the node it just made**: a
  new node has no work, so without that the row you just typed would appear
  nowhere and the field would seem to have done nothing. Selection alone only
  lasts until you click away, so the sidebar also pins the name in a
  session-only `justCreated` list (never persisted — a stored copy would be a
  setting that quietly undoes hiding). **Right-click the Areas label → "Show
  hidden"** (`uiStore.showHidden`, persisted) draws every empty area and
  project, and is how you reach an empty area to drop a project onto.
  (2026-09-26 briefly drew every area empty or not; reversed 2026-09-29.) The GSD board is
  untouched — `empty` is one of the states it exists to report. **Nothing here
  re-derives a count**: `open_count` already rolls
  up server-side, and a second roll-up in the renderer is how the badge and the
  list it opens start disagreeing. Every PARA rule (the depth cap, "an area has
  no finish line", "move the children out first") is enforced in `db::projects`
  and only SHOWN here — `projectMutation` surfaces the server's refusal rather
  than the UI pre-guessing it.
- **`src/assets/fonts/`** — **Geist Mono only** is loaded: the variable face,
  vendored from the `geist` npm package with its SIL OFL licence. The package is
  deliberately **not** a dependency (it ships ~10 MB of unused static weights).
  UI text is the **system stack** (San Francisco), declared byte-identically to
  cxmail's `--font-sans` so the two apps read as one family; nothing is
  downloaded for it and there is no `@font-face`. `Geist-Variable.woff2` stays
  checked in, unreferenced — Vite bundles nothing no rule points at, so keeping
  it is the entire cost of putting Geist back.
- **`src/mocks/RedesignMock.tsx`** — design-exploration page, reached only via
  `?mock` and lazy-loaded so it never enters a normal launch. **Every variant
  renders on one shared material**, held by the shell along with its controls
  (sheet + card alpha, kept per theme; a 3D-tilt toggle) and one simulated
  desktop behind them all — so the switcher answers *which layout*, not
  *before or after*. Variant A used to render the **real shipped `TaskRow`**
  and no longer does; that comparison had been made, and it is worth knowing
  the row is now unreachable outside the Tauri app, which cost this page its
  one side-by-side view of the four priority states.
  The material is **frosted**: a white scrim in light, a black scrim in dark,
  with the backdrop's saturation pulled *below* 100%. Four things in it are
  findings rather than styling, and each cost a round:
  **(1)** it must take its colour from the **scrim**, not the backdrop. Built
  from palette tokens (`--bg-surface`/`--bg-primary`) it looked right and came
  out **brown in both themes** — those tokens are near-neutral and very dark,
  so at the alpha translucency needs they stop being a colour and become a
  veil, and the wallpaper's hue walks straight through. That is *tinted* glass.
  **(2)** no bevel. The first pass ported cxmail's `GlassHarness` **kit B**
  ("Specular depth · no blur") — whose own verdict warns *"rows read as raised
  keys rather than glass"*, which is exactly what happened. Read that harness
  before extending this one.
  **(3)** a row needs **no `backdrop-filter`** and cannot use one (gotcha #22:
  its backdrop is the pane's flat tint, so it would blur a solid colour). It
  does not need to — alpha composites all the way down, so a translucent row
  inherits the real desktop blur from the `NSVisualEffectView` beneath. The
  earlier claim that a row "can never be real glass here" was too strong: it
  cannot add blur, but it can be genuinely translucent.
  **(4)** light and dark are **two materials, not one at two values** — a black
  scrim at 0.58 is a comfortable frost, a white one at 0.58 is a washed-out
  sheet. Hence `GLASS_DEFAULTS` and the per-theme dials.
  Focus and multi-select are deliberately given **different channels** (accent
  rim vs fill) — with every row lit, one channel cannot carry both.
  **Card values come from cxventures' `.glass-card`** (`app/globals.css` in that
  repo): fill `rgba(255,255,255,0.03)` dark, rim `0.06`, hover doubling the fill
  (the card dial ships one step below that, at **0.02** in both themes — in
  light it makes a card read almost entirely by its rim, which beats the
  white-on-white it replaced)
  and adding a `0 0 30px` **outer glow** on a 0.3s ease. The restraint is the
  finding — at 0.07/0.12 the rim stops being an edge and becomes a drawn
  outline, which is what made an earlier pass read as boxes. Hover **composes
  onto** the state rather than replacing it, so a focused or multi-selected row
  keeps its rim and only gains the glow. The cursor tilt is on by default and
  capped near 1° (`/46`): a list is scanned down a column, so a row that
  visibly swings as the pointer crosses it fights the scan it serves.

## Gotchas (frontend)

### 6. WKWebView renders `<select>` popups light-grey regardless of theme
**Symptom:** a bright grey menu erupts out of the dark app when a dropdown is
opened. `option` elements can't be styled on macOS, so there is no CSS fix.
**Fix:** `components/shared/Select.tsx` uses a Radix Select with a menu styled
to match macOS dark menus (translucent panel, hairline border, checkmark on the
left, accent-filled highlight). The closed-state trigger keeps the same look as
the other fields.
**Same for `<datalist>`, only worse:** WKWebView draws its suggestion popup
natively and, in our transparent window, puts it at the far LEFT edge of the
screen — outside the app. Every type-ahead field uses
`components/shared/Combobox.tsx` instead (Select's panel and rows over a Radix
Popover anchored to the input). A pick SETS the value on `mousedown` with the
default prevented and leaves commit to the caller's blur/Enter — a click would
blur first and save the half-typed word. And never give a Radix `Select` item
the value `""`: Radix reserves it for "no selection", so the trigger renders
blank (the Section picker's `UNGROUPED` sentinel is why).


### 10. Every flex/grid child in the pane chain needs `min-h-0`
**Symptom:** the sidebar and detail pane are cropped at the window's bottom edge
with **no scrollbar** — content simply unreachable. Worse at short window heights.
**Cause:** flex and grid items default to `min-height: auto`, which refuses to
shrink below content height. A tall pane therefore overflows its grid row, and
the `overflow-y-auto` inside it never receives a bounded height, so it never
becomes scrollable. Adding `overflow-y-auto` alone does nothing.
**Fix:** `min-h-0` on **every** link in the chain — the grid container, each grid
child, and each nested flex column — plus `shrink-0` on fixed chrome (header,
quick-add, budget panel). See `AppLayout.tsx`, which carries the comment.
**Also:** the sidebar's budget panel is deliberately OUTSIDE the scroll area.
It's a spend guard; it must not be the thing that scrolls out of sight.


### 13. The sectioned task list is deliberately NOT virtualized
`TaskList.tsx` has two render paths, switched on `sections.length > 0`. With
sections present it uses plain scroll; without them it keeps
`@tanstack/react-virtual`. That asymmetry is intentional, so don't "fix" it:
interleaving section headers into a virtualizer's flat index space is a bug
factory — the row-recycling problems that already need `getItemKey` +
`measureElement` (see the comment there: without both, rows recycle onto the
wrong task and the list visibly scrambles) get much worse once indices no longer
map 1:1 to tasks — and a hand-curated sectioned board is tens of rows, not
thousands. The ungrouped path keeps the virtualizer for the lists that need it.


### 15. Window dragging is an ATTRIBUTE, not CSS — `-webkit-app-region` is a no-op
**Symptom:** the window can only be moved by grabbing the extreme top edge;
everywhere else in the titlebar strip a drag does nothing.
**Cause:** `-webkit-app-region: drag` is an **Electron** property. WKWebView
does not implement it and a webview cannot move its host `NSWindow` on its own,
so the declaration is silently inert — it looks like the right incantation and
does nothing at all. Tauri's mechanism is the `data-tauri-drag-region`
attribute: its injected `drag.js` walks `e.composedPath()` on mousedown and
calls `plugin:window|start_dragging` itself. **Markup is the only thing that
matters.** The only draggable strip without it is the sliver of native titlebar
the webview doesn't cover, which is exactly "the very very edge".
**Fix:** `data-tauri-drag-region="deep"` on every pane that reaches the window
top. Three details, all from reading `drag.js` rather than guessing:
1. **`deep` vs bare.** A bare attribute means *only a direct click on that exact
   element* drags — `drag.js` does `return el === composedPath[0]`. So a bare
   attribute on the header leaves the `<h1>` heading and the task count as dead
   spots, which reads as "dragging works in some places". `deep` accepts any
   descendant. `"false"` blocks a subtree.
2. **Buttons and inputs need no opt-out.** `isClickableElement` already excludes
   `A/BUTTON/INPUT/SELECT/TEXTAREA/LABEL/SUMMARY`, `contenteditable`, anything
   with a real `tabindex`, and the interactive ARIA roles. There was once a
   `.cx-no-drag` class for this; it was redundant and is gone.
3. **A region inside `overflow-y-auto` scrolls away.** Reserving the headroom as
   `padding-top` on a *scroller* leaves the strip draggable only at scroll-top,
   so `Sidebar` and `AppLayout`'s detail wrapper each supply it as a separate
   `shrink-0` strip outside the scroll container. That also stops rows sliding
   under the floating traffic lights.
`user-select: none` on the region is **not** what makes this work — Tauri calls
`preventDefault()` when it accepts the drag. It only suppresses the I-beam and
any selection flicker in the frame before that, and form fields opt back in
because `user-select` inherits.

**AND the markup is still not enough — the ACL has to allow the command.**
`drag.js` ends at `invoke('plugin:window|start_dragging')`, which is an ordinary
ACL-gated command. `core:window:default` grants only **read-only getters**
(`allow-inner-size`, `allow-is-focused`, …) plus `allow-internal-toggle-maximize`
— *not* `allow-start-dragging`. So out of the box, correct markup produces a
drag that is authorized away, and `capabilities/default.json` must list
`core:window:allow-start-dragging` explicitly. Two tells that this is the layer
you're stuck on rather than the markup:
* **Double-click-to-maximize on the titlebar works while dragging doesn't** —
  `internal_toggle_maximize` is granted by default and `start_dragging` isn't,
  so the same strip responds to one gesture and not the other.
* Nothing in the DOM looks wrong, because nothing in the DOM *is* wrong.
Capabilities are compiled by `tauri-build`, so a change here needs a Rust
rebuild — a frontend hot-reload will not pick it up.
**Verify in two places, because each catches only its own layer.** For the
markup, port `isDragRegion` from
`~/.cargo/registry/src/*/tauri-<ver>/src/window/scripts/drag.js` into the dev
tab and probe `document.elementFromPoint` at the heading, the empty header, each
side strip, and a button — checking the attribute is merely *present* proves
nothing, since bare-vs-`deep` looks correct in the DOM. For the grant, read
`src-tauri/gen/schemas/capabilities.json` after a build and confirm
`start-dragging` resolved into it; the source capability file listing it is not
proof that the build picked it up.

**A drag region SWALLOWS `mousedown` for the whole app, so every click-outside
handler must listen in the CAPTURE phase.** `drag.js` calls
`stopImmediatePropagation()` (its own comment cites tauri#2549) from a
**bubble**-phase listener on `document`, so any bubble listener on `window` —
or on `document`, registered later — never sees a click that landed on the
header, the sidebar's top strip or the detail pane's. That is most of the app's
chrome, and the failure is one-sided and baffling: the filter bar's facet
popover dismissed on a click in the task list and *stayed open* on a click in
the header two inches away. Register these as
`document.addEventListener("mousedown", fn, true)` — capture runs before any
listener further along the path and cannot be stopped by one.
Two follow-ons: the same `preventDefault()` means the click **does not move
focus**, so a popover that reopens on `onFocus` goes dead after one dismissal
(the field it would refocus is already focused and fires no event) — open it
from the click instead; and a jsdom test cannot see any of this unless it
installs the adversary itself, which is what
`components/layout/__tests__/filterPopover.test.tsx` does and why deleting that
15-line helper makes the file pass against the broken version.


### 32. A task row is a frosted CARD, and its states compose through VARIABLES
**Where:** `.cx-task-card` in `globals.css`, applied by `TaskRow`.
**The rule:** every part of the card — fill, rim, glow, drop indicator, tilt —
is a CSS custom property feeding one `background` and one `box-shadow`. Nothing
sets those two properties directly except the base rule.
**Why it has to be that way:** the states STACK. A row can be selected *and*
hovered *and* a drop target at once, and the first two attempts composed them by
overwriting whole declarations. Both failed the same way and neither looked like
a bug: hovering a selected row silently stripped the accent rim — removing the
one mark saying which row you are on, at the exact moment you were pointing at
it — and `.cx-drop-above`'s `box-shadow` replaced the rim wholesale for as long
as a drag lasted. Cascading variables makes it structurally impossible: each
state owns one slot and cannot claim another's.
**Three consequences worth knowing before editing it:**
- Hover is split in two on purpose. `.cx-task-card:hover` adds only the glow, to
  every row; the fill/rim bump is `:not([data-card-state]):hover` so a row with
  a state of its own keeps it.
- **Selection is a FILL, not a rim** — the original `accent/12` and `accent/22`,
  unchanged. It spent one revision as an accent rim (a fill and a rim are two
  channels, so the two states provably cannot collide) and that was the wrong
  trade: a rim reads as a drawn box, and a highlight is the one thing in a list
  that has to read as light. The 0.12/0.22 gap is load-bearing — with both
  states tinted, they are told apart by strength alone.
- **`data-card-state` is selection ONLY** (`checked` / `selected` / absent). A
  running task briefly set it too, and that was one thing said twice: the status
  pill already reads "In progress" in accent. `--card-rim` now means the card's
  edge and nothing else — no state claims it, which is also what keeps the rim
  free to stay as faint as gotcha #32's geometry note requires.
- `background-color` is deliberately untouched by the card. `.cx-row-flash`
  animates it, and animations outrank normal declarations, so the live-bridge
  flash reads over the card without either rule knowing about the other.
- `.cx-drop-above/below` are shared with `Sidebar`, so they are **scoped**
  (`:not(.cx-task-card)`) rather than redefined; the card gets its own
  `--card-drop` variant.

### 45. Density is variables + ONE attribute, and the row estimate moves with it
`applyDensity` stamps `data-density` on `<html>` (before React mounts, like
`applyTheme` — set it after the first paint and the list visibly re-flows on
every launch); `globals.css` turns that into `--row-py`/`--row-pl`/`--row-gap`
and a handful of `[data-density=…] .cx-row-*` rules. Same shape as cxmail's,
deliberately: the two apps sit side by side and one idea should not have two
names.

**Three things in it are decisions, not styling.**
**What each mode DROPS is the design.** `compact` takes the row to one line by
turning `.cx-row-body` from a column into a row — the badge strip *moves*, it
is not hidden — and drops the tags and the project, which are what you scan for
only when hunting one and are what makes the strip too wide to share a line.
What never goes at any density is a badge saying the row cannot be worked
(blocked, deferred, tripwire): a denser list that hid those would be a list
lying about what is actionable, which is the one question the board exists to
answer.
**The Decide line keeps its own row at every density** (`.cx-row-decide` is
`flex: 1 0 100%`, which is why the compact body also sets `flex-wrap: wrap`).
It is an input you type a sentence into, not metadata — inline it came out
about 200px wide, making the cheapest close on the board the fiddliest.
**`DENSITY_ROW_ESTIMATE` is paired with the padding.** The virtualizer sizes
its scrollbar from `estimateSize` until each row is measured, so an estimate
that does not follow the padding makes the thumb jump on the first scroll —
gotcha #33's failure, arriving from the other direction. Pinned by a test
asserting every mode has a number and that denser means smaller, because
nothing about the failure errors.


### 33. The row gap belongs to the row's CONTAINER, never the row
**Symptom:** in the ungrouped (virtualized) list, rows overlap by exactly the
gap. The grouped list is fine.
**Cause:** `@tanstack/react-virtual` sizes each item with `measureElement`,
which reads the wrapper's border box. A bottom **margin** on the child collapses
straight through a wrapper that has no padding or border, so the measured height
comes back short and every row is positioned that much too high.
**Fix:** the gap is `pb-1.5` on the absolutely-positioned wrapper in the
virtualized path and on the `renderRows` wrapper in the grouped one — padding,
which is inside the border box, never margin. And `estimateSize` has to follow
the new height (56 → 62): the scrollbar is sized from the estimate until each
row is actually measured, so a stale one makes the thumb jump during the first
scroll.

### 16. A global shortcut that fires while typing will eventually destroy something
**Symptom:** editing a task's title, notes or a comment, pressing ⌘⌫ to clear
the line — and the task is deleted instead.
**Cause:** `useKeyboardShortcuts` handled ⌘⌫ **above** its `if (typing) return;`
guard, and said so in a comment ("deletes the selected task even while typing
elsewhere"). In every macOS text field ⌘⌫ means *delete to start of line*, so
the most destructive shortcut in the app was bound to a keystroke with a
completely different, extremely common meaning in exactly the context where it
was most likely to be pressed by accident. It cost a real task before it was
caught.
**Fix:** the ⌘⌫ handler now sits **below** the `typing` guard, and calls
`trash` rather than a hard delete. Moving it back above re-introduces the bug.
Only ⌘N / ⌘F / ⌘\ are deliberately global — none of them destroys anything.
**The general rule:** a shortcut allowed to fire while typing must be
non-destructive. Before adding one above that guard, check what the keystroke
already means inside a text field.


### 23. Two silent-fallback traps in the theme CSS
Neither of these errors; both just quietly render the wrong thing.

**`format("woff2-variations")` is a Safari-11-era spelling.** A modern engine may
reject the whole `src`, and a rejected `src` is not a console error — it falls
through to the next family in the stack, which for the one remaining `@font-face`
(Geist Mono) is `ui-monospace`. The app then looks entirely fine, in SF Mono, and
the only way to notice is to already know what Geist Mono looks like. Use plain
`format("woff2")`, which every engine accepts and which handles the variable axes
anyway. The `font-weight: 100 900` **range** is load-bearing for the same reason
— declare a single weight and every weight renders at 400, which reads as "this
font has no bold". (The UI sans is now the system stack with no webfont at all,
so neither trap applies to it — but it is also why a silent mono fallback is
*harder* to spot than it used to be: SF Mono beside SF Pro looks deliberate.)
Verify by *measurement*, not by looking: `document.fonts` status plus a
`measureText` width compared against the fallback and against an unknown family.
Eyeballing letterforms in a screenshot cannot distinguish these.

**The pre-hydrate alpha fallback is not optional.** `applyWindow` writes the
alphas from JS, and the first paint can precede it — without a `:root` default
every alpha is invalid at computed-value time and the panes paint fully
transparent, flashing the app as a bare vibrancy panel with floating text.
Opaque is the safe default. (An earlier cut set the levels via `[data-window]`
selectors instead, where this fallback was an active hazard: `:root` and
`[data-window="solid"]` have IDENTICAL specificity (0,1,0), so a `:root` block
placed after them won on source order and pinned the app to solid, killing every
level while the CSS still read correctly. Inline styles beat any selector, so
the current form has no such ordering to get wrong.)

### 24. A field that UNMOUNTS loses its blur, and a commit-on-blur draft with it
**Symptom:** type a new repo path into the launch console's workspace editor,
click another rail node, and the value is still on screen — then silently
reverts at the next live-refresh. Nothing errors. It looks saved right up until
it isn't.
**Cause:** `repo_path` and `prompt` are local drafts in `TaskDetailForm`
committed on **blur** (`onRepoCommit` / `onPromptCommit`), which was safe for as
long as the fields were always mounted: focus moving anywhere fired blur. The
launch console's rail renders **one editor at a time**, so clicking another node
— or collapsing the console — *removes the focused input from the DOM*, and a
focused element that is unmounted does **not** reliably fire blur. The commit
never runs. `patch` then bumps `updated_at`, whose effect resets every draft in
the pane, which is what erases it later and makes the two events look unrelated.
**Fix:** `leaveOpenEditor()` commits the open editor's draft *before* changing
`open` or toggling the collapse, and every path out of an editor calls it. Both
commits already no-op when the draft matches the stored value, so merely opening
and closing an editor writes nothing — which matters for the same `updated_at`
reason, inverted: a spurious write on every glance at the rail would discard
text being typed elsewhere in the pane.
**Generalizes to any conditionally-rendered commit-on-blur field.** Pinned by
`launchConsole.test.tsx`'s "leaving an editor with unsaved text" block, whose
third case asserts the *absence* of a write — don't drop it, it's the half that
stops the fix becoming a new bug.

**The console opens COLLAPSED on every task, and the reset keys on `task.id`
alone.** It is per-task state, not a remembered preference — expanding is a
decision about the task in front of you, and since most tasks carry no
execution context, a console that stayed open followed you across the board
after one use. `uiStore.launchCollapsed` therefore isn't persisted: a value
every task open overwrites is one nobody reads, and leaving it in
`localStorage` makes it look like a setting.

Two traps in implementing that, both pinned:
* **Its effect must not share `TaskDetailForm`'s draft reset**, which fires on
  `[task.id, task.updated_at]`. `updated_at` bumps on every patch — including
  the ones a background run writes back — so folding the collapse into it shuts
  the console while you are using it, on a timer you don't control.
* **`useLayoutEffect`, not `useEffect`.** The reset runs on mount, so with a
  passive effect a task opened while the previous one's console was expanded
  paints the rail for one frame before collapsing it.

**Two smaller things the same rail change surfaced.** The **availability warning
must live outside the editor it belongs to**: it was rendered under the runner
picker, and once that picker sat behind a closed panel, a Codex task on a machine
without Codex looked completely fine until the queued run failed — the exact
"discovered after you've already decided" failure the probe exists to prevent. It
is now rendered at rail level from `useExecutorAvailability`, with
`BackgroundAgentPicker`'s copy suppressed by `embedded` so one fact renders once.
And the **rail's connector line must go when the rail wraps**: its `12.5%` /
`62.5%` offsets are the centres of four columns, so under the 300px container
query (where the grid becomes 2×2) the line drew straight through the middle of
the grid. Production's console bottoms out at ~299px — 340px pane minus padding
— so that breakpoint is reachable at the narrowest pane and is not dead CSS.


### 28. Agent prose renders through ONE markdown surface, and HTML enters it only through the sanitize allowlist
Comment bodies and run results are both markdown now
(`components/shared/Markdown.tsx`, react-markdown + remark-gfm). Before, the
pane printed `c.body` in a `whitespace-pre-wrap` paragraph with no markdown
pass, so `**Result**` showed its asterisks — while `handoff_tail` was
simultaneously instructing read-only runs to "write it as markdown". The prompt
was asking for something the pane could not display; that is what got fixed.

**Both callers go through the one component on purpose.** `finish_run` writes a
short summary to the comment log and the full handoff to the run row, and the
two are read side by side in the same pane — rendering them differently would
be visible immediately.

**Notes is the third caller, and it is the one you can edit** (`NotesField`).
Same surface, same reasons — a task's notes arrive from `file_task` and
`/cxtasks:breakdown` written in markdown, so the field spent its life printing
`##` and pipe-tables verbatim. Three decisions specific to it being editable:
* **Escape COMMITS rather than discarding**, unlike the blocked-reason field
  where Escape abandons a staged status. Notes is a page of prose and there is
  no discard on the mouse path to be consistent with — clicking away saves. A
  keystroke that threw a note away because it *usually* means "cancel" is
  gotcha #16's lesson wearing a new hat.
* **`leave()` commits, not the textarea's `onBlur` alone** — gotcha #24 applies
  in full, because this textarea is conditionally rendered by construction.
* The focus ring moves to the wrapper via `.cx-notes`, the filter bar's trick
  and for its exact reason — including that `outline-none` as a Tailwind
  utility does **not** suppress the inner ring, since the generic
  `textarea:focus-visible` rule is unlayered and beats every layered utility.

Three things in there that look like polish and aren't:
* **Embedded HTML mounts through a SANITIZE allowlist, never raw** (since
  2026-09-22). Before that `rehype-raw` was absent and all HTML was escaped to
  text — the right default for prose an *unattended* agent writes against a
  repo's contents, where a `<script>` or `<img onerror=…>` reaching the DOM has
  no human in the loop. It was retired because Chris reads Onyx artifacts whose
  diagrams are inline SVG, and a note pasted from one rendered as angle
  brackets. So `rehype-raw` is now paired with `rehype-sanitize` over
  `lib/markdownSchema.ts`: GitHub's allowlist widened to inline SVG, `figure`,
  `details`, `mark`, `class` and `style` **attributes**; `script`, `iframe`,
  `object`, `form`, `<style>` **elements**, `foreignObject`, `use`, every `on*`
  handler and every `javascript:`/`data:` URL are dropped. Two decisions inside
  the schema that a tidy-up would undo: `clobber` is **off**, because
  prefixing ids breaks `marker-end="url(#a)"` in every diagram and the app
  addresses nothing by id; and `style` attributes are allowed while `<style>`
  elements are stripped, because an attribute cannot run code or reach outside
  the note and a selector can. **Widen by adding to the schema; never by
  removing the sanitize step.** The regression test lists what must never mount
  and checks `querySelector`, not `innerHTML` text.
  The same file maps the artifact kit's colour tokens (`--clay`, `--ivory`…)
  onto the app theme under `.cx-prose`, so a pasted diagram paints in the
  pane's palette in both themes. The TAG and ATTRIBUTE lists are not in the
  schema file: they are `crates/cxtasks-core/src/markdown_allowlist.json`,
  shared with the MCP's write-time render check (`mcp-plugin.md` § Writing
  prose that renders), so what an agent is warned about and what the pane
  drops are one list. The schema keeps rehype-sanitize's value constraints on
  top, and `markdown.test.tsx` fails if the JSON falls behind its defaults.
* **Notes reflows a wall of prose at RENDER time** (`Markdown`'s `reflow` prop,
  `lib/prose.ts`) and treats a single Return as a line break (`breaks`,
  remark-breaks). A long, single-line, unstructured paragraph is split at
  sentence boundaries and regrouped; the stored body is untouched, and
  clicking in shows the text as saved. It never touches a paragraph the author
  broke into lines, a list, a fence, a table or HTML. Neither prop is on for
  comments or run results — that is agent markdown with its own paragraphing,
  and hard-wrapped handoffs would sprout a `<br>` per line.
* **Links must never navigate.** A bare `<a>` in a WKWebView replaces the app
  with the page and offers no way back — recovery is a relaunch. Every link
  `preventDefault()`s and goes to `lib/openUrl.ts`, which allow-lists
  `http/https/mailto`; `shell:allow-open` would otherwise hand `file://` to
  `open(1)`. It also swallows its own errors, because `invoke` throws
  synchronously with no IPC bridge (gotcha #8's lesson, in a click handler).
* **Inline vs fenced `code` is settled by a CSS reset**, not by sniffing the
  `language-*` className. A bare ``` fence produces a `code` with no className,
  so className-sniffing mis-styles exactly the fences agents write most.

### 29. The leading box CLOSES the task; the ROW selects. And ⌘Z outlives its toast
Two halves of one change, and each one is load-bearing for the other.

**The shape is the whole signal.** A circle in the leading slot is macOS's
multi-select affordance — Finder, Photos, Mail — so a *circle* that completed
work read as "pick a few of these" and quietly closed rows instead. The fix at
the time was to make the circle select and move completion to a hover-only
button on the trailing edge, which cured the misfire and cost the thing a todo
list is actually for: closing a task is THE verb, and it ended up behind a
hover at the far edge of the row. A **square** is a checkbox everywhere on this
platform (Things 3 included) and needs no caption to say so, which frees the
whole row for the gesture the circle was being mistaken for. Don't "restore
consistency" by rounding it — the row would immediately start closing tasks by
accident again, and the mistake is invisible until you notice work has gone.

Four things follow, and three of them fail silently:
* **The box must `stopPropagation`.** It sits inside the row, and the row is now
  the selection control — without it, ticking one row off a set of six rewrites
  the set to a single row on the way past.
* **`aria-selected` has to cover `checked`, not just `selected`.** The row
  highlight is the only remaining mark of membership; there is no tick box left
  to read. Reporting only the focused row describes a six-row selection as one.
* **⌘-click seeds from the focused row.** Otherwise the first ⌘-click starts
  from an empty set, so clicking A then ⌘-clicking B selects only B — the row
  already highlighted drops out of a gesture whose entire meaning is "and this
  one too". Same seeding for ⇧-click, which otherwise has no anchor.
* **Checked and selected must look different.** Same treatment for both and
  "one row is selected" is indistinguishable from "one row of this set is the
  one you're reading", with the bulk bar the only thing left saying which.
* **The rows must be `select-none`.** ⇧-click is *both* the range gesture and
  WebKit's "extend the document selection to here", so picking six rows also
  painted `::selection` across every title, tag and chip between them — a
  second, brighter highlight fighting the real one. There is no text in a row
  worth copying; the title is an editable field in the detail pane.

**A drag carries the SELECTION, not the row under the cursor** — the other half
of "the row is the selection control". `TaskList`'s `moving` is the checked set
when the grabbed row is part of it and `[dragId]` when it isn't; a row outside
the set drags alone and is never adopted into it, which is how you move one task
without first tearing down a selection you still want. Four things hang off it:
* **The `Group` descriptor takes a LIST** (`onDropTasks(ids)`), so no drop
  target can quietly act on one row of six. Sections and projects are the same
  descriptor — a drop that moved a set into a section but a single row into a
  project would be one gesture meaning two things depending on a toggle two
  panes away.
* **One drop is ONE undo entry** (`moveTasksToSection` / `setTasksProject`, not
  the single-task actions in a loop), or taking back one drag costs N ⌘Zs with
  the board half-moved in between. `setTasksProject` diffs what the server
  *returned*, per row, for the `repo_path` reason gotcha #29's `inversePatch`
  note gives.
* **`runOnMany` subtracts the ids it consumed** rather than clearing the set
  outright. Every bulk-bar verb passes the whole checked set, so the outcome is
  unchanged there — but a drag of an unchecked row would otherwise wipe a
  selection the gesture never touched.
* **Every moving row dims, and only in the grouped path.** The dimming is the
  only thing on screen promising six rows will move. The ungrouped path is a
  *reorder*, which stays single-row: a reorder of a non-contiguous set has no
  honest answer to where the other five land, so it dims one row to say so.

**⌘Z is a stack, and the toast is not its lifetime.** `taskStore.undoStack`
holds up to 25 entries, each carrying its inverse as a *closure over the state
captured before the action ran*. The toast offers the newest for 8s; the entry
survives it. Tying the two together — which is what the old single armed
`undo` did — meant the recovery expired at roughly the moment it takes to
notice you hit the wrong row, and only a delete was reversible at all.

Five rules in there that a refactor breaks quietly:
1. **An inverse must not record an inverse.** `pushUndo` no-ops while
   `undoLast` is running, or a second ⌘Z is a redo — which is exactly what a
   failed undo looks like from the outside.
2. **The inverse is a closure, not a diff.** Completing a *recurring* task also
   materializes its next instance, so undoing it has to trash that instance
   too; a bulk action is N calls whose before-states differ per row. Neither is
   expressible as "reverse this patch".
3. **`inversePatch` diffs the task the SERVER returned against the one before
   it** — never the keys the caller sent. It restores only the fields that
   actually moved, and returns null when none did (spreading the whole prior
   task back would clobber edits made in between and anything the runner wrote;
   recording no-op entries makes ⌘Z appear dead for several presses, since
   commit-on-blur fields re-send values they already hold).

   **Keying off the applied patch was a real bug, and its victim was not the
   obvious one.** `db::tasks::update` writes fields nobody asked it to, so an
   inverse built from the sent keys is blind to every one of them. The
   reachable case is `project` → `repo_path`: moving a task between projects
   re-points the workspace (invariant 17's prefill), and undoing restored the
   project name while leaving the repo pointing into the project you just left
   — recreating the exact stale-workspace dispatch footgun the prefill exists
   to prevent, with the launch console showing the wrong repo as though you had
   chosen it. `bg_executor` → `model` (invariant 15) is the same shape and was
   the one first noticed, but it is nearly unreachable: nothing in the UI sets a
   per-task model. `reminded_at` moves too and is deliberately NOT restored —
   it isn't patchable, and re-arming a reminder is the safe direction to fail in.

   Two things hold this up. `UNDOABLE_FIELDS` must cover every `TaskPatch` key,
   which is a **compile-time** assertion rather than a test, because a missing
   key is silent — that field just stops being undoable with every test green.
   And the inverse sends `project` and `repo_path` **together**, which only
   works because `update` pushes the caller's `repo_path` AFTER the prefill's
   and SQLite takes the last assignment; reorder those two and undo silently
   snaps tasks onto whichever repo the project calls primary
   (`an_explicit_repo_in_the_same_patch_beats_the_project_prefill`).
4. **A hard delete prunes the entries naming it** (`dropUndoFor`), and
   `empty_trash` clears the stack outright — it destroys a set the caller never
   enumerated, so there is no honest way to say which entries still work.
5. **The native right-click menu needs its own wiring.** It mutates the DB in
   Rust and tells the window afterwards, so the store never sees the action and
   can't capture a before-state. `handle_task_menu_action` emits
   `task-undoable` with the row as it stood, and `pushMenuUndo` rebuilds the
   inverse — including reading `before.status` to tell a *complete* from a
   *reopen*, since that menu item is one toggle. Fixing only the in-app path
   leaves ⌘Z working for everything except the menu, silently. Same lesson as
   gotcha #18, in the same file.

⌘Z stays **below** the `typing` guard in `useKeyboardShortcuts` for the reason
gotcha #16 gives about ⌘⌫: inside a text field it is the field's own undo, and
taking that over would revert a whole task for a mistyped character.

### 44. An UNLAYERED rule beats every Tailwind utility — `text-*` on a `<button>` was inert
**Symptom:** a `<button className="text-content-secondary">` renders at full
content brightness. The class is on the element, `.text-content-secondary`
exists in the stylesheet with the right value, nothing errors, and the computed
`color` is the INHERITED one. Seventeen buttons in this app were doing it.

**Cause:** `globals.css` set `color: inherit` on `input, textarea, select,
button` **outside any `@layer`**. Tailwind v4 puts utilities in
`@layer utilities`, and an unlayered rule outranks every layered one regardless
of specificity — so the reset won every time.

**Fix (2026-08-25):** the line is gone. Preflight already sets `color: inherit`
on form controls inside `@layer base`, where a utility can beat it, so a button
with no colour class is unchanged and one with a class finally works. The two
remaining lines in that rule (`font-family`, `font-size`) are the ones this app
was actually burned by and stay.

**How to check it, in one line in the browser:**
```js
[...document.querySelectorAll('button')]
  .filter(b => /text-(content|accent|error|success|warning)/.test(b.className))
  .map(b => [b.className, getComputedStyle(b).color])
```
**The general rule:** anything unlayered in `globals.css` silently outranks
every utility. Put resets in `@layer base` or leave them to Preflight, and be
suspicious of an unlayered element selector that sets a property a utility also
sets — the failure is invisible, and `revert-layer` from a
higher-specificity unlayered rule is a workaround, never the fix.
