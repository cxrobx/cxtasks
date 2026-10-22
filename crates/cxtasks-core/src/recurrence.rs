//! RRULE-lite: `daily` | `weekly:MO,TH` | `monthly:15`.
//!
//! Deliberately not iCalendar RRULE. Three shapes cover every recurring task
//! Chris actually files, and each one is explainable in a sentence — a full
//! RRULE implementation is a week of edge cases (BYSETPOS, COUNT, UNTIL,
//! leap-day semantics) for no gain in a personal todo app.
//!
//! **Day arithmetic happens in local time**, then converts back to UTC for
//! storage. "Every Monday at 9am" must stay 9am local across a DST boundary;
//! doing the arithmetic on the UTC instant would drift it by an hour twice a
//! year. Storage stays ISO-8601 UTC with a trailing `Z` so the comparison rule
//! in `db/tasks.rs` (gotcha #23) holds everywhere.

use crate::db::tasks::{self, NewTask, Task};
use crate::error::AppError;
use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone, Utc, Weekday};
use rusqlite::Connection;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recurrence {
    Daily,
    /// Non-empty, de-duplicated, in calendar order.
    Weekly(Vec<Weekday>),
    /// 1..=31. A day past the end of a short month clamps to that month's last day.
    Monthly(u32),
    /// The `day`-th of each QUARTER's opening month — January, April, July,
    /// October. Added for the one thing Chris reviews on that cadence, the
    /// tripwire review (§8.4 of the career synthesis): `monthly:1` would fire
    /// three times for every review that matters, and a task that is wrong
    /// two times in three teaches you to ignore it.
    Quarterly(u32),
}

fn parse_weekday(token: &str) -> Result<Weekday, String> {
    match token.trim().to_ascii_uppercase().as_str() {
        "MO" | "MON" | "MONDAY" => Ok(Weekday::Mon),
        "TU" | "TUE" | "TUESDAY" => Ok(Weekday::Tue),
        "WE" | "WED" | "WEDNESDAY" => Ok(Weekday::Wed),
        "TH" | "THU" | "THURSDAY" => Ok(Weekday::Thu),
        "FR" | "FRI" | "FRIDAY" => Ok(Weekday::Fri),
        "SA" | "SAT" | "SATURDAY" => Ok(Weekday::Sat),
        "SU" | "SUN" | "SUNDAY" => Ok(Weekday::Sun),
        other => Err(format!("unknown weekday '{other}' (use MO,TU,WE,TH,FR,SA,SU)")),
    }
}

impl Recurrence {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err("empty recurrence".into());
        }
        let (head, tail) = match raw.split_once(':') {
            Some((h, t)) => (h.trim().to_ascii_lowercase(), t.trim()),
            None => (raw.to_ascii_lowercase(), ""),
        };

        match head.as_str() {
            "daily" => {
                if tail.is_empty() {
                    Ok(Recurrence::Daily)
                } else {
                    Err("daily takes no argument".into())
                }
            }
            "weekly" => {
                if tail.is_empty() {
                    return Err("weekly needs days, e.g. weekly:MO,TH".into());
                }
                let mut days: Vec<Weekday> = Vec::new();
                for tok in tail.split(',') {
                    let d = parse_weekday(tok)?;
                    if !days.contains(&d) {
                        days.push(d);
                    }
                }
                days.sort_by_key(|d| d.num_days_from_monday());
                Ok(Recurrence::Weekly(days))
            }
            "monthly" => {
                let day: u32 = tail
                    .parse()
                    .map_err(|_| "monthly needs a day of month, e.g. monthly:15".to_string())?;
                if !(1..=31).contains(&day) {
                    return Err("monthly day must be 1-31".into());
                }
                Ok(Recurrence::Monthly(day))
            }
            "quarterly" => {
                let day: u32 = tail
                    .parse()
                    .map_err(|_| "quarterly needs a day of month, e.g. quarterly:1".to_string())?;
                if !(1..=31).contains(&day) {
                    return Err("quarterly day must be 1-31".into());
                }
                Ok(Recurrence::Quarterly(day))
            }
            other => Err(format!(
                "unknown recurrence '{other}' (expected daily | weekly:MO,TH | monthly:15 | quarterly:1)"
            )),
        }
    }

    /// The first occurrence strictly after `after`, preserving its time of day.
    pub fn next_after(&self, after: DateTime<Utc>) -> DateTime<Utc> {
        let local = after.with_timezone(&Local);
        let time = local.time();
        let date = local.date_naive();

        let next_date: NaiveDate = match self {
            Recurrence::Daily => date + Duration::days(1),
            Recurrence::Weekly(days) => {
                // Scan the next 7 days for the soonest listed weekday.
                (1..=7)
                    .map(|n| date + Duration::days(n))
                    .find(|d| days.contains(&d.weekday()))
                    .unwrap_or(date + Duration::days(7))
            }
            Recurrence::Monthly(day) => {
                let this_month = clamp_day(date.year(), date.month(), *day);
                if this_month > date {
                    this_month
                } else {
                    let (y, m) = if date.month() == 12 {
                        (date.year() + 1, 1)
                    } else {
                        (date.year(), date.month() + 1)
                    };
                    clamp_day(y, m, *day)
                }
            }
            Recurrence::Quarterly(day) => {
                // The soonest quarter-opening month (1, 4, 7, 10) whose
                // `day`-th is strictly after `date`: this quarter's own if it
                // hasn't passed, otherwise the next quarter's.
                let mut y = date.year();
                let mut m = ((date.month() - 1) / 3) * 3 + 1;
                loop {
                    let candidate = clamp_day(y, m, *day);
                    if candidate > date {
                        break candidate;
                    }
                    m += 3;
                    if m > 12 {
                        m = 1;
                        y += 1;
                    }
                }
            }
        };

        // Ambiguous/skipped local times (DST transitions) fall back to the
        // earliest valid instant for that date rather than failing.
        match Local.from_local_datetime(&next_date.and_time(time)).earliest() {
            Some(dt) => dt.with_timezone(&Utc),
            None => Local
                .from_local_datetime(&next_date.and_hms_opt(12, 0, 0).unwrap_or_default())
                .earliest()
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or(after + Duration::days(1)),
        }
    }
}

/// The `day`-th of the given month, clamped to the month's last day.
fn clamp_day(year: i32, month: u32, day: u32) -> NaiveDate {
    for d in (1..=day).rev() {
        if let Some(date) = NaiveDate::from_ymd_opt(year, month, d) {
            return date;
        }
    }
    NaiveDate::from_ymd_opt(year, month, 1).unwrap_or_default()
}

/// Storage format: ISO-8601 UTC with a trailing `Z` and milliseconds — byte
/// identical to JavaScript's `Date.toISOString()`, which is what the frontend
/// writes. Keeping one format means `datetime()` parses every row the same way.
pub fn to_iso(dt: DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

pub fn parse_iso(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw.trim())
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Complete a task and, if it recurs, materialize the next instance.
///
/// Returns `(completed_task, next_instance)`. The next instance copies the
/// task's definition — title, notes, project, priority, execution context — and
/// starts at `todo` rather than `inbox` (a recurring commitment is already
/// triaged).
///
/// **The result becomes HISTORY, not an appendix to the body.** It used to be
/// stamped onto `tasks.body`, which is precisely what `db::comments` warns
/// against: the body is the task's *definition* and comments are its *history*,
/// and folding them gives you a body nobody dares edit because it also holds
/// the record. v12 already removed the same pattern from the runner (which
/// appended "Background run refused: …" to the body) in favour of
/// `blocked_reason`. It fixed a real second-order bug: a recurring task's
/// result was appended to the body, and every completion grew the body it had
/// to snapshot, so the definition drifted further from what the next instance
/// should say.
///
/// **And the history is SPLIT, exactly as `runner::finish_run` splits it**
/// (invariant 18): the full text onto a `task_runs` row via
/// [`crate::db::runs::record_foreground_handoff`], a derived one-line summary
/// into the comment log. The whole result used to go in as a comment, which
/// looked equivalent and was not — `dispatch::history_block` truncates a
/// comment at `HISTORY_COMMENT_CHARS` (1200) but replays a run's `result_text`
/// in full, so a 3,000-character foreground result reached the next agent as a
/// mid-sentence fragment while a background one of the same size arrived
/// whole. The split runs HERE rather than in `complete_task` because both the
/// Tauri command and the MCP tool route through this function, and gotcha
/// #18's lesson is that a rule enforced at one of two entry points is enforced
/// at neither.
///
/// The run-row write is best-effort in the honest direction: if it fails, the
/// comment falls back to carrying the full text, because losing the record
/// entirely is worse than losing the split.
pub fn complete_with_recurrence(
    conn: &Connection,
    task_id: &str,
    result: Option<&str>,
) -> Result<(Task, Option<Task>), AppError> {
    complete_with_recurrence_as(conn, task_id, result, crate::db::comments::AUTHOR_CLAUDE)
}

/// [`complete_with_recurrence`] with the result's comment signed by `author` —
/// the MCP passes the agent that is actually calling it (Codex signs as Codex).
pub fn complete_with_recurrence_as(
    conn: &Connection,
    task_id: &str,
    result: Option<&str>,
    author: &str,
) -> Result<(Task, Option<Task>), AppError> {
    let before = tasks::get(conn, task_id)?
        .ok_or_else(|| AppError::NotFound(format!("task {task_id} not found")))?;

    // The next instance copies this. No longer needs a "snapshot before the
    // append" caveat — nothing appends to it any more.
    let original_body = before.body.clone();

    if let Some(result) = result.map(str::trim).filter(|r| !r.is_empty()) {
        // The artifact goes on a run row, where `history_block` replays it in
        // full and the app renders it beside every other run's handoff.
        let body = match crate::db::runs::record_foreground_handoff(
            conn,
            task_id,
            before.bg_intent,
            result,
        ) {
            Ok(_) => format!(
                "Result · foreground session\n\n{}\n\n_Full handoff on the run._",
                crate::runner::handoff_summary(result)
            ),
            // The comment carries the whole thing rather than a summary
            // pointing at a run row that doesn't hold it. A pointer to nothing
            // is worse than a long comment.
            Err(e) => {
                log::warn!(
                    "task {task_id}: could not record the completion result on a run row \
                     ({e}); keeping the full text in the comment log instead"
                );
                format!("Result · foreground session\n\n{result}")
            }
        };
        // Best-effort, like the runner's mirror: failing to file the history
        // must not stop the task being completed.
        if let Err(e) =
            crate::db::comments::add(conn, task_id, author, &body)
        {
            log::warn!("task {task_id}: could not record the completion result: {e}");
        }
    }

    let completed = tasks::complete(conn, task_id)?;

    let Some(rule) = completed.recurrence.as_deref() else {
        return Ok((completed, None));
    };
    let Ok(rec) = Recurrence::parse(rule) else {
        // A rule that no longer parses (hand-edited DB) must not block
        // completion — the task is done either way.
        log::warn!("task {task_id}: unparseable recurrence '{rule}', not repeating");
        return Ok((completed, None));
    };

    // Anchor on the task's own due date when it has one, so a chain of daily
    // tasks doesn't drift later every time it's completed late.
    let anchor = completed
        .due_at
        .as_deref()
        .and_then(parse_iso)
        .unwrap_or_else(Utc::now);
    let now = Utc::now();

    // A task can be years overdue (a daily habit abandoned in 2020). Stepping
    // one period at a time from the original anchor would need thousands of
    // iterations, so re-anchor to yesterday *at the task's own time of day*
    // first — that preserves "every Monday at 9am" while collapsing the
    // backlog. The loop below then advances at most one period (a few, for
    // weekly rules whose next listed day isn't tomorrow).
    let mut base = anchor;
    if base < now {
        let time_of_day = anchor.with_timezone(&Local).time();
        let yesterday = now.with_timezone(&Local).date_naive() - Duration::days(1);
        if let Some(dt) = Local
            .from_local_datetime(&yesterday.and_time(time_of_day))
            .earliest()
        {
            base = dt.with_timezone(&Utc);
        }
    }

    let mut next_due = rec.next_after(base);
    let mut guard = 0;
    while next_due < now && guard < 40 {
        next_due = rec.next_after(next_due);
        guard += 1;
    }

    // **The next instance is DEFERRED to the start of its own local due day**,
    // and that is what keeps this build's behaviour identical to the last
    // one's. Until Upcoming was retired, a future `due_at` was itself enough
    // to keep the row out of Open — so next Monday's sweep was invisible over
    // the weekend for free. Open no longer honours a due date (a deadline is
    // not a start date), so the hold has to be written down rather than
    // implied: the row now says "Until a date" in Waiting and surfaces on the
    // morning it is due, exactly as it always did.
    //
    // Only when that instant is genuinely in the FUTURE. A next instance due
    // later today — a `daily` task closed before its own time of day — must
    // not be deferred to a midnight that has already passed, which would be a
    // hold that holds nothing while reading as one on the row.
    //
    // `earliest()` because a local midnight can be a DST gap (Brazil, Chile)
    // and there is then no such instant; no defer is the safe direction to
    // fail in, since the task merely shows a day early.
    let next_defer = Local
        .from_local_datetime(&next_due.with_timezone(&Local).date_naive().and_time(
            chrono::NaiveTime::MIN,
        ))
        .earliest()
        .map(|dt| dt.with_timezone(&Utc))
        .filter(|start| *start > now)
        .map(to_iso);

    let next = tasks::create(
        conn,
        &NewTask {
            // Server-side creation: let the DB mint the id.
            id: None,
            title: completed.title.clone(),
            body: original_body,
            status: Some("todo".into()),
            priority: Some(completed.priority),
            project: completed.project.clone(),
            due_at: Some(to_iso(next_due)),
            // The start of the due day — see `next_defer` above.
            defer_until: next_defer,
            recurrence: Some(rule.to_string()),
            parent_id: None,
            repo_path: completed.repo_path.clone(),
            prompt: completed.prompt.clone(),
            bg_allowed: Some(completed.bg_allowed),
            // Carried, like every other execution-context field: "run the
            // Monday sweep under Codex" is a property of the recurring task,
            // and silently reverting next week's instance to Claude would be a
            // configuration change nobody made.
            bg_executor: Some(completed.bg_executor),
            bg_writes_allowed: Some(completed.bg_writes_allowed),
            bg_shell_allowed: Some(completed.bg_shell_allowed),
            // Carried for the same reason as the executor: "the Monday sweep
            // may run on the NAS" is a property of the recurring task, and
            // dropping it would silently strand every future instance on the
            // one machine — a routing change nobody made, discovered as a queue
            // that stopped draining while the laptop was shut.
            bg_remote_allowed: Some(completed.bg_remote_allowed),
            // Carried for the same reason: the tool surface a detached task
            // needs is a property of the recurring task, and dropping it would
            // leave every future instance able to read nothing — which fails
            // as a confident report about an empty directory.
            bg_mcp_servers: completed.bg_mcp_servers.clone(),
            model: completed.model.clone(),
            section_id: completed.section_id.clone(),
            // Carried for the same reason as the executor above: "the Monday
            // sweep needs an hour" is a property of the recurring task, and
            // silently handing next week's instance the default 20 minutes
            // would be a budget change nobody made — discovered as a timeout.
            bg_timeout_minutes: completed.bg_timeout_minutes,
            bg_run_token_cap: completed.bg_run_token_cap,
            // Deliberately NOT carried. A blocker is about this instance —
            // "waiting on Dana's sign-off" was true of the sweep you just
            // finished, not of next Monday's, and inheriting it would file every
            // future occurrence pre-blocked on something already resolved.
            blocked_reason: None,
            // Deliberately NOT carried either — a tripwire is ONE-SHOT across
            // recurrence (invariant 26). Conditions are level-triggered: "an
            // outside user exists" stays true forever once it is true, so a
            // carried tripwire would fire on the next instance the moment it
            // was created, and with `on_trip = complete` would close every
            // future instance unread. The next instance starts with no
            // condition; a human puts one back if that is what they meant.
            tripwire: None,
            tripwire_every_minutes: None,
            tripwire_on_trip: None,
        },
    )?;

    Ok((completed, Some(next)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::test_conn;

    fn utc(s: &str) -> DateTime<Utc> {
        parse_iso(s).unwrap()
    }

    #[test]
    fn parses_the_three_shapes() {
        assert_eq!(Recurrence::parse("daily").unwrap(), Recurrence::Daily);
        assert_eq!(Recurrence::parse("DAILY").unwrap(), Recurrence::Daily);
        assert_eq!(
            Recurrence::parse("weekly:MO,TH").unwrap(),
            Recurrence::Weekly(vec![Weekday::Mon, Weekday::Thu])
        );
        assert_eq!(Recurrence::parse("monthly:15").unwrap(), Recurrence::Monthly(15));
    }

    #[test]
    fn weekly_dedupes_and_orders_days() {
        assert_eq!(
            Recurrence::parse("weekly:FR,MO,fri").unwrap(),
            Recurrence::Weekly(vec![Weekday::Mon, Weekday::Fri])
        );
    }

    #[test]
    fn rejects_junk_rules() {
        for bad in ["", "yearly", "weekly", "weekly:XX", "monthly", "monthly:0", "monthly:32", "daily:3"] {
            assert!(Recurrence::parse(bad).is_err(), "'{bad}' should not parse");
        }
    }

    #[test]
    fn daily_advances_one_day_preserving_time() {
        let next = Recurrence::Daily.next_after(utc("2026-08-01T13:30:00.000Z"));
        let local_before = utc("2026-08-01T13:30:00.000Z").with_timezone(&Local);
        let local_after = next.with_timezone(&Local);
        assert_eq!(local_after.time(), local_before.time());
        assert_eq!(
            (local_after.date_naive() - local_before.date_naive()).num_days(),
            1
        );
    }

    #[test]
    fn weekly_finds_the_next_listed_day_strictly_after() {
        // 2026-08-03 is a Monday.
        let monday = utc("2026-08-03T12:00:00.000Z");
        let rec = Recurrence::parse("weekly:MO,TH").unwrap();
        let next = rec.next_after(monday);
        assert_eq!(next.with_timezone(&Local).date_naive().weekday(), Weekday::Thu);

        // From Thursday it wraps to the following Monday.
        let after_thu = rec.next_after(next);
        assert_eq!(
            after_thu.with_timezone(&Local).date_naive().weekday(),
            Weekday::Mon
        );
        assert!(after_thu > next);
    }

    #[test]
    fn weekly_single_day_advances_a_full_week() {
        let rec = Recurrence::parse("weekly:MO").unwrap();
        let monday = utc("2026-08-03T12:00:00.000Z");
        let next = rec.next_after(monday);
        let days = (next.with_timezone(&Local).date_naive()
            - monday.with_timezone(&Local).date_naive())
        .num_days();
        assert_eq!(days, 7);
    }

    #[test]
    fn monthly_rolls_to_next_month_and_clamps_short_months() {
        let rec = Recurrence::parse("monthly:31").unwrap();
        // From Jan 31 the next is Feb 28 (2026 is not a leap year).
        let next = rec.next_after(utc("2026-01-31T12:00:00.000Z"));
        let d = next.with_timezone(&Local).date_naive();
        assert_eq!((d.month(), d.day()), (2, 28));
    }

    #[test]
    fn monthly_takes_this_month_when_the_day_is_still_ahead() {
        let rec = Recurrence::parse("monthly:15").unwrap();
        let next = rec.next_after(utc("2026-08-03T12:00:00.000Z"));
        let d = next.with_timezone(&Local).date_naive();
        assert_eq!((d.month(), d.day()), (8, 15));
    }

    #[test]
    fn next_after_is_always_strictly_later() {
        for rule in ["daily", "weekly:MO,TU,WE,TH,FR,SA,SU", "monthly:1"] {
            let rec = Recurrence::parse(rule).unwrap();
            let base = utc("2026-08-03T12:00:00.000Z");
            assert!(rec.next_after(base) > base, "{rule} did not advance");
        }
    }

    #[test]
    fn iso_round_trips_in_the_frontend_format() {
        let s = "2026-08-01T13:30:00.000Z";
        assert_eq!(to_iso(parse_iso(s).unwrap()), s);
    }

    #[test]
    fn completing_a_non_recurring_task_creates_nothing() {
        let conn = test_conn();
        let t = tasks::create(
            &conn,
            &NewTask {
                title: "one-off".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let (done, next) = complete_with_recurrence(&conn, &t.id, None).unwrap();
        assert_eq!(done.status, "done");
        assert!(next.is_none());
        assert_eq!(tasks::list(&conn, &Default::default()).unwrap().len(), 0);
    }

    #[test]
    fn completing_a_daily_task_materializes_tomorrow() {
        let conn = test_conn();
        let t = tasks::create(
            &conn,
            &NewTask {
                title: "morning pages".into(),
                recurrence: Some("daily".into()),
                due_at: Some(to_iso(Utc::now())),
                project: Some("chrisx".into()),
                priority: Some(2),
                ..Default::default()
            },
        )
        .unwrap();

        let (done, next) = complete_with_recurrence(&conn, &t.id, None).unwrap();
        assert_eq!(done.status, "done");
        let next = next.expect("daily task must repeat");
        assert_eq!(next.title, "morning pages");
        assert_eq!(next.project.as_deref(), Some("chrisx"));
        assert_eq!(next.priority, 2);
        assert_eq!(next.recurrence.as_deref(), Some("daily"));
        assert_eq!(next.status, "todo");
        assert_ne!(next.id, done.id);

        let due = parse_iso(next.due_at.as_deref().unwrap()).unwrap();
        assert!(due > Utc::now(), "next instance must be in the future");
    }

    /// Tomorrow's instance carries a REAL hold, rather than leaning on a gate
    /// Open no longer has. It must be out of Open and out of `next_task`, and
    /// in Waiting — the behaviour a recurring task always had, now written
    /// down as a defer instead of implied by the due date.
    #[test]
    fn the_next_instance_is_deferred_to_the_start_of_its_due_day() {
        let conn = test_conn();
        let t = tasks::create(
            &conn,
            &NewTask {
                title: "morning pages".into(),
                recurrence: Some("daily".into()),
                due_at: Some(to_iso(Utc::now())),
                ..Default::default()
            },
        )
        .unwrap();
        let (_, next) = complete_with_recurrence(&conn, &t.id, None).unwrap();
        let next = next.unwrap();

        let defer = parse_iso(next.defer_until.as_deref().expect("a hold, not a due date"))
            .unwrap()
            .with_timezone(&Local);
        let due = parse_iso(next.due_at.as_deref().unwrap())
            .unwrap()
            .with_timezone(&Local);
        assert_eq!(defer.date_naive(), due.date_naive(), "same local day");
        assert_eq!(defer.time(), chrono::NaiveTime::MIN, "start of that day");

        let names = |view| {
            tasks::list(
                &conn,
                &tasks::ListFilters {
                    view: Some(view),
                    ..Default::default()
                },
            )
            .unwrap()
            .into_iter()
            .map(|t| t.title)
            .collect::<Vec<_>>()
        };
        assert!(names(tasks::View::Open).is_empty(), "held, so not in Open");
        assert_eq!(names(tasks::View::Waiting), vec!["morning pages"]);
        assert!(
            tasks::next_task(&conn, None).unwrap().is_none(),
            "a held task is never handed out"
        );
    }

    /// A `daily` task closed before its own time of day repeats LATER TODAY,
    /// and today's midnight is already behind us. Deferring to it would write
    /// a hold that holds nothing while the row still reads "Until a date".
    #[test]
    fn a_next_instance_due_later_today_gets_no_defer() {
        let conn = test_conn();
        // Due in an hour, yesterday's date as the anchor's day-of-week is
        // irrelevant for `daily` — what matters is that `next_after` lands
        // inside today.
        // Halfway to local midnight rather than a flat hour: run after 23:00
        // and "an hour from now" is tomorrow, which failed the date assert
        // below every night for an hour.
        let now = Utc::now();
        let midnight = (now.with_timezone(&Local).date_naive() + Duration::days(1))
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_local_timezone(Local)
            .earliest()
            .unwrap()
            .with_timezone(&Utc);
        let anchor = now - Duration::days(1) + (midnight - now) / 2;
        let t = tasks::create(
            &conn,
            &NewTask {
                title: "standup note".into(),
                recurrence: Some("daily".into()),
                due_at: Some(to_iso(anchor)),
                ..Default::default()
            },
        )
        .unwrap();
        let (_, next) = complete_with_recurrence(&conn, &t.id, None).unwrap();
        let next = next.unwrap();

        let due = parse_iso(next.due_at.as_deref().unwrap()).unwrap();
        assert_eq!(
            due.with_timezone(&Local).date_naive(),
            Utc::now().with_timezone(&Local).date_naive(),
            "fixture must produce a same-day instance, or it proves nothing"
        );
        assert_eq!(next.defer_until, None, "a passed midnight is not a hold");
    }

    #[test]
    fn recurrence_carries_execution_context_forward() {
        let conn = test_conn();
        let t = tasks::create(
            &conn,
            &NewTask {
                title: "weekly repo sweep".into(),
                recurrence: Some("weekly:MO".into()),
                repo_path: Some("/tmp".into()),
                prompt: Some("check for stale branches".into()),
                bg_allowed: Some(true),
                bg_writes_allowed: Some(false),
                due_at: Some(to_iso(Utc::now())),
                ..Default::default()
            },
        )
        .unwrap();
        let (_, next) = complete_with_recurrence(&conn, &t.id, None).unwrap();
        let next = next.unwrap();
        assert_eq!(next.repo_path.as_deref(), Some("/tmp"));
        assert_eq!(next.prompt.as_deref(), Some("check for stale branches"));
        assert!(next.bg_allowed);
        assert!(!next.bg_writes_allowed);
    }

    #[test]
    fn result_text_lands_on_the_completed_task_not_the_next_instance() {
        let conn = test_conn();
        let t = tasks::create(
            &conn,
            &NewTask {
                title: "daily check".into(),
                body: Some("original notes".into()),
                recurrence: Some("daily".into()),
                due_at: Some(to_iso(Utc::now())),
                ..Default::default()
            },
        )
        .unwrap();

        let (done, next) = complete_with_recurrence(&conn, &t.id, Some("all green")).unwrap();

        // The result is HISTORY, so it goes in the comment log — the same
        // channel the runner writes to and the same one the next dispatched
        // agent is handed back (`dispatch::history_block`).
        let comments = crate::db::comments::for_task(&conn, &done.id).unwrap();
        assert_eq!(comments.len(), 1);
        assert!(comments[0].body.contains("all green"), "{:?}", comments[0].body);
        assert_eq!(comments[0].author, crate::db::comments::AUTHOR_CLAUDE);

        // And the body is the DEFINITION, untouched. It used to be appended to,
        // which is the pattern `db::comments` warns against and which grew the
        // body every single completion of a recurring task — so the definition
        // the next instance copies drifted a little further each time.
        let done = tasks::get(&conn, &done.id).unwrap().unwrap();
        assert_eq!(
            done.body.as_deref(),
            Some("original notes"),
            "the result must not be folded into the task's definition"
        );

        let next = next.unwrap();
        assert_eq!(
            next.body.as_deref(),
            Some("original notes"),
            "the next instance must not inherit this run's result"
        );
        assert!(
            crate::db::comments::for_task(&conn, &next.id).unwrap().is_empty(),
            "history belongs to the instance that produced it"
        );
    }

    /// **The same split the background runner performs** (invariant 18), and
    /// the reason it is worth doing: `dispatch::history_block` truncates a
    /// comment at 1200 chars but replays a run's `result_text` in full, so a
    /// long foreground result written only as a comment reaches the next agent
    /// as a mid-sentence fragment.
    ///
    /// The comment assertion is the half that fails silently if the split is
    /// only half-done — writing the run row and leaving the full text in the
    /// log too renders the same brief twice in one pane.
    #[test]
    fn completing_with_a_result_splits_it_across_the_run_row_and_the_log() {
        let conn = test_conn();
        let t = tasks::create(
            &conn,
            &NewTask { title: "long result".into(), ..Default::default() },
        )
        .unwrap();

        let handoff = format!(
            "Found the 429: a missing retry on lib/webhook.ts:88.\n\n\
             ## What I did\n\n{}\n\n## Next step\n\nAdd the backoff.",
            "Traced every call site. ".repeat(120)
        );
        let (done, _) = complete_with_recurrence(&conn, &t.id, Some(&handoff)).unwrap();

        let runs = crate::db::runs::list_for_task(&conn, &done.id).unwrap();
        assert_eq!(runs.len(), 1, "the artifact needs a row of its own");
        assert_eq!(runs[0].mode, "foreground");
        assert_eq!(runs[0].status, crate::db::runs::FOREGROUND_STATUS);
        assert_eq!(
            runs[0].result_text.as_deref(),
            Some(handoff.as_str()),
            "the run row holds the handoff in FULL — that is what gets replayed"
        );

        let comments = crate::db::comments::for_task(&conn, &done.id).unwrap();
        assert_eq!(comments.len(), 1);
        let body = &comments[0].body;
        assert!(body.starts_with("Result · foreground session"), "{body}");
        assert!(body.contains("Found the 429"), "the outcome line survives: {body}");
        assert!(body.contains("_Full handoff on the run._"), "{body}");
        assert!(
            !body.contains("Traced every call site."),
            "the log gets a summary, not a second copy of the brief: {body}"
        );
        assert!(body.len() < handoff.len() / 4, "it has to actually shrink: {body}");
    }

    #[test]
    fn a_long_overdue_recurring_task_skips_the_backlog() {
        let conn = test_conn();
        let t = tasks::create(
            &conn,
            &NewTask {
                title: "stale daily".into(),
                recurrence: Some("daily".into()),
                due_at: Some("2020-01-01T09:00:00.000Z".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let (_, next) = complete_with_recurrence(&conn, &t.id, None).unwrap();
        let due = parse_iso(next.unwrap().due_at.as_deref().unwrap()).unwrap();
        assert!(
            due > Utc::now(),
            "should skip forward past six years of missed instances, got {due}"
        );
        assert!(due < Utc::now() + Duration::days(2));
    }

    #[test]
    fn unparseable_stored_rule_still_completes_the_task() {
        let conn = test_conn();
        let t = tasks::create(
            &conn,
            &NewTask {
                title: "x".into(),
                ..Default::default()
            },
        )
        .unwrap();
        // Hand-edited DB: a rule that create() would have rejected.
        conn.execute(
            "UPDATE tasks SET recurrence = 'every blue moon' WHERE id = ?1",
            rusqlite::params![t.id],
        )
        .unwrap();
        let (done, next) = complete_with_recurrence(&conn, &t.id, None).unwrap();
        assert_eq!(done.status, "done");
        assert!(next.is_none());
    }

    /// The whole execution context carries to the next instance, executor
    /// included. "Run the Monday sweep under Codex" is a property of the
    /// recurring task; silently reverting next week's copy to Claude would be a
    /// configuration change nobody made and nobody would notice until the wrong
    /// agent produced the wrong-shaped answer.
    #[test]
    fn the_next_instance_keeps_the_background_executor_and_its_grant() {
        let conn = test_conn();
        let t = tasks::create(
            &conn,
            &NewTask {
                title: "weekly sweep".into(),
                recurrence: Some("weekly:MO".into()),
                due_at: Some("2026-07-27T09:00:00.000Z".into()),
                repo_path: Some("/tmp".into()),
                bg_allowed: Some(true),
                bg_executor: Some(crate::executor::BackgroundExecutor::Codex),
                bg_shell_allowed: Some(true),
                ..Default::default()
            },
        )
        .unwrap();

        let (_, next) = complete_with_recurrence(&conn, &t.id, None).unwrap();
        let next = next.expect("a recurring task must materialize its next instance");
        assert_eq!(next.bg_executor, crate::executor::BackgroundExecutor::Codex);
        assert!(next.bg_allowed, "the opt-in must carry, not silently reset");
        assert!(next.bg_shell_allowed);
        assert_eq!(next.repo_path.as_deref(), Some("/tmp"));
    }
}

#[cfg(test)]
mod quarterly_tests {
    use super::*;

    fn local_date(dt: DateTime<Utc>) -> NaiveDate {
        dt.with_timezone(&Local).date_naive()
    }

    fn at(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        Local
            .with_ymd_and_hms(y, m, d, 9, 0, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn quarterly_parses_and_rejects_like_monthly() {
        assert_eq!(Recurrence::parse("quarterly:1").unwrap(), Recurrence::Quarterly(1));
        assert_eq!(Recurrence::parse("Quarterly: 15").unwrap(), Recurrence::Quarterly(15));
        assert!(Recurrence::parse("quarterly").is_err());
        assert!(Recurrence::parse("quarterly:0").is_err());
        assert!(Recurrence::parse("quarterly:32").is_err());
    }

    #[test]
    fn quarterly_lands_on_the_next_quarter_opening_month() {
        let q = Recurrence::Quarterly(1);
        // Mid-quarter: the next opening month is the coming one.
        assert_eq!(
            local_date(q.next_after(at(2026, 8, 25))),
            NaiveDate::from_ymd_opt(2026, 10, 1).unwrap()
        );
        // ON the opening day: strictly after, so the following quarter.
        assert_eq!(
            local_date(q.next_after(at(2026, 10, 1))),
            NaiveDate::from_ymd_opt(2027, 1, 1).unwrap()
        );
        // Year boundary from December.
        assert_eq!(
            local_date(q.next_after(at(2026, 12, 15))),
            NaiveDate::from_ymd_opt(2027, 1, 1).unwrap()
        );
        // A day the opening month may not have is clamped, like monthly.
        assert_eq!(
            local_date(Recurrence::Quarterly(31).next_after(at(2026, 3, 31))),
            NaiveDate::from_ymd_opt(2026, 4, 30).unwrap()
        );
    }
}
