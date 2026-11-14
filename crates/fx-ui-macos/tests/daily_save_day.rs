//! A week of the daily save, simulated minute by minute (run 2.15).
//!
//! The decision is the app's own: at every 5-minute check while the Mac is
//! awake, `fx_ui_macos::daily_save::daily_save_due_at` is asked with the
//! simulated clock, the store's saved time and how long nobody has used the
//! keyboard or mouse. The rest is a model of the engine's saves:
//!
//! - a save sets the store's saved time (`fx_engine::Engine::saved_at`, the
//!   file's modification time) [`SAVE_TIME`] after it starts, the time a
//!   whole-disk save takes;
//! - a whole disk changes all the time while the Mac is awake, so there is
//!   always something to save, and the first unsaved change follows each
//!   save at once;
//! - a compaction saves at once, and so does the engine's backstop after
//!   `fx_engine::DEFAULT_CHECKPOINT_MAX_DELAY` of awake time since the first
//!   unsaved change: the engine counts it in `Instant`s, which on macOS stop
//!   while the Mac sleeps;
//! - while the Mac sleeps, nothing runs: no check, no change, no save.
//!
//! The week starts on a Monday at 00:00, with the index saved on the
//! Sunday before at 22:00 (a first scan, a quit or an earlier save). The
//! clock is UTC (`SystemTime`), so time zones and daylight saving don't
//! move it.

use std::time::{Duration, SystemTime};

use fx_engine::DEFAULT_CHECKPOINT_MAX_DELAY;
use fx_ui_macos::daily_save::{
    DAILY_SAVE_CHECK, DAILY_SAVE_INPUT_IDLE, DAILY_SAVE_INTERVAL, daily_save_due_at,
};

const MINUTE: i64 = 60;
const HOUR: i64 = 3600;
const DAY: i64 = 24 * HOUR;
/// The simulated span: one week from Monday 00:00.
const WEEK: i64 = 7 * DAY;
/// When the index was last saved before the week: Sunday 22:00.
const START: i64 = -2 * HOUR;
/// From a save's start to its file's rename: a whole-disk save (about
/// 251 MB, flushed with `F_FULLFSYNC`) takes a few seconds.
const SAVE_TIME: i64 = 3;
/// How long ago the last input was while someone uses the Mac.
const BUSY_IDLE: i64 = 5;
/// A wall-clock origin for Monday 00:00: any fixed time does.
const MONDAY: Duration = Duration::from_secs(1_800_000_000);

fn secs(d: Duration) -> i64 {
    d.as_secs() as i64
}

/// The wall-clock time `t` seconds after Monday 00:00.
fn at(t: i64) -> SystemTime {
    let origin = SystemTime::UNIX_EPOCH + MONDAY;
    if t >= 0 {
        origin + Duration::from_secs(t as u64)
    } else {
        origin - Duration::from_secs(t.unsigned_abs())
    }
}

/// Day of the week (0 is Monday) and seconds since midnight of `t`.
fn day_and_time(t: i64) -> (i64, i64) {
    (t.div_euclid(DAY).rem_euclid(7), t.rem_euclid(DAY))
}

/// `h:m` as seconds since midnight.
const fn hm(h: i64, m: i64) -> i64 {
    h * HOUR + m * MINUTE
}

/// A Mac's week.
struct Mac {
    /// Whether the Mac is awake on `day` (0 is Monday) at `time` (seconds
    /// since midnight).
    awake: fn(day: i64, time: i64) -> bool,
    /// The spans of `day` (start and end, seconds since midnight) during
    /// which someone uses the keyboard or mouse.
    busy: fn(day: i64) -> Vec<(i64, i64)>,
    /// When the engine compacts, and so saves (seconds after Monday 00:00,
    /// on the minute, while the Mac is awake).
    compactions: Vec<i64>,
}

impl Mac {
    /// How long nobody has used the keyboard or mouse at `t`.
    fn input_idle(&self, t: i64) -> i64 {
        let day_start = t.div_euclid(DAY) * DAY;
        // The latest end of a busy span at or before `t`, up to a week back.
        for back in 0..8 {
            let start = day_start - back * DAY;
            let (day, _) = day_and_time(start);
            let mut spans = (self.busy)(day);
            spans.sort_unstable_by_key(|&(_, end)| std::cmp::Reverse(end));
            for (from, to) in spans {
                let (from, to) = (start + from, start + to);
                if from <= t && t < to {
                    return BUSY_IDLE;
                }
                if to <= t {
                    return t - to;
                }
            }
        }
        WEEK
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Daily,
    Compaction,
    Backstop,
}

/// One save: what made it, and its saved time (seconds after Monday 00:00).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Save {
    kind: Kind,
    at: i64,
}

/// Runs the week, checks every decision against the rule, and returns the
/// saves made in it.
fn simulate(mac: &Mac) -> Vec<Save> {
    let check = secs(DAILY_SAVE_CHECK);
    let backstop = secs(DEFAULT_CHECKPOINT_MAX_DELAY);
    let mut saved_at = START;
    // Awake time since the first unsaved change: the backstop's clock.
    let mut awake_unsaved = 0;
    let mut saves = Vec::new();
    let mut t = START;
    while t < WEEK {
        let (day, time) = day_and_time(t);
        let awake = (mac.awake)(day, time);
        let mut save = None;
        if awake && mac.compactions.contains(&t) {
            save = Some(Kind::Compaction);
        } else if awake && awake_unsaved >= backstop {
            save = Some(Kind::Backstop);
        } else if awake && t.rem_euclid(check) == 0 {
            let idle = mac.input_idle(t);
            let idle_d = Duration::from_secs(idle as u64);
            let due = daily_save_due_at(at(t), Some(at(saved_at)), idle_d);
            let rule =
                t - saved_at >= secs(DAILY_SAVE_INTERVAL) && idle >= secs(DAILY_SAVE_INPUT_IDLE);
            assert_eq!(
                due,
                rule,
                "day {day} at {time} s: saved {} s ago, idle {idle} s",
                t - saved_at
            );
            if due {
                save = Some(Kind::Daily);
            }
        }
        if let Some(kind) = save {
            saved_at = t + SAVE_TIME;
            awake_unsaved = 0;
            if t >= 0 {
                saves.push(Save { kind, at: saved_at });
            }
        }
        if awake {
            awake_unsaved += MINUTE;
        }
        t += MINUTE;
    }
    saves
}

/// The checks every week passes: a daily save comes only a day or more
/// after the last save of any kind, so a compaction's or the backstop's
/// save restarts the day and never adds a daily save of its own.
fn assert_rules(saves: &[Save]) {
    let mut previous = START;
    for s in saves {
        if s.kind == Kind::Daily {
            assert!(
                s.at - previous >= secs(DAILY_SAVE_INTERVAL),
                "a daily save {} s after the previous save: {saves:?}",
                s.at - previous
            );
        }
        previous = s.at;
    }
}

fn count(saves: &[Save], kind: Kind) -> usize {
    saves.iter().filter(|s| s.kind == kind).count()
}

/// The gaps between consecutive saves, the save before the week included.
fn gaps(saves: &[Save]) -> Vec<i64> {
    let mut previous = START;
    saves
        .iter()
        .map(|s| {
            let gap = s.at - previous;
            previous = s.at;
            gap
        })
        .collect()
}

fn always_awake(_day: i64, _time: i64) -> bool {
    true
}

/// A laptop open from 8:00 to 23:00 and asleep the rest of the night.
fn awake_8_to_23(_day: i64, time: i64) -> bool {
    (hm(8, 0)..hm(23, 0)).contains(&time)
}

/// Office hours on weekdays, with a lunch break; nobody at the Mac at night
/// or at the weekend.
fn office_hours(day: i64) -> Vec<(i64, i64)> {
    if day < 5 {
        vec![(hm(9, 0), hm(12, 30)), (hm(13, 15), hm(17, 30))]
    } else {
        Vec::new()
    }
}

/// In use from waking at 8:00 to the lid closing at 23:00, with a lunch
/// break and a dinner break.
fn laptop_day(_day: i64) -> Vec<(i64, i64)> {
    vec![
        (hm(8, 0), hm(12, 0)),
        (hm(12, 30), hm(18, 0)),
        (hm(19, 0), hm(23, 0)),
    ]
}

/// Input at every moment: something posts events all the time.
fn never_idle(_day: i64) -> Vec<(i64, i64)> {
    vec![(0, DAY)]
}

/// A small, seeded generator, so the compaction times are the same in
/// every run.
struct XorShift(u64);

impl XorShift {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

#[test]
fn an_office_desktop_saves_once_a_day_at_night() {
    let mac = Mac {
        awake: always_awake,
        busy: office_hours,
        compactions: Vec::new(),
    };
    let saves = simulate(&mac);
    assert_rules(&saves);
    // One a day, at 22:00 on Monday, then 5 minutes later each day: the
    // first check after the day has passed falls just after it, because
    // the saved time is the end of the save.
    assert_eq!(count(&saves, Kind::Daily), 7, "{saves:?}");
    assert_eq!(saves.len(), 7, "no other save: {saves:?}");
    assert_eq!(saves[0].at, hm(22, 0) + SAVE_TIME);
    assert_eq!(saves[6].at, 6 * DAY + hm(22, 30) + SAVE_TIME);
    for gap in gaps(&saves) {
        assert!(gap <= secs(DAILY_SAVE_INTERVAL + DAILY_SAVE_CHECK) + SAVE_TIME);
    }
}

#[test]
fn a_laptop_asleep_at_night_saves_once_a_day_at_a_break() {
    let mac = Mac {
        awake: awake_8_to_23,
        busy: laptop_day,
        compactions: Vec::new(),
    };
    let saves = simulate(&mac);
    assert_rules(&saves);
    // Monday's day ends at 22:00, while the laptop is in use until it
    // sleeps: the save waits for Tuesday's lunch break. It then moves 5
    // minutes a day, past the end of the lunch break on Sunday, and waits
    // for the dinner break.
    let want: Vec<i64> = [
        DAY + hm(12, 5),
        2 * DAY + hm(12, 10),
        3 * DAY + hm(12, 15),
        4 * DAY + hm(12, 20),
        5 * DAY + hm(12, 25),
        6 * DAY + hm(18, 5),
    ]
    .iter()
    .map(|t| t + SAVE_TIME)
    .collect();
    assert_eq!(saves.iter().map(|s| s.at).collect::<Vec<_>>(), want);
    assert_eq!(count(&saves, Kind::Daily), 6, "{saves:?}");
    // The longest the store went unsaved: from Sunday 22:00 to Tuesday's
    // lunch, never near the backstop's 48 h of awake time.
    let longest = gaps(&saves).into_iter().max().expect("saves");
    assert_eq!(longest, DAY + hm(14, 5) + SAVE_TIME);
}

#[test]
fn a_mac_never_idle_at_a_check_falls_back_to_the_backstop() {
    // No check ever finds a minute without input, so no daily save; the
    // engine's backstop saves after 48 h of awake time instead.
    let desktop = Mac {
        awake: always_awake,
        busy: never_idle,
        compactions: Vec::new(),
    };
    let saves = simulate(&desktop);
    assert_rules(&saves);
    assert_eq!(count(&saves, Kind::Daily), 0);
    let times: Vec<i64> = saves.iter().map(|s| s.at - SAVE_TIME).collect();
    assert_eq!(times, vec![46 * HOUR, 94 * HOUR, 142 * HOUR], "{saves:?}");
    assert!(saves.iter().all(|s| s.kind == Kind::Backstop));

    // A laptop awake 15 hours a day reaches 48 h of awake time only every
    // three and a half days: the store can be that old.
    let laptop = Mac {
        awake: awake_8_to_23,
        busy: never_idle,
        compactions: Vec::new(),
    };
    let saves = simulate(&laptop);
    assert_rules(&saves);
    assert_eq!(count(&saves, Kind::Daily), 0);
    let times: Vec<i64> = saves.iter().map(|s| s.at - SAVE_TIME).collect();
    assert_eq!(
        times,
        vec![3 * DAY + hm(10, 0), 6 * DAY + hm(13, 0)],
        "{saves:?}"
    );
    let longest = gaps(&saves).into_iter().max().expect("saves");
    assert_eq!(longest, 3 * DAY + hm(12, 0) + SAVE_TIME);
}

#[test]
fn a_compaction_save_restarts_the_daily_clock() {
    // Compactions at random moments of the week, on the office desktop.
    let mut rng = XorShift(0x2015_0d4f_5afe_c0de);
    for round in 0..50 {
        let n = 1 + rng.below(8);
        let compactions = (0..n)
            .map(|_| (rng.below((WEEK / MINUTE) as u64) as i64) * MINUTE)
            .collect();
        let mac = Mac {
            awake: always_awake,
            busy: office_hours,
            compactions,
        };
        let saves = simulate(&mac);
        // Every daily save comes a day or more after the last save of any
        // kind: a compaction adds its own save and none besides.
        assert_rules(&saves);
        assert_eq!(count(&saves, Kind::Compaction), n as usize, "round {round}");
        assert!(count(&saves, Kind::Daily) <= 7, "round {round}: {saves:?}");
        assert_eq!(count(&saves, Kind::Backstop), 0, "round {round}");
        // A day that ends during office hours, or just before them, after
        // the last idle check, waits for the end of the busy spell (at
        // most the afternoon's 4 h 15 min) and the check after it.
        let check = secs(DAILY_SAVE_CHECK);
        for gap in gaps(&saves) {
            assert!(
                gap <= secs(DAILY_SAVE_INTERVAL) + hm(4, 15) + 2 * check + SAVE_TIME,
                "round {round}: {saves:?}"
            );
        }
    }
}

#[test]
fn compactions_more_often_than_daily_leave_no_daily_save() {
    // A disk busy enough to compact every 10 hours: each compaction's save
    // restarts the day before it ends, so the saves are the compactions'
    // alone, and "one save a day plus one per compaction" is an upper
    // bound, not the count.
    let mac = Mac {
        awake: always_awake,
        busy: office_hours,
        compactions: (1..=16).map(|k| k * 10 * HOUR).collect(),
    };
    let saves = simulate(&mac);
    assert_rules(&saves);
    assert_eq!(count(&saves, Kind::Daily), 0, "{saves:?}");
    assert_eq!(count(&saves, Kind::Compaction), 16);
    assert_eq!(saves.len(), 16);
}
