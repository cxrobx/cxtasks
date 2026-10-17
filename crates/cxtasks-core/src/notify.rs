//! Where the runner hands off a "your run finished" banner.
//!
//! The runner used to call `notify_macos::show_notification` directly, and that
//! single line was the only thing stopping `runner/` from building for Linux.
//!
//! # Why a process-global hook and not a trait parameter
//!
//! The obvious alternative is a `&dyn Notifier` threaded down to
//! `finish_run`. It would need to pass through five signatures, and ~30 tests
//! construct those by hand — so the diff would be dominated by test bodies
//! gaining an argument they do not care about. That is exactly the churn the
//! split is trying to avoid, and it would make the "no test body changed"
//! review gate useless precisely where it is most needed.
//!
//! A `OnceLock` set once at startup is the same shape `notify_macos::APP_HANDLE`
//! already uses for the same reason, so it is also the shape a reader of this
//! codebase already knows.
//!
//! # Why the default is silence
//!
//! An unset notifier drops the banner. On Linux that is not a degradation, it
//! is the truth: nobody is standing at the NAS to see a notification, and the
//! run's outcome is on the task either way. Notification is a courtesy to
//! someone at the machine; a machine with no one at it owes no courtesy.
//!
//! It is emphatically NOT a substitute for recording the outcome — that goes
//! through `db::runs::finish`, which has nothing to do with this module.

use std::sync::OnceLock;

/// Posts a user-visible banner. Implemented per platform by whoever owns the
/// UI toolkit; this crate deliberately cannot.
pub trait Notifier: Send + Sync {
    fn post(&self, task_id: &str, title: &str, body: &str);
}

static NOTIFIER: OnceLock<Box<dyn Notifier>> = OnceLock::new();

/// Install the process's notifier. First call wins.
///
/// Returns whether it was installed. A second call is a no-op and says so
/// rather than panicking: losing a banner is not worth taking down a run, and
/// a silent overwrite would make "which notifier is live" unanswerable.
pub fn set_notifier(notifier: Box<dyn Notifier>) -> bool {
    let installed = NOTIFIER.set(notifier).is_ok();
    if !installed {
        log::warn!("a notifier was already installed; keeping the first one");
    }
    installed
}

/// Post a banner, if this process has a notifier. Silent when it does not.
pub fn post(task_id: &str, title: &str, body: &str) {
    match NOTIFIER.get() {
        Some(n) => n.post(task_id, title, body),
        None => log::debug!("no notifier installed; dropping banner for {task_id}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The no-op default must not panic. This is the Linux path, and it is
    /// reached on the unhappy branch of a run that has already failed — the
    /// worst possible moment to discover an unwrap.
    ///
    /// Deliberately asserts nothing about delivery, so it holds whether or not
    /// the test below has already claimed the `OnceLock`.
    #[test]
    fn posting_with_no_notifier_installed_is_silent_not_fatal() {
        post("task-1", "title", "body");
    }

    static DELIVERED: AtomicUsize = AtomicUsize::new(0);
    struct Counting;
    impl Notifier for Counting {
        fn post(&self, _task_id: &str, _title: &str, _body: &str) {
            DELIVERED.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// An installed notifier actually receives the banner.
    ///
    /// Worth its lines because the failure it catches is invisible: the hook
    /// compiles, the runner calls it, every test passes, and notifications
    /// simply stop — a regression nobody reports, because nothing looks broken.
    ///
    /// This is the only test that installs a notifier, so the `OnceLock` is
    /// uncontended and the assertion is deterministic.
    #[test]
    fn an_installed_notifier_receives_the_banner() {
        assert!(set_notifier(Box::new(Counting)), "nothing else may install one");
        post("task-2", "title", "body");
        assert_eq!(DELIVERED.load(Ordering::SeqCst), 1);

        // And a second install is refused rather than silently replacing it.
        assert!(!set_notifier(Box::new(Counting)));
        post("task-3", "title", "body");
        assert_eq!(DELIVERED.load(Ordering::SeqCst), 2, "the first notifier stayed live");
    }
}
