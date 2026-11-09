//! A process's CPU, energy, wakeups, disk traffic and memory over time,
//! from the kernel's per-process resource counters (`proc_pid_rusage` with
//! `RUSAGE_INFO_V6`). `fx-bench sample` uses it.
//!
//! The arithmetic is pure and portable: [`interval`] turns two
//! [`Snapshot`]s into per-second rates, [`summarize`] turns a run's
//! snapshots into a [`Summary`], [`metric_lines`] prints it as the
//! `key=value` lines the performance gate reads, and [`Bursts`] decides
//! when to capture a stack sample. Reading the counters and the sampling
//! loop (`run`, `Options`) exist on macOS only.
//!
//! Every rate divides by the real time between two readings, not the
//! nominal interval: a burst capture blocks the loop for several seconds,
//! and the interval around it is longer than the others.

#[cfg(target_os = "macos")]
use std::fs::OpenOptions;
#[cfg(target_os = "macos")]
use std::io::{self, Write};
#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::{Command, Stdio};
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Seconds in a day, for the disk-writes-per-day projection.
const DAY_S: f64 = 86_400.0;

/// The most stack samples one run captures.
pub const MAX_BURSTS: usize = 3;

/// How long `/usr/bin/sample` records a burst, in seconds.
pub const BURST_SAMPLE_S: u32 = 3;

/// Cumulative counters of one process at one moment, times in nanoseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Snapshot {
    /// Monotonic time of the reading in nanoseconds, from any fixed origin.
    pub t_ns: u64,
    /// Wall-clock time of the reading, seconds since the Unix epoch; only
    /// written to the CSV.
    pub epoch_s: f64,
    pub user_ns: u64,
    pub system_ns: u64,
    /// CPU energy, in nanojoules.
    pub energy_nj: u64,
    /// The part of `energy_nj` spent on performance cores.
    pub penergy_nj: u64,
    /// Wakeups from package idle.
    pub pkg_idle_wkups: u64,
    /// Wakeups by interrupt.
    pub interrupt_wkups: u64,
    pub disk_read_bytes: u64,
    pub disk_written_bytes: u64,
    pub logical_writes_bytes: u64,
    /// Physical footprint: the memory figure Activity Monitor shows.
    pub phys_footprint: u64,
    /// The largest footprint since the kernel last reset this counter.
    pub interval_max_phys_footprint: u64,
    pub lifetime_max_phys_footprint: u64,
    pub resident_bytes: u64,
    pub instructions: u64,
    pub cycles: u64,
    /// Time spent runnable but waiting for a core.
    pub runnable_ns: u64,
    /// CPU time per QoS class: default, maintenance, background, utility,
    /// legacy, user-initiated, user-interactive.
    pub qos_ns: [u64; 7],
}

/// The header of `rusage.csv`; [`rusage_csv_row`] writes its rows.
pub const RUSAGE_HEADER: &str = "epoch_s,elapsed_s,user_ns,system_ns,energy_nj,penergy_nj,\
pkg_idle_wkups,interrupt_wkups,disk_read_bytes,disk_written_bytes,logical_writes_bytes,\
phys_footprint_bytes,interval_max_phys_footprint_bytes,lifetime_max_phys_footprint_bytes,\
resident_bytes,instructions,cycles,runnable_ns,qos_default_ns,qos_maintenance_ns,\
qos_background_ns,qos_utility_ns,qos_legacy_ns,qos_user_initiated_ns,qos_user_interactive_ns";

/// One `rusage.csv` row: the reading's time (epoch, and seconds since the
/// first reading at `t0_ns`) and its cumulative counters.
pub fn rusage_csv_row(s: &Snapshot, t0_ns: u64) -> String {
    let mut row = format!(
        "{:.3},{:.3},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
        s.epoch_s,
        seconds(s.t_ns.saturating_sub(t0_ns)),
        s.user_ns,
        s.system_ns,
        s.energy_nj,
        s.penergy_nj,
        s.pkg_idle_wkups,
        s.interrupt_wkups,
        s.disk_read_bytes,
        s.disk_written_bytes,
        s.logical_writes_bytes,
        s.phys_footprint,
        s.interval_max_phys_footprint,
        s.lifetime_max_phys_footprint,
        s.resident_bytes,
        s.instructions,
        s.cycles,
        s.runnable_ns,
    );
    for q in s.qos_ns {
        row.push_str(&format!(",{q}"));
    }
    row
}

/// What happened between two readings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Interval {
    /// Start, in seconds since the run's first reading.
    pub start_s: f64,
    /// Real length, in seconds.
    pub seconds: f64,
    /// CPU time (user + system) as a percentage of one core.
    pub cpu_pct: f64,
    /// Average CPU power, in milliwatts.
    pub energy_mw: f64,
    /// Package-idle plus interrupt wakeups per second.
    pub wakeups_per_s: f64,
    pub disk_read_bytes: u64,
    pub disk_written_bytes: u64,
    /// Physical footprint at the end of the interval.
    pub footprint_bytes: u64,
}

/// The header of `intervals.csv`; [`interval_csv_row`] writes its rows.
pub const INTERVALS_HEADER: &str = "start_s,seconds,cpu_pct,energy_mw,wakeups_per_s,\
disk_read_bytes,disk_written_bytes,footprint_bytes";

/// One `intervals.csv` row.
pub fn interval_csv_row(iv: &Interval) -> String {
    format!(
        "{:.3},{:.3},{:.3},{:.3},{:.3},{},{},{}",
        iv.start_s,
        iv.seconds,
        iv.cpu_pct,
        iv.energy_mw,
        iv.wakeups_per_s,
        iv.disk_read_bytes,
        iv.disk_written_bytes,
        iv.footprint_bytes
    )
}

/// The interval from reading `a` to reading `b`, with `t0_ns` the run's
/// first reading. `None` when `b` is not later than `a`. A counter that
/// went down (which the kernel shouldn't do) counts as no change.
pub fn interval(a: &Snapshot, b: &Snapshot, t0_ns: u64) -> Option<Interval> {
    let dt_ns = b.t_ns.checked_sub(a.t_ns).filter(|&d| d > 0)?;
    let secs = seconds(dt_ns);
    Some(Interval {
        start_s: seconds(a.t_ns.saturating_sub(t0_ns)),
        seconds: secs,
        cpu_pct: 100.0 * cpu_ns(b).saturating_sub(cpu_ns(a)) as f64 / dt_ns as f64,
        energy_mw: mw(b.energy_nj.saturating_sub(a.energy_nj), secs),
        wakeups_per_s: wakeups(b).saturating_sub(wakeups(a)) as f64 / secs,
        disk_read_bytes: b.disk_read_bytes.saturating_sub(a.disk_read_bytes),
        disk_written_bytes: b.disk_written_bytes.saturating_sub(a.disk_written_bytes),
        footprint_bytes: b.phys_footprint,
    })
}

fn seconds(ns: u64) -> f64 {
    ns as f64 / 1e9
}

fn cpu_ns(s: &Snapshot) -> u64 {
    s.user_ns.saturating_add(s.system_ns)
}

fn wakeups(s: &Snapshot) -> u64 {
    s.pkg_idle_wkups.saturating_add(s.interrupt_wkups)
}

/// Average power in milliwatts of `nj` nanojoules over `secs` seconds.
fn mw(nj: u64, secs: f64) -> f64 {
    nj as f64 / 1e6 / secs
}

/// A whole run, as the gate reads it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Summary {
    pub samples: usize,
    /// From the first reading to the last.
    pub duration_s: f64,
    /// The rates below need two readings; `None` with only one.
    pub cpu_pct: Option<f64>,
    pub cpu_max_interval_pct: Option<f64>,
    pub energy_mw: Option<f64>,
    pub wakeups_per_s: Option<f64>,
    pub disk_written_bytes: Option<u64>,
    /// `disk_written_bytes` scaled to 86,400 s.
    pub disk_written_per_day_bytes: Option<f64>,
    /// Physical footprint at the last reading.
    pub footprint_bytes: u64,
    /// The largest physical footprint of any reading, or the kernel's
    /// interval maximum when that is larger.
    pub footprint_max_bytes: u64,
    /// Whether the process exited before the run's end.
    pub process_exited: bool,
}

/// Summarizes a run's readings, in time order. `None` without readings.
pub fn summarize(snaps: &[Snapshot], process_exited: bool) -> Option<Summary> {
    let first = snaps.first()?;
    let last = snaps.last()?;
    let span_ns = last.t_ns.saturating_sub(first.t_ns);
    let whole = interval(first, last, first.t_ns);
    let busiest = snaps
        .windows(2)
        .filter_map(|w| interval(&w[0], &w[1], first.t_ns))
        .map(|iv| iv.cpu_pct)
        .reduce(f64::max);
    let written = whole.map(|_| {
        last.disk_written_bytes
            .saturating_sub(first.disk_written_bytes)
    });
    Some(Summary {
        samples: snaps.len(),
        duration_s: seconds(span_ns),
        cpu_pct: whole.map(|iv| iv.cpu_pct),
        cpu_max_interval_pct: busiest,
        energy_mw: whole.map(|iv| iv.energy_mw),
        wakeups_per_s: whole.map(|iv| iv.wakeups_per_s),
        disk_written_bytes: written,
        disk_written_per_day_bytes: written.map(|w| w as f64 * DAY_S / seconds(span_ns)),
        footprint_bytes: last.phys_footprint,
        footprint_max_bytes: snaps
            .iter()
            .map(|s| s.phys_footprint.max(s.interval_max_phys_footprint))
            .max()
            .unwrap_or_default(),
        process_exited,
    })
}

/// The run's `key=value` lines, each key prefixed with `label` and `_`:
/// `cpu_pct`, `cpu_max_interval_pct`, `energy_mw`, `wakeups_per_s`,
/// `disk_written_bytes`, `disk_written_per_day_bytes`, `footprint_bytes`,
/// `footprint_max_bytes`, `duration_s`, `samples`, and
/// `process_exited=true` when the process exited. A rate that needs two
/// readings is left out after only one, so the gate reports it as not
/// measured instead of reading a zero.
pub fn metric_lines(label: &str, s: &Summary) -> Vec<String> {
    let mut lines = Vec::new();
    let mut put = |key: &str, value: Option<String>| {
        if let Some(v) = value {
            lines.push(format!("{label}_{key}={v}"));
        }
    };
    let f3 = |v: f64| format!("{v:.3}");
    put("cpu_pct", s.cpu_pct.map(f3));
    put("cpu_max_interval_pct", s.cpu_max_interval_pct.map(f3));
    put("energy_mw", s.energy_mw.map(f3));
    put("wakeups_per_s", s.wakeups_per_s.map(f3));
    put(
        "disk_written_bytes",
        s.disk_written_bytes.map(|v| v.to_string()),
    );
    put(
        "disk_written_per_day_bytes",
        s.disk_written_per_day_bytes.map(|v| format!("{v:.0}")),
    );
    put("footprint_bytes", Some(s.footprint_bytes.to_string()));
    put(
        "footprint_max_bytes",
        Some(s.footprint_max_bytes.to_string()),
    );
    put("duration_s", Some(f3(s.duration_s)));
    put("samples", Some(s.samples.to_string()));
    if s.process_exited {
        put("process_exited", Some("true".to_string()));
    }
    lines
}

/// Decides when to capture a stack sample: once at the start of each burst
/// (a run of consecutive intervals above the threshold), for at most a fixed
/// number of bursts, so the captures stay few and each shows a different
/// burst's cause.
#[derive(Clone, Debug, PartialEq)]
pub struct Bursts {
    threshold_pct: f64,
    max: usize,
    hot: bool,
    taken: usize,
}

impl Bursts {
    /// Bursts are intervals whose CPU is above `threshold_pct`; at most
    /// `max` are captured.
    pub fn new(threshold_pct: f64, max: usize) -> Bursts {
        Bursts {
            threshold_pct,
            max,
            hot: false,
            taken: 0,
        }
    }

    /// Records one interval's CPU. Returns the burst's number (1-based)
    /// when a capture should start now.
    pub fn observe(&mut self, cpu_pct: f64) -> Option<usize> {
        if cpu_pct <= self.threshold_pct {
            self.hot = false;
            return None;
        }
        if self.hot {
            return None;
        }
        self.hot = true;
        if self.taken >= self.max {
            return None;
        }
        self.taken += 1;
        Some(self.taken)
    }
}

/// Converts mach absolute time units to nanoseconds with the timebase
/// `numer / denom` (1/1 on Intel, 125/3 on Apple silicon).
pub fn mach_to_ns(ticks: u64, numer: u32, denom: u32) -> u64 {
    let ns = u128::from(ticks) * u128::from(numer) / u128::from(denom.max(1));
    u64::try_from(ns).unwrap_or(u64::MAX)
}

/// The kernel interface, declared from the macOS SDK headers
/// (`sys/resource.h`, `libproc.h`, `mach/mach_time.h`).
#[cfg(target_os = "macos")]
mod sys {
    use std::ffi::{c_int, c_void};
    use std::io;

    /// `RUSAGE_INFO_V6` (`sys/resource.h`).
    const RUSAGE_INFO_V6: c_int = 6;

    /// `ESRCH` (`sys/errno.h`): no such process.
    pub const ESRCH: i32 = 3;

    /// `struct rusage_info_v6` from `sys/resource.h`, field for field. The
    /// `ri_*_time` fields are in mach absolute time units.
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct RusageInfoV6 {
        pub ri_uuid: [u8; 16],
        pub ri_user_time: u64,
        pub ri_system_time: u64,
        pub ri_pkg_idle_wkups: u64,
        pub ri_interrupt_wkups: u64,
        pub ri_pageins: u64,
        pub ri_wired_size: u64,
        pub ri_resident_size: u64,
        pub ri_phys_footprint: u64,
        pub ri_proc_start_abstime: u64,
        pub ri_proc_exit_abstime: u64,
        pub ri_child_user_time: u64,
        pub ri_child_system_time: u64,
        pub ri_child_pkg_idle_wkups: u64,
        pub ri_child_interrupt_wkups: u64,
        pub ri_child_pageins: u64,
        pub ri_child_elapsed_abstime: u64,
        pub ri_diskio_bytesread: u64,
        pub ri_diskio_byteswritten: u64,
        pub ri_cpu_time_qos_default: u64,
        pub ri_cpu_time_qos_maintenance: u64,
        pub ri_cpu_time_qos_background: u64,
        pub ri_cpu_time_qos_utility: u64,
        pub ri_cpu_time_qos_legacy: u64,
        pub ri_cpu_time_qos_user_initiated: u64,
        pub ri_cpu_time_qos_user_interactive: u64,
        pub ri_billed_system_time: u64,
        pub ri_serviced_system_time: u64,
        pub ri_logical_writes: u64,
        pub ri_lifetime_max_phys_footprint: u64,
        pub ri_instructions: u64,
        pub ri_cycles: u64,
        pub ri_billed_energy: u64,
        pub ri_serviced_energy: u64,
        pub ri_interval_max_phys_footprint: u64,
        pub ri_runnable_time: u64,
        pub ri_flags: u64,
        pub ri_user_ptime: u64,
        pub ri_system_ptime: u64,
        pub ri_pinstructions: u64,
        pub ri_pcycles: u64,
        pub ri_energy_nj: u64,
        pub ri_penergy_nj: u64,
        pub ri_secure_time_in_system: u64,
        pub ri_secure_ptime_in_system: u64,
        pub ri_neural_footprint: u64,
        pub ri_lifetime_max_neural_footprint: u64,
        pub ri_interval_max_neural_footprint: u64,
        pub ri_conclave_footprint: u64,
        pub ri_page_wait_time_mach: u64,
        pub ri_page_cache_hits: u64,
        pub ri_reserved: [u64; 6],
    }

    // The kernel writes `sizeof(struct rusage_info_v6)` bytes, 464 in the
    // SDK; a smaller struct here would let it write past the end.
    const _: () = assert!(std::mem::size_of::<RusageInfoV6>() == 464);

    /// `struct mach_timebase_info` (`mach/mach_time.h`).
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    struct MachTimebaseInfo {
        numer: u32,
        denom: u32,
    }

    unsafe extern "C" {
        fn proc_pid_rusage(pid: c_int, flavor: c_int, buffer: *mut c_void) -> c_int;
        fn mach_timebase_info(info: *mut MachTimebaseInfo) -> c_int;
        /// Takes no arguments and only reads a clock.
        safe fn mach_absolute_time() -> u64;
    }

    /// The counters of process `pid`. Works for a process of the same user
    /// and for one that has exited but not yet been reaped (a zombie);
    /// fails with `ESRCH` once it is gone.
    pub fn rusage(pid: i32) -> io::Result<RusageInfoV6> {
        let mut info = RusageInfoV6::default();
        // SAFETY: `info` is a live, writable `rusage_info_v6` laid out as in
        // the SDK header (its size is asserted above), which is exactly what
        // the kernel fills for the `RUSAGE_INFO_V6` flavor.
        let rc = unsafe { proc_pid_rusage(pid, RUSAGE_INFO_V6, (&raw mut info).cast()) };
        if rc == 0 {
            Ok(info)
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// The mach timebase as `(numer, denom)`.
    pub fn timebase() -> io::Result<(u32, u32)> {
        let mut tb = MachTimebaseInfo::default();
        // SAFETY: `tb` is a live, writable `mach_timebase_info` struct.
        let kr = unsafe { mach_timebase_info(&raw mut tb) };
        if kr != 0 || tb.numer == 0 || tb.denom == 0 {
            return Err(io::Error::other(format!(
                "mach_timebase_info failed ({kr})"
            )));
        }
        Ok((tb.numer, tb.denom))
    }

    /// The current mach absolute time, the clock `ri_proc_exit_abstime` uses.
    pub fn now_ticks() -> u64 {
        mach_absolute_time()
    }
}

/// The settings of a sampling run.
#[cfg(target_os = "macos")]
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub pid: i32,
    pub duration: Duration,
    pub interval: Duration,
    /// Prefix of every metric key.
    pub label: String,
    /// CPU percentage of one core above which an interval is a burst.
    pub burst_pct: f64,
    /// The new folder for `rusage.csv`, `intervals.csv`, `metrics.txt` and
    /// the burst captures.
    pub out: PathBuf,
}

/// A reading of the process.
#[cfg(target_os = "macos")]
enum Reading {
    Live(Snapshot),
    /// The process has exited: its final counters while it is a zombie,
    /// nothing once it is reaped.
    Exited(Option<Snapshot>),
}

#[cfg(target_os = "macos")]
fn read(pid: i32, (numer, denom): (u32, u32)) -> io::Result<Reading> {
    let now_ticks = sys::now_ticks();
    let epoch_s = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    let info = match sys::rusage(pid) {
        Ok(info) => info,
        Err(e) if e.raw_os_error() == Some(sys::ESRCH) => return Ok(Reading::Exited(None)),
        Err(e) => return Err(e),
    };
    let ns = |ticks: u64| mach_to_ns(ticks, numer, denom);
    let mut s = Snapshot {
        t_ns: ns(now_ticks),
        epoch_s,
        user_ns: ns(info.ri_user_time),
        system_ns: ns(info.ri_system_time),
        energy_nj: info.ri_energy_nj,
        penergy_nj: info.ri_penergy_nj,
        pkg_idle_wkups: info.ri_pkg_idle_wkups,
        interrupt_wkups: info.ri_interrupt_wkups,
        disk_read_bytes: info.ri_diskio_bytesread,
        disk_written_bytes: info.ri_diskio_byteswritten,
        logical_writes_bytes: info.ri_logical_writes,
        phys_footprint: info.ri_phys_footprint,
        interval_max_phys_footprint: info.ri_interval_max_phys_footprint,
        lifetime_max_phys_footprint: info.ri_lifetime_max_phys_footprint,
        resident_bytes: info.ri_resident_size,
        instructions: info.ri_instructions,
        cycles: info.ri_cycles,
        runnable_ns: ns(info.ri_runnable_time),
        qos_ns: [
            ns(info.ri_cpu_time_qos_default),
            ns(info.ri_cpu_time_qos_maintenance),
            ns(info.ri_cpu_time_qos_background),
            ns(info.ri_cpu_time_qos_utility),
            ns(info.ri_cpu_time_qos_legacy),
            ns(info.ri_cpu_time_qos_user_initiated),
            ns(info.ri_cpu_time_qos_user_interactive),
        ],
    };
    if info.ri_proc_exit_abstime == 0 {
        return Ok(Reading::Live(s));
    }
    // A zombie's counters stopped at its exit: date them there, so the
    // time after the exit doesn't dilute the last interval's rates.
    let exit_ns = ns(info.ri_proc_exit_abstime);
    if exit_ns < s.t_ns {
        s.epoch_s -= seconds(s.t_ns - exit_ns);
        s.t_ns = exit_ns;
    }
    Ok(Reading::Exited(Some(s)))
}

/// Samples `opts.pid` every `opts.interval` until `opts.duration` has
/// passed or the process exits, writing `rusage.csv` and `intervals.csv`
/// row by row into the new folder `opts.out` (so an interrupted run keeps
/// its data), then `metrics.txt`. `log` receives progress notes (bursts).
///
/// The process is read once before `opts.out` is created, so a wrong pid
/// leaves nothing behind. When an interval's CPU passes `opts.burst_pct`,
/// `/usr/bin/sample` records the process's stacks for 3 s into
/// `burst-<n>.txt`, once per burst and at most [`MAX_BURSTS`] times; a
/// capture that fails is noted and the run goes on.
#[cfg(target_os = "macos")]
pub fn run(opts: &Options, log: &mut dyn FnMut(&str)) -> io::Result<Summary> {
    let tb = sys::timebase()?;
    let first = match read(opts.pid, tb)? {
        Reading::Live(s) => s,
        Reading::Exited(_) => {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no running process with pid {}", opts.pid),
            ));
        }
    };
    std::fs::create_dir(&opts.out)?;
    let mut rusage_csv = new_file(&opts.out.join("rusage.csv"))?;
    let mut intervals_csv = new_file(&opts.out.join("intervals.csv"))?;
    writeln!(rusage_csv, "{RUSAGE_HEADER}")?;
    writeln!(intervals_csv, "{INTERVALS_HEADER}")?;
    writeln!(rusage_csv, "{}", rusage_csv_row(&first, first.t_ns))?;

    let t0 = first.t_ns;
    let mut snaps = vec![first];
    let mut bursts = Bursts::new(opts.burst_pct, MAX_BURSTS);
    let mut exited = false;
    let start = Instant::now();
    let interval_ns = opts.interval.as_nanos().max(1);
    let mut tick: u128 = 0;
    loop {
        // The next tick still ahead: after a slow burst capture the loop
        // skips the ticks it missed instead of reading several in a row.
        tick = (tick + 1).max(start.elapsed().as_nanos() / interval_ns + 1);
        let due = Duration::from_nanos(u64::try_from(tick * interval_ns).unwrap_or(u64::MAX));
        let last_tick = due >= opts.duration;
        sleep_until(start + due.min(opts.duration));

        let (snap, gone) = match read(opts.pid, tb)? {
            Reading::Live(s) => (Some(s), false),
            Reading::Exited(s) => (s, true),
        };
        let prev = snaps[snaps.len() - 1];
        if let Some(s) = snap.filter(|s| s.t_ns > prev.t_ns) {
            writeln!(rusage_csv, "{}", rusage_csv_row(&s, t0))?;
            if let Some(iv) = interval(&prev, &s, t0) {
                writeln!(intervals_csv, "{}", interval_csv_row(&iv))?;
                if let Some(n) = bursts.observe(iv.cpu_pct).filter(|_| !gone && !last_tick) {
                    capture_burst(opts, n, &iv, log);
                }
            }
            snaps.push(s);
        }
        if gone {
            exited = true;
            break;
        }
        if last_tick {
            break;
        }
    }

    let summary = summarize(&snaps, exited).expect("the first reading is always kept");
    let mut metrics = new_file(&opts.out.join("metrics.txt"))?;
    for line in metric_lines(&opts.label, &summary) {
        writeln!(metrics, "{line}")?;
    }
    Ok(summary)
}

/// Creates a file that must not exist yet.
#[cfg(target_os = "macos")]
fn new_file(path: &Path) -> io::Result<std::fs::File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

#[cfg(target_os = "macos")]
fn sleep_until(t: Instant) {
    let now = Instant::now();
    if t > now {
        std::thread::sleep(t - now);
    }
}

/// Runs `/usr/bin/sample PID 3 -file OUT/burst-<n>.txt`, which blocks for
/// about 3 s. Its own output is discarded so stdout keeps only metric lines.
#[cfg(target_os = "macos")]
fn capture_burst(opts: &Options, n: usize, iv: &Interval, log: &mut dyn FnMut(&str)) {
    let file = opts.out.join(format!("burst-{n}.txt"));
    log(&format!(
        "burst {n} at {:.1} s ({:.0}% CPU): recording stacks into {}",
        iv.start_s + iv.seconds,
        iv.cpu_pct,
        file.display()
    ));
    let status = Command::new("/usr/bin/sample")
        .arg(opts.pid.to_string())
        .arg(BURST_SAMPLE_S.to_string())
        .arg("-file")
        .arg(&file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => log(&format!("burst {n}: /usr/bin/sample exited with {s}")),
        Err(e) => log(&format!("burst {n}: cannot run /usr/bin/sample: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = 1_000_000_000;

    fn snap(
        t_s: u64,
        cpu_ms: u64,
        energy_mj: u64,
        wakeups: u64,
        written: u64,
        fp: u64,
    ) -> Snapshot {
        Snapshot {
            t_ns: t_s * S,
            user_ns: cpu_ms * 1_000_000 / 2,
            system_ns: cpu_ms * 1_000_000 / 2,
            energy_nj: energy_mj * 1_000_000,
            pkg_idle_wkups: wakeups / 2,
            interrupt_wkups: wakeups - wakeups / 2,
            disk_written_bytes: written,
            disk_read_bytes: written * 2,
            phys_footprint: fp,
            ..Snapshot::default()
        }
    }

    #[test]
    fn an_interval_gives_rates_per_real_second() {
        // 10 s, 2.5 s of CPU, 7 J, 300 wakeups, 4096 bytes written.
        let a = snap(100, 1_000, 500, 50, 1_000, 10);
        let b = snap(110, 3_500, 7_500, 350, 5_096, 20);
        let iv = interval(&a, &b, 90 * S).expect("later reading");
        assert_eq!(iv.start_s, 10.0);
        assert_eq!(iv.seconds, 10.0);
        assert!((iv.cpu_pct - 25.0).abs() < 1e-9, "{iv:?}");
        assert!((iv.energy_mw - 700.0).abs() < 1e-9, "{iv:?}");
        assert!((iv.wakeups_per_s - 30.0).abs() < 1e-9, "{iv:?}");
        assert_eq!(iv.disk_written_bytes, 4_096);
        assert_eq!(iv.disk_read_bytes, 8_192);
        assert_eq!(iv.footprint_bytes, 20);
    }

    #[test]
    fn a_long_interval_divides_by_its_real_length() {
        // A burst capture made this interval 13 s instead of 10.
        let a = snap(0, 0, 0, 0, 0, 1);
        let b = snap(13, 13_000, 0, 0, 0, 1);
        let iv = interval(&a, &b, 0).unwrap();
        assert!((iv.cpu_pct - 100.0).abs() < 1e-9);
        assert_eq!(iv.seconds, 13.0);
    }

    #[test]
    fn no_interval_without_time_passing_and_no_negative_counts() {
        let a = snap(5, 100, 100, 100, 100, 1);
        assert_eq!(interval(&a, &a, 0), None);
        let earlier = snap(4, 0, 0, 0, 0, 1);
        assert_eq!(interval(&a, &earlier, 0), None);
        let later_but_lower = snap(6, 0, 0, 0, 0, 1);
        let iv = interval(&a, &later_but_lower, 0).unwrap();
        assert_eq!(iv.cpu_pct, 0.0);
        assert_eq!(iv.disk_written_bytes, 0);
    }

    #[test]
    fn a_summary_covers_the_whole_run() {
        let mut snaps = vec![
            snap(0, 0, 0, 0, 0, 100),
            snap(10, 1_000, 2_000, 100, 4_000, 300),
            snap(20, 7_000, 3_000, 150, 4_000, 200),
            snap(30, 7_500, 6_000, 300, 6_000, 250),
        ];
        snaps[2].interval_max_phys_footprint = 900;
        let s = summarize(&snaps, false).unwrap();
        assert_eq!(s.samples, 4);
        assert_eq!(s.duration_s, 30.0);
        assert!((s.cpu_pct.unwrap() - 25.0).abs() < 1e-9, "{s:?}");
        assert!(
            (s.cpu_max_interval_pct.unwrap() - 60.0).abs() < 1e-9,
            "{s:?}"
        );
        assert!((s.energy_mw.unwrap() - 200.0).abs() < 1e-9, "{s:?}");
        assert!((s.wakeups_per_s.unwrap() - 10.0).abs() < 1e-9, "{s:?}");
        assert_eq!(s.disk_written_bytes, Some(6_000));
        let per_day = s.disk_written_per_day_bytes.unwrap();
        assert!((per_day - 6_000.0 * 2_880.0).abs() < 1e-6, "{per_day}");
        assert_eq!(s.footprint_bytes, 250);
        assert_eq!(s.footprint_max_bytes, 900);
        assert!(!s.process_exited);
    }

    #[test]
    fn footprint_max_is_the_largest_reading_without_a_kernel_maximum() {
        let snaps = [
            snap(0, 0, 0, 0, 0, 100),
            snap(1, 0, 0, 0, 0, 700),
            snap(2, 0, 0, 0, 0, 50),
        ];
        assert_eq!(summarize(&snaps, false).unwrap().footprint_max_bytes, 700);
    }

    #[test]
    fn one_reading_has_no_rates() {
        let s = summarize(&[snap(3, 10, 10, 10, 10, 42)], true).unwrap();
        assert_eq!(s.samples, 1);
        assert_eq!(s.duration_s, 0.0);
        assert_eq!(s.cpu_pct, None);
        assert_eq!(s.cpu_max_interval_pct, None);
        assert_eq!(s.disk_written_bytes, None);
        assert_eq!(s.footprint_bytes, 42);
        assert!(summarize(&[], false).is_none());

        let lines = metric_lines("app", &s);
        assert_eq!(
            lines,
            [
                "app_footprint_bytes=42",
                "app_footprint_max_bytes=42",
                "app_duration_s=0.000",
                "app_samples=1",
                "app_process_exited=true",
            ]
        );
    }

    #[test]
    fn metric_lines_carry_the_label_and_every_key() {
        let snaps = [
            snap(0, 0, 0, 0, 0, 100),
            snap(10, 1_000, 5_000, 100, 2_000, 300),
        ];
        let lines = metric_lines("idle", &summarize(&snaps, false).unwrap());
        assert_eq!(
            lines,
            [
                "idle_cpu_pct=10.000",
                "idle_cpu_max_interval_pct=10.000",
                "idle_energy_mw=500.000",
                "idle_wakeups_per_s=10.000",
                "idle_disk_written_bytes=2000",
                "idle_disk_written_per_day_bytes=17280000",
                "idle_footprint_bytes=300",
                "idle_footprint_max_bytes=300",
                "idle_duration_s=10.000",
                "idle_samples=2",
            ]
        );
    }

    #[test]
    fn bursts_capture_once_per_burst_and_at_most_three() {
        let mut b = Bursts::new(150.0, MAX_BURSTS);
        let cpu = [
            10.0, 150.0, 151.0, 300.0, 20.0, 200.0, 10.0, 160.0, 10.0, 170.0, 180.0, 10.0,
        ];
        let got: Vec<Option<usize>> = cpu.iter().map(|&c| b.observe(c)).collect();
        assert_eq!(
            got,
            [
                None,
                None,
                Some(1),
                None,
                None,
                Some(2),
                None,
                Some(3),
                None,
                None,
                None,
                None
            ]
        );
    }

    #[test]
    fn mach_time_converts_with_the_timebase() {
        assert_eq!(mach_to_ns(24_000_000, 125, 3), 1_000_000_000);
        assert_eq!(mach_to_ns(1_000, 1, 1), 1_000);
        assert_eq!(mach_to_ns(u64::MAX, 125, 3), u64::MAX);
    }

    #[test]
    fn csv_rows_match_their_headers() {
        let s = snap(12, 5, 6, 7, 8, 9);
        let row = rusage_csv_row(&s, 2 * S);
        assert_eq!(
            row.split(',').count(),
            RUSAGE_HEADER.split(',').count(),
            "{row}"
        );
        assert!(
            row.starts_with("0.000,10.000,2500000,2500000,6000000,"),
            "{row}"
        );
        let iv = interval(&snap(0, 0, 0, 0, 0, 1), &s, 0).unwrap();
        let row = interval_csv_row(&iv);
        assert_eq!(
            row.split(',').count(),
            INTERVALS_HEADER.split(',').count(),
            "{row}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn reads_this_process() {
        let tb = sys::timebase().expect("timebase");
        let Reading::Live(a) = read(std::process::id() as i32, tb).expect("read self") else {
            panic!("this process is running");
        };
        // Burn a little CPU so the counters move.
        let mut x = 0u64;
        for i in 0..5_000_000u64 {
            x = x.wrapping_add(std::hint::black_box(i));
        }
        std::hint::black_box(x);
        let Reading::Live(b) = read(std::process::id() as i32, tb).expect("read self") else {
            panic!("this process is running");
        };
        assert!(b.t_ns > a.t_ns);
        assert!(b.user_ns + b.system_ns > a.user_ns + a.system_ns);
        assert!(b.phys_footprint > 0);
    }
}
