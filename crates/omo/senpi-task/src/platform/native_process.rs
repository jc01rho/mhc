//! `platform/native-process.ts`: synchronous reads of another process's kernel facts, with no
//! process spawn per probe (#8096, #13165, #14808): `/proc` on Linux, `libproc` on darwin and
//! `kernel32` on Windows. Every reader answers `None` when the fact cannot be read (another
//! runtime, a gone pid, a refused handle), never a guess.

/// Linux clock ticks per second for `utime`/`stime`; 100 on every kernel these tests and users run.
#[cfg(any(target_os = "linux", test))]
const LINUX_CLK_TCK: f64 = 100.0;

#[cfg(any(target_os = "linux", test))]
pub(crate) fn parse_linux_stat_fields(stat: &str) -> Option<Vec<&str>> {
    let end = stat.rfind(')')?;
    let rest = stat.get(end + 2..)?.trim();
    if rest.is_empty() {
        return None;
    }
    Some(rest.split_whitespace().collect())
}

#[cfg(any(target_os = "linux", test))]
pub(crate) fn parse_linux_stat_start_identity(stat: &str) -> Option<String> {
    let fields = parse_linux_stat_fields(stat)?;
    let start_ticks = *fields.get(19)?;
    (!start_ticks.is_empty()).then(|| format!("linux-proc-start-ticks:{start_ticks}"))
}

#[cfg(any(target_os = "linux", test))]
pub(crate) fn parse_linux_stat_cpu_seconds(stat: &str) -> Option<f64> {
    let fields = parse_linux_stat_fields(stat)?;
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    let ticks = utime.checked_add(stime)?;
    let seconds = (ticks as f64) / LINUX_CLK_TCK;
    seconds.is_finite().then_some(seconds)
}

/// Convert Darwin `proc_taskinfo` Mach absolute-time ticks to seconds using the Mach timebase
/// (`numer / denom` nanoseconds per tick: `1 / 1` on Intel, `125 / 3` on Apple Silicon, #14808).
pub fn scale_mach_ticks(ticks: u64, numer: u32, denom: u32) -> Option<f64> {
    if denom == 0 {
        return None;
    }
    let nanoseconds_per_tick = f64::from(numer) / f64::from(denom);
    let seconds = (ticks as f64 * nanoseconds_per_tick) / 1e9;
    seconds.is_finite().then_some(seconds)
}

#[cfg(target_os = "linux")]
fn linux_stat(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()
}

#[cfg(target_os = "macos")]
mod darwin {
    pub(super) const PROC_PIDTBSDINFO: libc::c_int = 3;
    /// `sizeof(struct proc_bsdinfo)` on 64-bit darwin.
    pub(super) const PROC_BSDINFO_SIZE: usize = 136;
    /// Byte offset of `pbi_start_tvsec` inside `struct proc_bsdinfo`.
    pub(super) const START_TVSEC_OFFSET: usize = 120;

    pub(super) const PROC_PIDTASKINFO: libc::c_int = 4;
    /// `sizeof(struct proc_taskinfo)` on 64-bit darwin.
    pub(super) const PROC_TASKINFO_SIZE: usize = 96;
    /// Byte offsets of `pti_total_user` and `pti_total_system` inside `struct proc_taskinfo`.
    pub(super) const TOTAL_USER_OFFSET: usize = 16;
    pub(super) const TOTAL_SYSTEM_OFFSET: usize = 24;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub(super) struct MachTimebaseInfo {
        pub(super) numer: u32,
        pub(super) denom: u32,
    }

    unsafe extern "C" {
        fn mach_timebase_info(info: *mut MachTimebaseInfo) -> libc::c_int;
    }

    pub(super) fn timebase() -> Option<MachTimebaseInfo> {
        let mut info = MachTimebaseInfo::default();
        // SAFETY: `info` is a valid mutable stack struct matching `mach_timebase_info_data_t`.
        let status = unsafe { mach_timebase_info(&mut info) };
        (status == 0 && info.denom != 0).then_some(info)
    }

    pub(super) fn pid_info<const N: usize>(pid: u32, flavor: libc::c_int) -> Option<[u8; N]> {
        let pid = libc::c_int::try_from(pid).ok()?;
        let size = libc::c_int::try_from(N).ok()?;
        let mut buffer = [0_u8; N];
        // SAFETY: `buffer` is a valid mutable byte array of `N` bytes matching `size`.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                flavor,
                0,
                buffer.as_mut_ptr().cast::<libc::c_void>(),
                size,
            )
        };
        (written == size).then_some(buffer)
    }

    pub(super) fn read_le_u64(bytes: &[u8], offset: usize) -> Option<u64> {
        let slice = bytes.get(offset..offset + 8)?;
        let raw: [u8; 8] = slice.try_into().ok()?;
        Some(u64::from_le_bytes(raw))
    }
}

#[cfg(target_os = "windows")]
mod win32 {
    use std::ffi::c_void;

    type Handle = *mut c_void;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    impl FileTime {
        fn ticks(self) -> u64 {
            (u64::from(self.high) << 32) | u64::from(self.low)
        }
    }

    unsafe extern "system" {
        fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> Handle;
        fn GetProcessTimes(
            process: Handle,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
        fn CloseHandle(object: Handle) -> i32;
    }

    pub(super) struct ProcessTimes {
        pub(super) creation: u64,
        pub(super) kernel: u64,
        pub(super) user: u64,
    }

    pub(super) fn process_times(pid: u32) -> Option<ProcessTimes> {
        // SAFETY: `OpenProcess` reads plain integer arguments; null return is checked before use.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return None;
        }
        let mut creation = FileTime::default();
        let mut exit = FileTime::default();
        let mut kernel = FileTime::default();
        let mut user = FileTime::default();
        // SAFETY: `handle` is a live process handle and all four out-pointers are valid stack locals.
        let ok =
            unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) };
        // SAFETY: `handle` was opened above and is closed once here.
        unsafe {
            CloseHandle(handle);
        }
        (ok != 0).then(|| ProcessTimes {
            creation: creation.ticks(),
            kernel: kernel.ticks(),
            user: user.ticks(),
        })
    }
}

/// A start identity that changes when a pid is reused, or `None` when it cannot be read.
pub fn read_process_start_identity(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        return parse_linux_stat_start_identity(&linux_stat(pid)?);
    }
    #[cfg(target_os = "macos")]
    {
        let info =
            darwin::pid_info::<{ darwin::PROC_BSDINFO_SIZE }>(pid, darwin::PROC_PIDTBSDINFO)?;
        let seconds = darwin::read_le_u64(&info, darwin::START_TVSEC_OFFSET)?;
        return (seconds > 0).then(|| format!("proc-start-epoch:{seconds}"));
    }
    #[cfg(target_os = "windows")]
    {
        let times = win32::process_times(pid)?;
        return (times.creation > 0).then(|| format!("win32-creation-filetime:{}", times.creation));
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        None
    }
}

/// User plus system CPU seconds the process itself has used, or `None` when it cannot be read.
pub fn read_process_cpu_seconds(pid: u32) -> Option<f64> {
    if pid == 0 {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        return parse_linux_stat_cpu_seconds(&linux_stat(pid)?);
    }
    #[cfg(target_os = "macos")]
    {
        let info =
            darwin::pid_info::<{ darwin::PROC_TASKINFO_SIZE }>(pid, darwin::PROC_PIDTASKINFO)?;
        let timebase = darwin::timebase()?;
        let user = darwin::read_le_u64(&info, darwin::TOTAL_USER_OFFSET)?;
        let system = darwin::read_le_u64(&info, darwin::TOTAL_SYSTEM_OFFSET)?;
        return scale_mach_ticks(user.saturating_add(system), timebase.numer, timebase.denom);
    }
    #[cfg(target_os = "windows")]
    {
        let times = win32::process_times(pid)?;
        return Some((times.kernel.saturating_add(times.user) as f64) / 1e7);
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn runtime_cpu_seconds() -> f64 {
        // SAFETY: `usage` is a zeroed stack `libc::rusage` populated by `getrusage(RUSAGE_SELF, _)`.
        let usage = unsafe {
            let mut usage: libc::rusage = std::mem::zeroed();
            libc::getrusage(libc::RUSAGE_SELF, &mut usage);
            usage
        };
        let user = usage.ru_utime.tv_sec as f64 + f64::from(usage.ru_utime.tv_usec) / 1e6;
        let system = usage.ru_stime.tv_sec as f64 + f64::from(usage.ru_stime.tv_usec) / 1e6;
        user + system
    }

    // native-process.test.ts (#14808 review M1): spin until the runtime's own CPU clock counts
    // 0.5 s of CPU and assert read_process_cpu_seconds agrees within [0.8x, 1.25x].
    #[cfg(unix)]
    #[test]
    fn this_process_reads_its_own_cpu_seconds() {
        let start = runtime_cpu_seconds();
        let before = read_process_cpu_seconds(std::process::id()).expect("initial cpu reading");
        let mut acc = 1_u64;
        while runtime_cpu_seconds() - start < 0.5 {
            for step in 1..10_000_u64 {
                acc =
                    std::hint::black_box(acc.wrapping_mul(6364136223846793005).wrapping_add(step));
            }
        }
        std::hint::black_box(acc);
        let runtime_seconds = runtime_cpu_seconds() - start;
        let after = read_process_cpu_seconds(std::process::id()).expect("final cpu reading");
        let reader_seconds = after - before;
        assert!(
            reader_seconds > runtime_seconds * 0.8,
            "reader={reader_seconds} runtime={runtime_seconds}"
        );
        assert!(
            reader_seconds < runtime_seconds * 1.25,
            "reader={reader_seconds} runtime={runtime_seconds}"
        );
        assert!(read_process_start_identity(std::process::id()).is_some());
    }

    #[test]
    fn read_process_cpu_seconds_rejects_a_pid_that_is_not_a_positive_integer() {
        assert_eq!(read_process_cpu_seconds(0), None);
        assert_eq!(read_process_start_identity(0), None);
    }

    #[test]
    fn scale_mach_ticks_agrees_with_the_runtime_clock() {
        // Intel: 1 ns per tick -> 500_000_000 ticks = 0.5 s.
        let intel = scale_mach_ticks(500_000_000, 1, 1).expect("intel timebase");
        assert!((intel - 0.5).abs() < 1e-9);
        // Apple Silicon: 125/3 ns per tick -> 12_000_000 ticks = 0.5 s (unscaled was 0.012 s).
        let apple_silicon = scale_mach_ticks(12_000_000, 125, 3).expect("apple silicon timebase");
        assert!((apple_silicon - 0.5).abs() < 1e-9);
        assert_eq!(scale_mach_ticks(12_000_000, 125, 0), None);
    }

    #[test]
    fn proc_stat_cpu_ticks_survive_a_command_name_with_spaces_and_parens() {
        let stat = "42 (a (b) c) S 1 2 3 4 5 6 7 8 9 10 11 125 75 14 15 16 17 18 19 777 21";
        assert_eq!(parse_linux_stat_cpu_seconds(stat), Some(2.0));
        assert_eq!(
            parse_linux_stat_start_identity(stat).as_deref(),
            Some("linux-proc-start-ticks:777")
        );
        assert_eq!(parse_linux_stat_cpu_seconds("42 (x) S 1"), None);
    }
}
