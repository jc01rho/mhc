//! Port of senpi `terminal/process-identity.ts`: boot and own-process start instants used to
//! tell a live lease holder or watcher apart from a recycled pid.
//!
//! senpi#3152: the boot instant comes from the kernel's own boot record (`btime` in `/proc/stat` on
//! Linux, `kern.boottime` on macOS), read once per process and platform and remembered only when it
//! holds a value, instead of `now - os.uptime()`, whose whole-second rounding let two reads straddle
//! an uptime tick. A wall-clock step moves the recorded instant for every process alike, so a cached
//! value that disagrees with a peer is read again once before the peer is judged stale
//! (`same_boot_as_own`).
use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

pub use maho_tool_child::group_ownership::same_process_start_ms as same_process_start;

/// Upstream re-exports `PROCESS_START_TOLERANCE_MS` from `utils/process-identity.ts` (3 s).
pub const PROCESS_START_TOLERANCE_MS: f64 = 3_000.0;

/// Two boot instants further apart than this belong to different boots. Where the kernel's boot record
/// is readable two processes read the same value; on the `now - uptime` fallback a laptop's sleep is
/// accounted in uptime, so the only drift left there is whole-second rounding on either side.
pub const BOOT_INSTANT_TOLERANCE_MS: f64 = 120_000.0;

/// Upstream's `platform` argument for the two platforms with a kernel boot record.
pub const BOOT_PLATFORM_LINUX: &str = "linux";
pub const BOOT_PLATFORM_DARWIN: &str = "darwin";
pub const BOOT_PLATFORM_WINDOWS: &str = "win32";
fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0)
}

fn whole_seconds(ms: f64) -> f64 {
    (ms / 1000.0).round() * 1000.0
}

fn floor_to_second(ms: f64) -> f64 {
    (ms / 1000.0).floor() * 1000.0
}

/// This host's platform, spelled as upstream's `NodeJS.Platform`.
pub fn current_platform() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        BOOT_PLATFORM_LINUX
    }
    #[cfg(target_os = "macos")]
    {
        BOOT_PLATFORM_DARWIN
    }
    #[cfg(windows)]
    {
        BOOT_PLATFORM_WINDOWS
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        "other"
    }
}

/// The kernel's own boot instant in epoch seconds: `btime` in `/proc/stat` on Linux, `kern.boottime`
/// from sysctl on macOS. It is one recorded value, not `now - uptime`, so two reads agree to the second
/// however they straddle an uptime tick (senpi#3152), and a wall-clock step moves it for every process
/// alike. `None` where neither is readable.
pub fn read_kernel_boot_epoch_seconds(platform: &str) -> Option<u64> {
    match platform {
        BOOT_PLATFORM_LINUX => std::fs::read_to_string("/proc/stat").ok().and_then(|stat| {
            stat.lines().find_map(|line| {
                line.strip_prefix("btime ")
                    .and_then(|value| value.trim().parse::<u64>().ok())
            })
        }),
        BOOT_PLATFORM_DARWIN => {
            let output = std::process::Command::new("/usr/sbin/sysctl")
                .args(["-n", "kern.boottime"])
                .env("LC_ALL", "C")
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output()
                .ok()?;
            if !output.status.success() {
                return None;
            }
            let text = String::from_utf8_lossy(&output.stdout).into_owned();
            let (_, tail) = text.split_once("sec = ")?;
            tail.split(',').next()?.trim().parse::<u64>().ok()
        }
        _ => None,
    }
}

/// `os.uptime()` in integral seconds, as Node reports it: `/proc/uptime` on Linux, `kern.boottime` on
/// macOS (which is how Node derives it there), whole seconds since boot on Windows.
fn platform_uptime_seconds(platform: &str) -> Option<f64> {
    match platform {
        BOOT_PLATFORM_LINUX => std::fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|text| text.split_whitespace().next()?.parse::<f64>().ok())
            .map(f64::floor),
        BOOT_PLATFORM_DARWIN => read_kernel_boot_epoch_seconds(platform)
            .map(|boot| (now_ms() / 1000.0 - boot as f64).floor()),
        #[cfg(windows)]
        BOOT_PLATFORM_WINDOWS => Some((winsafe::GetTickCount64() / 1000) as f64),
        _ => None,
    }
}

/// The kernel boot records of one process, per platform: they cannot change while the machine is up.
/// A read that found no value is not remembered, so the next call reads again, and the uptime fallback
/// is computed on every read.
#[derive(Default)]
pub struct BootInstantCache {
    records: Mutex<BTreeMap<String, f64>>,
}

impl BootInstantCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn records(&self) -> MutexGuard<'_, BTreeMap<String, f64>> {
        self.records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The remembered kernel record of `platform`, if this process already read one.
    pub fn cached(&self, platform: &str) -> Option<f64> {
        self.records().get(platform).copied()
    }

    /// Drops the remembered record, so the next read asks the kernel again.
    pub fn forget(&self, platform: &str) {
        self.records().remove(platform);
    }

    fn remember(&self, platform: &str, boot_at_ms: f64) {
        self.records().insert(platform.to_owned(), boot_at_ms);
    }

    /// The boot instant of `platform`: the kernel's record, else `now - uptime` at whole seconds.
    pub fn read(&self, platform: &str, now: &dyn Fn() -> f64) -> f64 {
        self.read_with(
            platform,
            now,
            read_kernel_boot_epoch_seconds,
            platform_uptime_seconds,
        )
    }

    /// [`Self::read`] with the two reads injected, so a case can answer them without this host.
    pub fn read_with(
        &self,
        platform: &str,
        now: &dyn Fn() -> f64,
        kernel: impl Fn(&str) -> Option<u64>,
        uptime: impl Fn(&str) -> Option<f64>,
    ) -> f64 {
        if let Some(known) = self.cached(platform) {
            return known;
        }
        if let Some(seconds) = kernel(platform) {
            let boot_at_ms = seconds as f64 * 1000.0;
            self.remember(platform, boot_at_ms);
            return boot_at_ms;
        }
        whole_seconds(now() - uptime(platform).unwrap_or(0.0) * 1000.0)
    }

    /// A new read of the kernel's record that replaces the cache (`ownBootAtMs({ fresh: true })`).
    pub fn read_fresh(&self, platform: &str, now: &dyn Fn() -> f64) -> f64 {
        self.forget(platform);
        self.read(platform, now)
    }
}

fn boot_instants() -> &'static BootInstantCache {
    static RECORDS: OnceLock<BootInstantCache> = OnceLock::new();
    RECORDS.get_or_init(BootInstantCache::new)
}

/// `processBootAtMs(now, platform)`: `platform`'s boot instant, the kernel's record read once per
/// process; every later caller waits on the same remembered value. A resolve that found nothing is
/// dropped before it is returned, so the next call reads again.
pub fn process_boot_at_ms_for(platform: &str, now: Option<&dyn Fn() -> f64>) -> f64 {
    const DEFAULT_CLOCK: fn() -> f64 = now_ms;
    let clock: &dyn Fn() -> f64 = now.unwrap_or(&DEFAULT_CLOCK);
    boot_instants().read(platform, clock)
}

/// `processBootAtMs`: this host's boot instant.
pub fn process_boot_at_ms(now: Option<&dyn Fn() -> f64>) -> f64 {
    process_boot_at_ms_for(current_platform(), now)
}

/// This process's boot instant: the cached kernel record, or with `fresh` a new read of it that
/// replaces the cache. The kernel's recorded instant moves with a wall-clock step, so a value cached
/// before a step can be stale.
pub fn own_boot_at_ms(fresh: bool) -> f64 {
    let platform = current_platform();
    if fresh {
        boot_instants().forget(platform);
    }
    process_boot_at_ms(None)
}

/// Whether `other_boot_at_ms` belongs to this machine's current boot. A 'different boot' verdict on the
/// cached record is checked once more against a fresh read before it stands: after a wall-clock step
/// past the tolerance the cache is stale while a live peer recorded the moved instant, and calling that
/// peer's lock or claim stale would let a second owner in.
pub fn same_boot_as_own(other_boot_at_ms: Option<f64>) -> bool {
    same_boot_as_own_with(other_boot_at_ms, own_boot_at_ms)
}

/// [`same_boot_as_own`] with the own-boot reader injected: `read(true)` is a fresh read.
pub fn same_boot_as_own_with(other_boot_at_ms: Option<f64>, read: impl Fn(bool) -> f64) -> bool {
    let Some(other) = other_boot_at_ms.filter(|value| value.is_finite()) else {
        return false;
    };
    if same_boot(read(false), Some(other)) {
        return true;
    }
    same_boot(read(true), Some(other))
}

/// `process.uptime()` in ms. Linux derives it from `/proc/self/stat` start ticks (USER_HZ 100,
/// as `group_ownership::process_start_time_ms` assumes); elsewhere the origin is this module's
/// first use, the native analogue of a worker isolate's uptime origin.
fn process_uptime_ms() -> f64 {
    #[cfg(target_os = "linux")]
    {
        let start_ticks = std::fs::read_to_string("/proc/self/stat")
            .ok()
            .and_then(|stat| {
                let (_, fields) = stat.rsplit_once(") ")?;
                fields.split_whitespace().nth(19)?.parse::<f64>().ok()
            });
        let uptime = std::fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|text| text.split_whitespace().next()?.parse::<f64>().ok());
        if let (Some(ticks), Some(uptime)) = (start_ticks, uptime) {
            return (uptime - ticks / 100.0).max(0.0) * 1000.0;
        }
    }
    static FIRST_USE: OnceLock<std::time::Instant> = OnceLock::new();
    FIRST_USE
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs_f64()
        * 1000.0
}

static OWN_START_AT_LOAD: OnceLock<f64> = OnceLock::new();

/// `ownProcessStartedAtMs`: captured once (a monotonic uptime stops during a Linux suspend), and
/// floored like `ps -o lstart` so it never lands after now.
pub fn own_process_started_at_ms(now: Option<&dyn Fn() -> f64>) -> f64 {
    match now {
        None => *OWN_START_AT_LOAD.get_or_init(|| floor_to_second(now_ms() - process_uptime_ms())),
        Some(now) => floor_to_second(now() - process_uptime_ms()),
    }
}

pub fn same_boot(boot_at_ms: f64, other_boot_at_ms: Option<f64>) -> bool {
    let Some(other) = other_boot_at_ms.filter(|value| value.is_finite()) else {
        return false;
    };
    (boot_at_ms - other).abs() <= BOOT_INSTANT_TOLERANCE_MS
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildProcessIdentity {
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_group_id: Option<u32>,
    pub started_at_ms: f64,
    pub boot_at_ms: f64,
    pub argv: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn reports_a_whole_second_boot_instant_before_now_on_this_host() {
        let boot = process_boot_at_ms(None);
        assert_eq!(boot % 1000.0, 0.0);
        assert!(boot < now_ms());
        assert_eq!(process_boot_at_ms(None), boot);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn matches_the_kernel_boot_time_on_darwin_within_the_tolerance() {
        let seconds =
            read_kernel_boot_epoch_seconds(BOOT_PLATFORM_DARWIN).expect("darwin kern.boottime");
        assert!(
            (process_boot_at_ms(None) - seconds as f64 * 1000.0).abs() <= BOOT_INSTANT_TOLERANCE_MS
        );
    }

    #[test]
    fn reads_the_same_boot_instant_on_both_sides_of_an_uptime_tick() {
        for platform in [BOOT_PLATFORM_LINUX, BOOT_PLATFORM_DARWIN] {
            let cache = BootInstantCache::new();
            let now = Cell::new(5_000_900.0);
            let uptime = Cell::new(4_000.0);
            let clock = || now.get();
            let first = cache.read_with(platform, &clock, |_| Some(1_000), |_| Some(uptime.get()));
            now.set(5_001_400.0);
            uptime.set(4_001.0);
            let second = cache.read_with(platform, &clock, |_| Some(1_000), |_| Some(uptime.get()));
            assert_eq!(second, first, "{platform}");
            assert_eq!(first, 1_000_000.0, "{platform}");
        }
    }

    #[test]
    fn gives_two_readers_the_same_moved_instant_after_a_wall_clock_step() {
        for platform in [BOOT_PLATFORM_LINUX, BOOT_PLATFORM_DARWIN] {
            let clock = || 5_000_000.0;
            let reader_a =
                BootInstantCache::new().read_with(platform, &clock, |_| Some(1_300), |_| None);
            let reader_b =
                BootInstantCache::new().read_with(platform, &clock, |_| Some(1_300), |_| None);
            assert_eq!(
                (reader_a, reader_b),
                (1_300_000.0, 1_300_000.0),
                "{platform}"
            );
        }
    }

    #[test]
    fn reads_the_darwin_boot_record_with_one_sysctl_call_however_often_it_is_asked() {
        let cache = BootInstantCache::new();
        let sysctl_calls = Cell::new(0usize);
        let clock = || 5_000_000.0;
        let kernel = |_: &str| {
            sysctl_calls.set(sysctl_calls.get() + 1);
            Some(1_000)
        };
        cache.read_with(BOOT_PLATFORM_DARWIN, &clock, kernel, |_| None);
        cache.read_with(BOOT_PLATFORM_DARWIN, &clock, kernel, |_| None);
        assert_eq!(sysctl_calls.get(), 1);
    }

    #[test]
    fn retries_a_kernel_read_that_failed_instead_of_remembering_the_failure() {
        let cache = BootInstantCache::new();
        let calls = Cell::new(0usize);
        let throws_once = Cell::new(true);
        let clock = || 5_000_900.0;
        let kernel = |_: &str| {
            calls.set(calls.get() + 1);
            if throws_once.replace(false) {
                None
            } else {
                Some(1_000)
            }
        };
        let fallback = cache.read_with(BOOT_PLATFORM_DARWIN, &clock, kernel, |_| Some(4_000.0));
        let from_kernel = cache.read_with(BOOT_PLATFORM_DARWIN, &clock, kernel, |_| Some(4_000.0));
        assert_eq!(
            (fallback, from_kernel, calls.get()),
            (1_001_000.0, 1_000_000.0, 2)
        );
    }

    #[test]
    fn reads_the_kernel_boot_record_once_per_process_and_does_no_io_at_a_later_call() {
        for platform in [BOOT_PLATFORM_LINUX, BOOT_PLATFORM_DARWIN] {
            let cache = BootInstantCache::new();
            let reads = Cell::new(0usize);
            let clock = || 5_000_000.0;
            let kernel = |_: &str| {
                reads.set(reads.get() + 1);
                Some(1_000)
            };
            let first = cache.read_with(platform, &clock, kernel, |_| None);
            let reads_after_first = reads.get();
            let second = cache.read_with(platform, &clock, kernel, |_| None);
            assert_eq!(second, first, "{platform}");
            assert_eq!(reads.get() - reads_after_first, 0, "{platform}");
        }
    }

    #[test]
    fn re_reads_a_stale_cached_boot_record_before_calling_a_live_peer_from_another_boot() {
        let cache = BootInstantCache::new();
        let btime = Cell::new(1_000u64);
        let clock = || 5_000_000.0;
        let own_boot = |fresh: bool| {
            if fresh {
                cache.forget(BOOT_PLATFORM_LINUX);
            }
            cache.read_with(BOOT_PLATFORM_LINUX, &clock, |_| Some(btime.get()), |_| None)
        };
        let cached = own_boot(false);
        btime.set(btime.get() + 300);

        let verdict = same_boot_as_own_with(Some(btime.get() as f64 * 1000.0), own_boot);

        assert_eq!(
            (cached, verdict, own_boot(false)),
            (1_000_000.0, true, 1_300_000.0)
        );
    }

    #[test]
    fn reports_the_own_process_start_within_a_few_minutes_of_now() {
        let started = own_process_started_at_ms(None);
        assert_eq!(started % 1000.0, 0.0);
        assert!(started <= now_ms());
        assert!(now_ms() - started < 15.0 * 60_000.0);
    }

    #[test]
    fn floors_the_own_start_to_the_second_so_it_never_lands_after_now() {
        let fixed = 1_790_000_000_600.0 + process_uptime_ms();
        let started = {
            let clock = || fixed;
            own_process_started_at_ms(Some(&clock))
        };
        assert_eq!(started % 1000.0, 0.0);
        assert!(started <= fixed);
    }

    #[test]
    fn treats_two_boot_instants_as_the_same_boot_only_inside_the_tolerance() {
        let boot = 1_700_000_000_000.0;
        assert!(same_boot(boot, Some(boot + BOOT_INSTANT_TOLERANCE_MS)));
        assert!(same_boot(boot, Some(boot - BOOT_INSTANT_TOLERANCE_MS)));
        assert!(!same_boot(
            boot,
            Some(boot + BOOT_INSTANT_TOLERANCE_MS + 1.0)
        ));
        assert!(!same_boot(boot, None));
    }
}
