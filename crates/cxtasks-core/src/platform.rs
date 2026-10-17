//! Facts about the host that only the host's own crate can answer.
//!
//! Same shape and the same reasoning as [`crate::notify`]: a `OnceLock` filled
//! once at startup, rather than a parameter threaded through call sites whose
//! tests would then all have to supply it.
//!
//! Everything here is a QUESTION, never a capability. Nothing in this module
//! decides whether a run may happen — it only reports what is true about the
//! machine, so the UI can say something honest. Keeping that line matters:
//! invariant 19 is that intent must never decide a capability, and a hook that
//! quietly gates execution is the easiest way to break it by accident.

use std::sync::OnceLock;

type Probe = Box<dyn Fn() -> bool + Send + Sync>;

static DRAIN_INSTALLED: OnceLock<Probe> = OnceLock::new();

/// Teach this process how to answer "is a drain loop installed for this host".
///
/// On macOS that is a launchd query, which lives in `cxtasks_lib::launchd` and
/// cannot be reached from here. Installed by every binary that can be asked
/// the question.
pub fn set_drain_probe(probe: Probe) -> bool {
    let installed = DRAIN_INSTALLED.set(probe).is_ok();
    if !installed {
        log::warn!("a drain probe was already installed; keeping the first one");
    }
    installed
}

/// Is something going to drain the background queue on this host?
///
/// **Defaults to `true` when no probe is installed, and the direction of that
/// default is deliberate.** The only consumer is gotcha #18's warning — "you
/// queued into a runner that isn't running" — which is shown to a user who
/// would otherwise watch a task sit at `queued` forever with no explanation.
///
/// An unprobed process cannot tell whether a drain loop exists. Defaulting to
/// `false` would make it *assert* that none does, and every queue would raise a
/// warning that is merely a guess. A warning that fires when nothing is wrong
/// is worse than no warning: it trains you to dismiss the one that matters.
/// So an unknown answer stays quiet, and the platforms that can actually check
/// install a probe that speaks up.
pub fn drain_is_installed() -> bool {
    DRAIN_INSTALLED.get().map(|p| p()).unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unprobed must not warn. See the note on `drain_is_installed`: the
    /// failure this prevents is a permanent false alarm on every queue.
    #[test]
    fn an_unprobed_host_stays_quiet_rather_than_crying_wolf() {
        // Runs before any probe is installed in this binary; no test here
        // installs one, so this is deterministic.
        assert!(drain_is_installed());
    }
}
