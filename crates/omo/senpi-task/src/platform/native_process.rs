//! `platform/native-process.ts`: synchronous reads of another process's kernel facts, with no
//! process spawn per probe (#8096, #13165, #14808): `/proc` on Linux, `libproc` on darwin and
//! `kernel32` on Windows. Every reader answers `None` when the fact cannot be read (another
//! runtime, a gone pid, a refused handle), never a guess.

/// USER_HZ every mainstream Linux kernel uses; only the answer when `sysconf` cannot be read.
#[cfg(any(target_os = "linux", test))]
const FALLBACK_LINUX_CLK_TCK: f64 = 100.0;

/// Linux clock ticks per second for `/proc` `utime`/`stime`, read once from
/// `sysconf(_SC_CLK_TCK)` instead of assuming 100 (#14821).
#[cfg(any(target_os = "linux", test))]
fn linux_clock_ticks_per_second() -> f64 {
    static TICKS: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *TICKS.get_or_init(|| {
        #[cfg(target_os = "linux")]
        {
            // SAFETY: `sysconf` takes one plain integer argument and returns a `long`; no pointer is
            // passed, and a non-positive answer is checked before it is used.
            let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
            if ticks > 0 {
                return ticks as f64;
            }
        }
        FALLBACK_LINUX_CLK_TCK
    })
}

#[cfg(any(target_os = "linux", test))]
pub(crate) fn parse_linux_stat_fields(stat: &str) -> Option<Vec<&str>> {
    let end = stat.rfind(')')?;
    let rest = stat.get(end + 2..)?.trim();
    if rest.is_empty() {
        return None;
    }
    Some(rest.split_whitespace().collect())
}

/// The parent pid a `/proc/<pid>/stat` line names: the first field after the command name.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn parse_linux_stat_parent_pid(stat: &str) -> Option<u32> {
    let fields = parse_linux_stat_fields(stat)?;
    fields.get(1)?.parse().ok()
}

/// When the process started, in boot clock ticks (a unit that orders processes by creation).
#[cfg(any(target_os = "linux", test))]
pub(crate) fn parse_linux_stat_start_time(stat: &str) -> Option<u64> {
    let fields = parse_linux_stat_fields(stat)?;
    fields.get(19)?.parse().ok()
}

#[cfg(any(target_os = "linux", test))]
pub(crate) fn parse_linux_stat_cpu_seconds(stat: &str) -> Option<f64> {
    let fields = parse_linux_stat_fields(stat)?;
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    let ticks = utime.checked_add(stime)?;
    let seconds = (ticks as f64) / linux_clock_ticks_per_second();
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
    struct MachTimebaseInfo {
        pub(super) numer: u32,
        pub(super) denom: u32,
    }

    /// Byte offset of `pbi_ppid` inside `struct proc_bsdinfo`.
    pub(super) const PPID_OFFSET: usize = 16;

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

    pub(super) fn read_le_u32(bytes: &[u8], offset: usize) -> Option<u32> {
        let slice = bytes.get(offset..offset + 4)?;
        let raw: [u8; 4] = slice.try_into().ok()?;
        Some(u32::from_le_bytes(raw))
    }

    /// Every live pid `proc_listallpids` reports, or `None` when the kernel call fails.
    pub(super) fn list_all_pids() -> Option<Vec<u32>> {
        const CAPACITY: usize = 8192;
        let mut buffer = vec![0_i32; CAPACITY];
        // SAFETY: `buffer` is a valid mutable array of `CAPACITY` i32 values, and the byte length
        // passed with it is exactly that array's size.
        let written = unsafe {
            libc::proc_listallpids(
                buffer.as_mut_ptr().cast::<libc::c_void>(),
                libc::c_int::try_from(CAPACITY * std::mem::size_of::<i32>()).ok()?,
            )
        };
        if written <= 0 {
            return None;
        }
        let count = usize::try_from(written).ok()?.min(CAPACITY);
        Some(
            buffer[..count]
                .iter()
                .filter_map(|pid| u32::try_from(*pid).ok())
                .filter(|pid| *pid > 0)
                .collect(),
        )
    }
}

#[cfg(target_os = "windows")]
mod win32 {
    use std::collections::BTreeMap;
    use std::ffi::c_void;

    type Handle = *mut c_void;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const TH32CS_SNAPPROCESS: u32 = 0x2;
    const INVALID_HANDLE_VALUE: Handle = -1_isize as Handle;

    /// `PROCESSENTRY32W`'s fixed prefix: `dwSize`, `cntUsage`, `th32ProcessID`,
    /// `th32DefaultHeapID`, `th32ModuleID`, `cntThreads`, `th32ParentProcessID`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct ProcessEntry32W {
        dw_size: u32,
        cnt_usage: u32,
        th32_process_id: u32,
        th32_default_heap_id: usize,
        th32_module_id: u32,
        cnt_threads: u32,
        th32_parent_process_id: u32,
        pc_pri_class_base: i32,
        dw_flags: u32,
        sz_exe_file: [u16; 260],
    }

    impl Default for ProcessEntry32W {
        fn default() -> Self {
            Self {
                dw_size: u32::try_from(std::mem::size_of::<Self>()).unwrap_or(568),
                cnt_usage: 0,
                th32_process_id: 0,
                th32_default_heap_id: 0,
                th32_module_id: 0,
                cnt_threads: 0,
                th32_parent_process_id: 0,
                pc_pri_class_base: 0,
                dw_flags: 0,
                sz_exe_file: [0; 260],
            }
        }
    }

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
        fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> Handle;
        fn Process32FirstW(snapshot: Handle, entry: *mut ProcessEntry32W) -> i32;
        fn Process32NextW(snapshot: Handle, entry: *mut ProcessEntry32W) -> i32;
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

    /// Every live process's parent pid from one toolhelp snapshot, or `None` when it cannot be taken.
    pub(super) fn parent_pids() -> Option<BTreeMap<u32, u32>> {
        // SAFETY: `CreateToolhelp32Snapshot` takes plain integer arguments; an invalid handle is
        // checked before it is used and closed exactly once below.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot.is_null() || snapshot == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut parents = BTreeMap::new();
        let mut entry = ProcessEntry32W::default();
        // SAFETY: `snapshot` is a live snapshot handle and `entry` is a valid mutable
        // `PROCESSENTRY32W` whose `dw_size` names its own size, as the API requires.
        let mut more = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
        while more {
            parents.insert(entry.th32_process_id, entry.th32_parent_process_id);
            // SAFETY: as above; the same `entry` buffer is reused for the next row.
            more = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
        }
        // SAFETY: `snapshot` was opened above and is closed once here.
        unsafe {
            CloseHandle(snapshot);
        }
        Some(parents)
    }
}

/// A start identity that changes when a pid is reused, or `None` when it cannot be read.
pub fn read_process_start_identity(pid: u32) -> Option<String> {
    let start = read_process_start_time(pid)?;
    let prefix = start_identity_prefix()?;
    Some(format!("{prefix}:{start}"))
}

fn start_identity_prefix() -> Option<&'static str> {
    if cfg!(target_os = "linux") {
        Some("linux-proc-start-ticks")
    } else if cfg!(target_os = "macos") {
        Some("proc-start-epoch")
    } else if cfg!(target_os = "windows") {
        Some("win32-creation-filetime")
    } else {
        None
    }
}

/// When the process started, in a platform unit that orders processes by creation (Linux boot
/// clock ticks, darwin epoch seconds, Windows FILETIME), or `None` when it cannot be read.
pub fn read_process_start_time(pid: u32) -> Option<u64> {
    if pid == 0 {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        parse_linux_stat_start_time(&linux_stat(pid)?).filter(|ticks| *ticks > 0)
    }
    #[cfg(target_os = "macos")]
    {
        let info =
            darwin::pid_info::<{ darwin::PROC_BSDINFO_SIZE }>(pid, darwin::PROC_PIDTBSDINFO)?;
        darwin::read_le_u64(&info, darwin::START_TVSEC_OFFSET).filter(|seconds| *seconds > 0)
    }
    #[cfg(target_os = "windows")]
    {
        win32::process_times(pid)
            .map(|times| times.creation)
            .filter(|creation| *creation > 0)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        None
    }
}

/// Every live process's parent pid, or `None` when the process table cannot be read.
pub fn read_parent_pids() -> Option<std::collections::BTreeMap<u32, u32>> {
    #[cfg(target_os = "linux")]
    {
        let entries = std::fs::read_dir("/proc").ok()?;
        let mut parents = std::collections::BTreeMap::new();
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Ok(pid) = name.parse::<u32>() else {
                continue;
            };
            if let Some(parent) =
                linux_stat(pid).and_then(|stat| parse_linux_stat_parent_pid(&stat))
            {
                parents.insert(pid, parent);
            }
        }
        Some(parents)
    }
    #[cfg(target_os = "macos")]
    {
        let pids = darwin::list_all_pids()?;
        let mut parents = std::collections::BTreeMap::new();
        for pid in pids {
            let Some(info) =
                darwin::pid_info::<{ darwin::PROC_BSDINFO_SIZE }>(pid, darwin::PROC_PIDTBSDINFO)
            else {
                continue;
            };
            if let Some(parent) = darwin::read_le_u32(&info, darwin::PPID_OFFSET) {
                parents.insert(pid, parent);
            }
        }
        Some(parents)
    }
    #[cfg(target_os = "windows")]
    {
        let parents = win32::parent_pids()?;
        (!parents.is_empty()).then_some(parents)
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
        parse_linux_stat_cpu_seconds(&linux_stat(pid)?)
    }
    #[cfg(target_os = "macos")]
    {
        let info =
            darwin::pid_info::<{ darwin::PROC_TASKINFO_SIZE }>(pid, darwin::PROC_PIDTASKINFO)?;
        let timebase = darwin::timebase()?;
        let user = darwin::read_le_u64(&info, darwin::TOTAL_USER_OFFSET)?;
        let system = darwin::read_le_u64(&info, darwin::TOTAL_SYSTEM_OFFSET)?;
        scale_mach_ticks(user.saturating_add(system), timebase.numer, timebase.denom)
    }
    #[cfg(target_os = "windows")]
    {
        let times = win32::process_times(pid)?;
        Some((times.kernel.saturating_add(times.user) as f64) / 1e7)
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
        let user = usage.ru_utime.tv_sec as f64 + (usage.ru_utime.tv_usec as f64) / 1e6;
        let system = usage.ru_stime.tv_sec as f64 + (usage.ru_stime.tv_usec as f64) / 1e6;
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
        let stat = "42 (a (b) c) S 1 2 3 4 5 6 7 8 9 10 125 75 13 14 15 16 17 18 777 20";
        assert_eq!(parse_linux_stat_cpu_seconds(stat), Some(2.0));
        assert_eq!(parse_linux_stat_start_time(stat), Some(777));
        assert_eq!(parse_linux_stat_parent_pid(stat), Some(1));
        assert_eq!(parse_linux_stat_cpu_seconds("42 (x) S 1"), None);
    }
}
