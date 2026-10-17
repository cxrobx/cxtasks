---
paths:
  - "src-tauri/src/lib.rs"
  - "src-tauri/src/notify_macos.rs"
  - "src-tauri/src/appearance_macos.rs"
  - "src-tauri/src/vibrancy_macos.rs"
  - "src-tauri/Info.plist"
  - "src-tauri/tauri.conf.json"
  - "src-tauri/capabilities/**"
---

# macOS Native Layer & Shipping Rules

Signing/shipping detail plus every native-layer gotcha. Invariant and gotcha
numbers are **global across the rules files** — the index in `CLAUDE.md` maps
every number to its file. Gotcha #15 (window dragging: `data-tauri-drag-region`
markup AND the `core:window:allow-start-dragging` capability grant) lives in
`frontend.md` — its capabilities half included.

## Shipping — why each step exists

The recipe itself lives in `CLAUDE.md` → "Shipping a build". The reasoning:

**The re-sign is required even when you copy nothing.** As of Tauri CLI 2.10.1
`tauri build` signs with **hardened runtime** (`flags=0x10000`), and CXTasks
grants no `com.apple.security.automation.apple-events` entitlement — so a
hardened-runtime build silently loses the Ghostty AppleScript route and spawns a
fresh Ghostty *app* on every dispatch (gotcha #25). Re-signing without
`--options runtime` strips it back to `0x0`. That is what this step has always
been doing; it merely used to look like a consequence of the copy. **Verify the
flag rather than the command** — the two signings differ by a flag, not by
anything visible in the bundle.

**There is no `cp` of the two extra binaries any more, and there must not be
one.** They arrive through `externalBin`, staged into `src-tauri/binaries/` by
`scripts/build-helpers.sh` (run from `beforeBuildCommand`). If a helper is
missing from the bundle, the STAGING failed — fix that rather than hand-copying,
which invalidates the signature and hides the real fault.

Until the crate split, the Tauri CLI bundled both helpers by itself, purely
because they were extra `[[bin]]` targets in the app's own package — undocumented
and never requested. Moving them into `cxtasks-core` so they could build for
Linux stopped it, **and the way it stopped is the lesson**: the build printed
`resource path ... doesn't exist`, exited 0, and produced an app containing only
`cxtasks`. A missing sidecar is a warning, not an error. Check the bundle.

Installing the bundle **before** the build finishes installs the *previous*
build — which presents as "the migration didn't run" (check `schema_version` in
the DB against `CURRENT_VERSION`). Has happened, as has shipping a stale
runner/MCP.


## Gotchas (native)

### 4. `UNUserNotificationCenter` **terminates** any process that isn't in an .app bundle
**Symptom:** `cxtasks-runner` died mid-batch (`libc++abi: terminating due to
uncaught exception of type NSException`), leaving an in-flight run stuck at
`running` and holding a concurrency slot forever. Separately, `npx tauri dev`
crashed at launch with `NSInternalInconsistencyException: bundleProxyForCurrentProcess
is nil`.
**Cause:** `+[UNUserNotificationCenter currentNotificationCenter]` requires a
bundle identifier. From a bare binary — the runner, and `target/debug/cxtasks`
under `tauri dev` — it raises an ObjC exception, and an uncaught ObjC exception
calls `std::terminate`. It does not fail the notification; it aborts the
process. **`catch_unwind` does not help** — it's not a Rust panic.
**Fix:** `notify_macos::inside_app_bundle()` gates `init`, and
`show_notification` now routes on something stricter — see gotcha #34. Any new
ObjC call in a path the runner can reach needs the same guard.
**Related:** the runner still prefers to hand a finished run to the app over the
bridge (`notify_delivered` reports whether anyone was listening), but that is
now about WORDING, not about identity: the app can phrase the banner around
what is already on screen. Either way the banner is posted by a process macOS
recognises as CXTasks.
**The stuck-run half of that symptom is now recovered rather than permanent** —
`db::runs::sweep_abandoned` reclaims the slot on the next `initialize` or
`runner::tick` (invariant 20). Two things that still need saying: the guard
above is the actual fix, since a process that doesn't die strands nothing; and
the sweep is not a licence to let ObjC exceptions reach the runner, because a
run killed this way is recorded as `abandoned` with its work unreported.

### 5. All three binaries must be in the bundle — but the re-sign is NOT about the copy
`launchd.rs` resolves `cxtasks-runner` as a sibling of the running executable,
and the `cxtasks` MCP registration points into the installed bundle (gotcha #3).
If either is missing, "Enable the background runner" fails with "cxtasks-runner
not found" and the MCP tools stay at whatever the last install shipped.

**The two helpers now arrive via `externalBin`, and that is deliberate.** They
used to be bundled by the CLI as a side effect of living in the app's package;
moving them to `cxtasks-core` (so they build for Linux) silently ended that, and
the build still exited 0 with a one-line warning. `externalBin` is the supported
mechanism and does not depend on the two crates staying married.

Two traps in wiring it, both of which cost a build cycle here:
* **`externalBin` paths are resolved BEFORE `beforeBundleCommand` runs**, so
  staging the binaries there is too late.
* **`beforeBuildCommand` runs from the directory holding `package.json`** — the
  repo root, not `src-tauri/`. A `../scripts/...` path escapes the repo, dies
  with exit 127, and the build reports the failure while still exiting 0.

**The re-sign survives that change, for an unrelated reason:** it strips the
hardened runtime `tauri build` now applies, without which the Ghostty
AppleScript route dies silently (CLAUDE.md → "Shipping a build", gotcha #25). It must use the
**Developer ID**, never ad-hoc `--sign -`, or you lose the notification grant on
every build (gotcha #7). Sign the bundle in `target/release/bundle/macos/`
before copying it to `/Applications`, not after.

**The trap this leaves behind** is that the recipe now *looks* like it does
nothing — same command, no copy, and the bundle already contains everything. The
only evidence it mattered is `codesign -dv` reading `flags=0x0` instead of
`flags=0x10000`, which is why that check is in the recipe rather than left to
memory.


### 7. Notification authorization is silent when denied — log it
`show_notification` succeeds whether or not the user granted permission, so
"reminders don't fire" and "permission was never granted" look identical. The
`requestAuthorizationWithOptions` completion handler now logs GRANTED/DENIED;
that one line is what turns this into a one-second diagnosis.

Two related traps:
* **`UNNotificationPresentationOptions` are NOT the same bits as
  `UNAuthorizationOptions`.** Authorization: badge 1, sound 2, alert 4.
  Presentation: badge 1, sound 2, **alert 4 is deprecated on macOS 11+**, list
  8, banner 16. Passing the old alert bit shows nothing while the app is
  frontmost — the scan logs "fired 1 due reminder(s)" and no banner appears.
* **Sign with the Developer ID, not ad-hoc.** Ad-hoc signing gives the bundle a
  new cdhash on every build, so macOS treats each install as a different app
  and drops the notification grant — you re-authorize forever. `tauri.conf.json`
  sets `bundle.macOS.signingIdentity`; after copying the extra binaries in, re-sign
  with the same identity (`codesign --force --deep --sign "Developer ID Application: …"`).
  Same lesson as DiskSight's Full Disk Access in `~/.claude/docs/disk-cleanup.md`.

### 8. Native chrome follows the SYSTEM appearance, not your CSS theme
**Symptom:** a light-grey `NSMenu` (or `<select>` popup, sheet, scrollbar)
appearing out of the dark window — on a Mac set to Light mode.
**Cause:** AppKit draws native chrome from `NSApp.effectiveAppearance`, which
follows System Settings. Nothing in the web layer can influence it.
**Fix:** `appearance_macos::set_appearance(dark)` pins `NSApp.appearance` to
`NSAppearanceNameDarkAqua`/`NSAppearanceNameAqua`. Verify from the log line,
which reads `effectiveAppearance` back rather than echoing what was requested.
**This was the root cause of two symptoms that looked unrelated** — the
`<select>` popup rendering light grey, and the right-click menu rendering light.

**Since the theme became user-selectable this is a toggle, not a constant, and
the theme must be set in TWO places on every change or the app is visibly
half-converted.** `uiStore::applyTheme` is the single funnel that does both:
the `data-theme` attribute (CSS variables) *and* `set_native_appearance`
(AppKit). Route every theme change through it — setting only the attribute
reproduces this exact gotcha inverted, as a light UI with a dark context menu.
Three further traps in that path:
* **The command must hop to the main thread.** `-[NSApplication setAppearance:]`
  is main-thread-only (cxmail gotcha #20) and `#[tauri::command]` bodies run on
  Tokio workers, so `commands::appearance` goes through
  `AppHandle::run_on_main_thread`. Calling the setter directly from the command
  is a threading violation.
* **Rust cannot read the saved theme at startup.** It lives in the frontend's
  `localStorage`, so `.setup()` sets dark unconditionally to match
  `tauri.conf.json`'s window `"theme": "Dark"`, and `main.tsx` corrects it on
  hydrate. Don't "fix" the redundant-looking startup call — removing it means a
  dark-theme user gets whatever the *system* is until the frontend loads.
* **`applyTheme` runs before `createRoot`, so it must not throw.** `invoke`
  throws *synchronously* when the Tauri IPC bridge isn't on `window` (any plain
  `vite dev` tab), which a bare `.catch()` sails past — hence the `try` around
  it too. Without that, browser-based UI validation is a white screen.

### 9. Right-click menus are RENDERED, not `NSMenu` — and that is what makes ⌘Z work
`main.tsx` still suppresses WKWebView's default context menu (the developer
"Reload / Inspect Element" one) everywhere except editable fields, where the
native cut/copy/paste/spelling menu is wanted. Everything past that is React:
`components/shared/Menu.tsx` holds the styled Radix primitives,
`TaskContextMenu` the row menu, and `ListMenu` the sidebar's two.

**This used to be a real `NSMenu`, and the argument for it was the same one that
still holds for the Select popup (gotcha #6): a web-rendered menu reads as
foreign next to the rest of macOS.** What changed is that cxmail built a matched
dark menu, so "foreign" now means *unlike the app beside it* rather than *unlike
AppKit* — and the native route was carrying a real cost:

- **Undo had to be faked.** The menu mutated the DB in Rust and told the window
  afterwards, so the store never ran the action and could not record an inverse.
  Rust snapshotted every task before mutating and shipped the before-state back
  over a `task-undoable` event, purely so ⌘Z would work there as it does
  everywhere else. Rendered items call the same store actions the buttons call,
  so undo is just how the store already behaves. That whole bridge is gone —
  `pushMenuUndo`, `MenuUndoEvent`, the listener and its four tests with it.
- **Half the items could not do their own job.** An `NSMenuItem` has no text
  field and cannot raise a confirm, so rename, both deletes and the repo sheet
  each emitted an intent that a `listen()` in `Sidebar` translated back. Four
  round trips for four menu items.
- **`queue_background`'s result was unreachable**, so the one route where
  queueing into a dead runner looks identical to queueing into a live one had to
  be told about it by a second event (`background-idle-warning`).

What was actually given up: system accent colour, menu-key navigation, and
AppKit's open/close animation. Worth knowing before anyone proposes going back.

**The trap when wiring one up:** the row sits INSIDE Radix's trigger, so a
`stopPropagation()` in the row's own `onContextMenu` — which is what the native
version needed, to keep the event off ancestors — stops the event reaching the
trigger and the menu never opens at all. Anything the row needs to do first
(taking the selection, so the menu acts on what was clicked) belongs on the
menu's `onOpenChange`, not on the row.


### 11. The bridge socket must be bound inside the Tokio runtime
`UnixListener::bind` registers the fd with the reactor, so binding directly in
Tauri's `.setup()` (main thread, outside any runtime) panics with "there is no reactor
running" — and because setup runs inside the ObjC `did_finish_launching` callback,
that panic can't unwind and aborts the app. Bind inside the spawned task.


### 17. `dialog:default` does NOT grant `confirm`, whatever its description says
**Symptom:** `confirm()` from `@tauri-apps/plugin-dialog` rejects at runtime
with a permissions error, even though `capabilities/default.json` lists
`dialog:default` and the plugin's own default-permission description reads
"All dialog types are enabled."
**Cause:** it isn't true. The plugin's `permissions/default.toml` actually
grants `["allow-message", "allow-save", "allow-open"]` — `allow-confirm` and
`allow-ask` are both omitted. The prose and the list disagree, and the list is
what's enforced.
**Fix:** `"dialog:allow-confirm"` is listed explicitly alongside
`dialog:default`. Anything reaching for `ask()` needs `dialog:allow-ask` too.
**Worth remembering generally:** read the plugin's `permissions/*.toml` in
`~/.cargo/registry` rather than trusting a permission set's description — this
one fails only when the dialog is actually opened, so a build and a launch both
look fine.


### 22. Window transparency is NATIVE — `backdrop-filter` cannot see past the window
**Symptom:** the window is transparent, `backdrop-filter: blur()` is on the root
element, and the desktop shows through perfectly **sharp**. The filter looks
correct and is doing nothing, so it reads as a value that needs tuning.
**Cause:** `backdrop-filter` blurs an element's *backdrop root*, which for
anything inside the page is the page. It has no access to what is behind the
**window**, so on a transparent window it blurs a transparent nothing —
correctly applied, to zero pixels. Same class of mistake as gotcha #15's
`-webkit-app-region`: an incantation that belongs to a different layer.
**Fix:** three things, and all three are required — any one missing looks like
one of the other two is broken.
1. `"transparent": true` on the window in `tauri.conf.json` (needs
   `macOSPrivateApi`, which was already on).
2. A blur behind the webview — `glass_macos::enable`, which calls the private
   WindowServer symbol `CGSSetWindowBackgroundBlurRadius` through `dlsym`.
   **Not `NSVisualEffectView` any more, and the difference is the whole look.**
   A visual-effect view is a *material*: Apple's radius, Apple's tint and
   Apple's saturation boost welded into one enum case with no dial on any of
   it, and in dark mode `UnderWindowBackground` reads as flat milky grey that
   destroys both the colour and the shape of the wallpaper behind it. The CGS
   call is a plain Gaussian blur at a radius we choose, with no material on
   top — so the wallpaper stays recognisably itself and our own `--alpha-*`
   tint is the only thing colouring it. `vibrancy_macos` survives as the
   fallback for a macOS that stops exporting the symbol; see gotcha #47.
3. `body { background: transparent }`. An opaque body paints straight over the
   effect view, and the blur is then present, correct, and completely invisible.

**The tint is the frontend's half, and the two must agree.** `--alpha-pane` /
`--alpha-sidebar` / `--alpha-surface`, written as INLINE styles on `<html>` by
`uiStore::applyWindow` from one continuous slider value. Blur with no tint is an
unreadable smear; tint with no blur is a flat panel with a legible desktop
behind it.

**No pane may sit on top of another pane's background.** This is the trap that
cost the most here, because the CSS and the arithmetic were both already
correct. `AppLayout`'s root element carried `bg-base`, painting one opaque layer
across the whole window — so the sidebar's alpha composited over THAT rather
than over the vibrancy, and the sidebar could never be more transparent than the
content, only more coloured than it. Turning the sidebar's alpha down moved it
toward the content's colour instead of toward the desktop, so "much more
transparent" rendered as "slightly less contrast" and read as the setting doing
nothing at all. It came out DARKER than the content when the whole point was
lighter. Each pane paints its own background and the root paints none; the gaps
between panes are meant to be blurred desktop. Check this first whenever a
transparency change has no visible effect.

**The same rule from the other side: the background belongs to the pane's OWN
element, not to a child of it.** The detail column painted `bg-surface` on
`TaskDetail`'s scroller, which is a SIBLING of the traffic-light headroom rather
than its parent — so the top ~28px of the pane was raw blurred desktop with the
traffic lights floating on it, and the "No task selected" branch (which carries
no background at all) went fully transparent whenever nothing was selected.
`Sidebar` had it right all along: `bg-sidebar` sits on the `<nav>` that contains
its headroom. The tell is that the gap looks like a *transparency* bug — it goes
away at slider 0, because at slider 0 everything is opaque anyway.

**The sidebar runs on its own curve, not a fraction of the content's.** Codex's
bar reads lighter than its content because it is much *thinner*, not because it
is painted lighter — so `transparencyToAlphas` boosts the slider before applying
it to the sidebar and gives it a much lower floor. Boost the TRANSPARENCY, never
scale the resulting alpha: `pane * 0.5` leaves the sidebar half-transparent even
with the slider at zero, so the opaque end of the range wouldn't be opaque.

**Which tokens carry alpha is a decision, not a detail.** `base`, `sidebar` and
`surface` are panes and are alpha-aware; `elevated` and `input` are deliberately
**opaque**. Popovers and menus portal to the document root, so their backdrop is
the transparent window itself — a translucent menu at Glass is text floating on
raw wallpaper with nothing behind it. `SettingsDialog` is opaque for the same
reason, and does not dim the app behind it: every control in it changes how the
window looks, so a normal scrim would black out the exact surface being
adjusted.

Applied **once**, at setup, and never re-applied on theme change — the material
renders against the window's `NSAppearance`, which `appearance_macos` already
pins to the UI theme, so the theme toggle moves it for free. Re-applying per
change stacks a second effect view behind the first. It is also not cleared for
Solid: at `--alpha-pane: 1` the panes cover it completely, so leaving it
installed makes level switching pure CSS with no native round-trip, and no
failed IPC call can strand the window half-transparent.

Ordering: apply it **after** `appearance_macos::set_appearance` in `.setup()`,
or it blurs against the system appearance until something repaints it. Both are
main-thread-only, which `.setup()` already is (cxmail gotcha #20).

**Verifying this needs the real window.** Playwright renders the webview only,
so a transparent window screenshots as flat black there and proves nothing — a
`tauri dev` window plus `screencapture` is the only honest check. Two traps in
doing that: sample a region you have confirmed by a pixel scan (a patch that
catches the window border reads as the wrong surface entirely, which cost a
wrong conclusion here), and **wait out the `transition-colors duration-75` on
the nav rows** before capturing after a theme toggle — capture inside it and the
old theme's text color is still mid-transition over the new theme's background,
which looks exactly like a broken palette.


### 26. `set_focus` is a NO-OP on a minimized window, and three other ways a notification click dies
Clicking a banner has to survive four separate failure points, and for a long
time it survived none of them. Every one fails **silently**, and the combined
symptom is a click that does absolutely nothing.

**`window.set_focus()` does nothing while the window is minimized or hidden.**
tao guards it with `if !is_minimized && is_visible`
(`tao/src/platform_impl/macos/window.rs`), so for a task app that lives
minimized — which is most of the time — the click was discarded by the one call
meant to honour it. Undo every kind of hidden FIRST and ask for focus LAST:
`AppHandle::show()` (app-wide ⌘H — and while the app is hidden its windows
aren't "visible", so every per-window call below it no-ops too), then
`unminimize()`, then `show()`, then `set_focus()`. Calling these from the UN
delegate is safe despite being main-thread AppKit work:
`tauri-runtime-wry::send_user_message` compares against `main_thread_id` and
handles the message inline rather than posting and blocking, so there is no
deadlock — read in the registry source rather than assumed.

**Selecting a task is not showing it.** `TaskDetail` resolves its task out of
`tasks`, the rows the *current view* loaded, so `select`ing one that view
doesn't hold sets `selectedId` to an id nothing renders and leaves the pane
blank. A finished background run lands in `in_review`; clicking its banner from
Today looked completely inert. `taskStore.reveal` is the entry point for
anything arriving from **outside** the list: it fetches by id (`get` is
unfiltered, which is exactly what makes a trashed or archived task revealable),
and only if the task is missing from the current list moves to a view that holds
it — Trash before Archive, matching the backend, then Done, else All — through
`setView`, which also drops the query and tag filters that would re-hide it.
Already on screen → nothing moves, because yanking someone out of their view to
show them a row they can already see is its own bug. The list scrolls on
`revealTick`, never on `selectedId`: scrolling on every selection would drag the
viewport around under anyone clicking a row that sat half off-screen.

**A due SUBTASK fires a reminder and exists in no view.** `list_due_for_reminder`
has no `parent_id` filter while `list_tasks` defaults `include_subtasks` to
false, so that notification could never resolve to a row at all. `reveal` hops to
the parent — the row that exists, and whose pane lists the subtask. The same hop
is what rescues a trashed child, since the Trash lists batch roots only.

**A cold launch drops the event.** Clicking while the app is CLOSED launches it
and Rust emits from `.setup()`, before React has mounted a listener — losing
precisely the click the user cared most about. So the id travels in a latch
(`notify_macos::PENDING_TASK`) and the event is a bare **ping** carrying no
payload: one source of truth, drained by both triggers — the event handler (fast
path, app already running) and a mount-time call (cold launch). Don't "simplify"
by putting the id back on the event. Two sources means only one path drains the
latch, and the stale id sits there waiting to yank the user to a task they dealt
with days ago.

**Verifying this needs the installed bundle.** `tauri dev` runs the bare binary,
where `init` skips UNUserNotificationCenter entirely (gotcha #4). Its banners now
carry the right name and icon — they are re-posted through the installed app
(gotcha #34) — but a click on one activates the INSTALLED app, not the dev build
that raised it, so **click routing still cannot be tested in dev**. Build →
re-sign → install, then fire a real reminder. Two traps in scripting the click:
the banner auto-dismisses in ~5s, and Notification Center's history window
vanishes between `osascript` invocations, so an enumerate-then-click across two
calls always finds zero windows.

### 34. Only the bundle's MAIN executable is "the app" — a helper beside it is not
**Symptom:** two failures that looked unrelated. A background run that finished
while the app was **closed** put no banner on screen at all. And a `tauri dev`
build's banners arrived branded **Script Editor**, wearing its pencil icon.
**Cause:** macOS grants notification permission to an *app*, and an app is its
bundle's `CFBundleExecutable`. `cxtasks-runner` lives in the same
`Contents/MacOS` directory, resolves the same `com.cxtasks.app` bundle
identifier, and is still refused: `getNotificationSettings` reports
`authorizationStatus: 0` (**notDetermined**) for it, `requestAuthorization`
answers `granted: false`, and every post fails with `UNErrorDomain error 1`
(*notifications not allowed*). Confirmed against a helper that adopted
`NSApplicationActivationPolicy` `.accessory` and `.regular` — the activation
policy changes nothing, because the identity is the problem. The runner's
completion block then swallowed the error, so the whole failure had no symptom
beyond a missing banner, which reads as "the run didn't finish". Meanwhile a
process with no bundle at all could only reach `osascript`, and macOS attributes
those banners to the scripting host that posted them.
**Fix:** `notify_macos::show_notification` routes on
`is_the_running_app()` — `APP_HANDLE` set (only `init`, only from the Tauri
app's `.setup()`) **and** inside a bundle. That process posts directly. Everyone
else — the LaunchAgent runner, every dev build, anything run out of
`target/release` — runs `CXTasks.app/Contents/MacOS/cxtasks --notify`, handled in
`main.rs` **before** `run()` so it never opens the DB, binds the bridge, or puts
a second window on screen. It posts and exits inside a second. `osascript`
survives only as the last resort for a machine where CXTasks isn't installed.
**Also:** authorization is per-PROCESS. Even the right executable is refused
until it has called `requestAuthorizationWithOptions` in that process, which is
what `ensure_authorized` does (memoized, blocking — deliberately not shared with
`init`, which must not block `.setup()` behind a permission dialog).
**Testing it:** `--notify` is a real command line. Run it by hand and read the
exit status — 0 posted, 1 means the caller falls back.

### 35. RETIRED — the embedded `__info_plist` trap is gone by construction
**What it was:** `tauri build` and a bare `cargo build --release` did not produce
the same binary. The cargo one carried an embedded `__TEXT,__info_plist`
section; the bundled one did not. A helper declaring its own Info.plist *inside*
another bundle gave macOS two answers about who it was, and it returned neither
— so `UNUserNotificationCenter` raised `NSInternalInconsistencyException` and
the process died (gotcha #4). It survived `lsregister -f`, reinstalling, and
re-signing.

**Why it is gone:** the section came from `tauri-build`'s link arguments, which
applied to **every** `[[bin]]` in the `src-tauri` package — including the two
helpers. Those helpers are built by `cxtasks-core` now, which has no build
script, so a plain `cargo build --release` produces them with zero
`__info_plist`. The combination that poisoned a bundle — build a helper with
cargo, copy it in — is no longer reachable, and the golden command
`cargo build --release --bin cxtasks-mcp` is safe again.

**What still holds:** binaries in the bundle come from `npm run tauri build`,
which stages them through `scripts/build-helpers.sh`. That script asserts
`otool -l <binary> | grep -c __info_plist` is **0** and refuses otherwise — kept
deliberately, because the day anything gives `cxtasks-core` a build script this
would come back silently and the bundle would ship looking perfectly fine.

### 43. A rebuilt `target/release/cxtasks-server` is killed at spawn by launchd — re-sign it
**Symptom:** after `cargo build --release --bin cxtasks-server` and
`launchctl kickstart -k gui/$UID/com.cxtasks.app.server`, the node never
answers. `launchctl print gui/$UID/com.cxtasks.app.server` shows
`state = spawn scheduled` and `last exit reason = OS_REASON_CODESIGNING`, with a
crash report in `~/Library/Logs/DiagnosticReports/` per attempt. Two things
make it look like anything but a signature: `codesign --verify --strict` says
**valid on disk**, and the binary runs perfectly from a shell. And the newest
error in `server.err.log` is an old `could not bind 100.84.253.20:8787` from a
day Tailscale was down, which sends you to the network layer.

**Cause:** cargo's release binary carries only the linker's ad-hoc signature —
`codesign -dv` reads `flags=0x20002(adhoc,linker-signed)`. A shell exec accepts
that; launchd does not.

**Fix:** sign it with the same identity the bundle's helpers carry (#5), no
hardened runtime:
```bash
codesign --force --sign "Developer ID Application: Christopher Robinson (CCYV5HQZCM)" target/release/cxtasks-server
codesign -dv target/release/cxtasks-server 2>&1 | grep flags    # flags=0x0(none)
launchctl kickstart -k "gui/$(id -u)/com.cxtasks.app.server"
```
The plist's `ThrottleInterval` is 30 s, so after a run of failed spawns the
first good one can take half a minute — `launchctl print` keeps showing the
OLD exit reason until then. Rebuilding the server for the LaunchAgent is a
two-step, build then sign, and nothing in the golden commands does the second
step for you.

### 47. The glass has to be turned on AFTER the first paint, or launch shows bare wallpaper
**Symptom:** the app launches into a rectangle of desktop with three traffic
lights floating in it, for anywhere from a few frames to most of a second, then
snaps to the board. Nothing errors, and it only reproduces on a cold start —
so it survives every hot-reload iteration you do while building the feature.

**Cause:** `"transparent": true` means the NSWindow has no background of its
own from the moment it is created. The old `NSVisualEffectView` concealed that
gap by *being* something to look at during it — a material that paints as soon
as it is installed, before any web content exists. A CGS blur has nothing to
paint: it blurs whatever is behind the window and contributes no pixels itself,
so with an empty webview in front of it the user is looking straight at their
desktop.

**Fix, in two halves that have to stay paired.** `lib.rs`'s `.setup()` calls
`glass_macos::set_launch_background(window, 24, 24, 24)` — opaque, in dark's
`--bg-primary`, matching `tauri.conf.json`'s `"theme": "Dark"` boot default.
`main.tsx` then calls `startGlass` behind a **double**
`requestAnimationFrame`, which is what flips `setOpaque(false)` and applies the
radius. A single frame fires before the composite often enough to show the
flash it exists to prevent; a `setTimeout` is worse, because it is a guess at
the number the double-rAF measures.

**Two smaller traps inside that, each of which cost a round:**
- **Clear the background to alpha 0.01, never 0.** A fully clear background on
  a window that also draws a shadow makes AppKit chamfer the corners — a
  hairline notch of desktop cut across each rounded corner, where the shadow's
  mask and the clear background disagree about where the window ends. One
  hundredth of a percent of black is invisible and stops it.
- **`invalidateShadow()` after changing opacity.** AppKit caches the shadow
  path against the window's opaque-ness, so without it the window keeps the
  *opaque* shadow it computed at creation and the corners draw a second,
  misaligned edge.

**And the radius must go to 0 BEFORE the background goes opaque** when turning
glass off (`glass_macos::disable`). The other order leaves one frame where an
opaque window still carries a blur region, which the WindowServer paints as a
grey halo just outside the frame.

### 49. `NSPasteboard` is main-thread only — a background clipboard read crashes on ⌘V

**Symptom:** the app dies with SIGSEGV the moment an image is pasted. The crash
report shows `__NSFastEnumerationMutationHandler` under
`-[NSPasteboard _updateTypeCacheIfNeeded]`, on a non-main thread (2026-10-01,
T233).

**Cause:** `tauri-plugin-clipboard-manager` (arboard underneath) reads the
pasteboard on whatever thread calls it. The first version of `clipboard_image()`
called it inside `spawn_blocking`. On ⌘V, WebKit reads the SAME pasteboard on the
main thread at the same moment, the type cache is rebuilt under the background
enumeration, and AppKit faults. A button-triggered read rarely collides, so the
bug shows only on the keyboard path. That is the path you can't test headless.

**Fix:** read through `app.run_on_main_thread` and hand the pixels back over a
`tokio::sync::oneshot`. Only the PNG encode may run off the main thread. Any new
pasteboard access (text, HTML, files) follows the same rule.
