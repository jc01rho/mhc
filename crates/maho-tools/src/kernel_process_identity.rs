//! Port of senpi `core/kernel-process-identity.ts`: who a pid is, as the kernel reports it, its parent
//! and when it started. An unreadable pid is an unidentified pid.
//!
//! - Linux: `/proc/<pid>/stat`, fields 4 (ppid), 3 (state) and 22 (start in clock ticks since boot),
//!   kept boot-relative so a stepped wall clock (an NTP step, a VM resume) never changes it, plus
//!   `/proc/sys/kernel/random/boot_id`;
//! - macOS: `ps -o ppid=,stat=,lstart=` in UTC, whole seconds (UTC has no repeated hour, so this agrees
//!   with the kernel's stamp all year). senpi reads `sysctl(KERN_PROC)` through `bun:ffi` where it can;
//!   this crate forbids `unsafe`, so the `ps` reader is the only one;
//! - anything else: `None`.
//!
//! `started_at_ms` is derived the same way on every read on one boot, so two reads of one process
//! compare equal; a different process wearing the same pid compares unequal.

use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KernelProcessIdentity {
    pub ppid: u32,
    /// Milliseconds since boot on Linux, scoped by `boot_id`; the kernel's wall-clock start stamp in
    /// whole seconds on macOS.
    pub started_at_ms: u64,
    /// The boot `started_at_ms` is relative to (Linux); `None` where it is a wall-clock stamp.
    pub boot_id: Option<String>,
    /// Exited but not yet reaped: the pid is still taken, but nothing runs under it.
    pub zombie: bool,
}

/// The Linux USER_HZ every supported kernel exports to userspace.
const LINUX_CLOCK_TICKS_PER_SECOND: u64 = 100;

#[cfg(target_os = "linux")]
pub fn read_kernel_process_identity(pid: u32) -> Option<KernelProcessIdentity> {
    read_linux_identity(Path::new("/proc"), pid)
}

#[cfg(target_os = "macos")]
pub fn read_kernel_process_identity(pid: u32) -> Option<KernelProcessIdentity> {
    if pid == 0 {
        return None;
    }
    let output = std::process::Command::new("ps")
        .args(["-o", "ppid=,stat=,lstart=", "-p", &pid.to_string()])
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .env("TZ", "UTC")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_ps_identity(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn read_kernel_process_identity(_pid: u32) -> Option<KernelProcessIdentity> {
    None
}

/// The Linux reader over a `/proc` tree rooted at `proc_root`.
pub fn read_linux_identity(proc_root: &Path, pid: u32) -> Option<KernelProcessIdentity> {
    if pid == 0 {
        return None;
    }
    let stat = std::fs::read_to_string(proc_root.join(pid.to_string()).join("stat")).ok()?;
    let boot_id = std::fs::read_to_string(proc_root.join("sys/kernel/random/boot_id")).ok()?;
    let boot_id = boot_id.trim();
    if boot_id.is_empty() {
        return None;
    }
    parse_linux_stat(&stat, boot_id)
}

/// One `/proc/<pid>/stat` line; `comm` (field 2) is parenthesised and may hold spaces, so the fields are
/// counted from its last `)`.
pub fn parse_linux_stat(stat: &str, boot_id: &str) -> Option<KernelProcessIdentity> {
    let rest = stat.get(stat.rfind(')')? + 2..)?;
    let fields: Vec<&str> = rest.split(' ').collect();
    let ppid = fields.get(1)?.parse::<u32>().ok()?;
    let start_ticks = fields.get(19)?.trim().parse::<u64>().ok()?;
    Some(KernelProcessIdentity {
        ppid,
        started_at_ms: start_ticks * (1000 / LINUX_CLOCK_TICKS_PER_SECOND),
        boot_id: Some(boot_id.to_owned()),
        zombie: fields.first() == Some(&"Z"),
    })
}

/// One `ps -o ppid=,stat=,lstart=` line printed under `TZ=UTC`; the start time is read as UTC whatever
/// the reader's zone.
pub fn parse_ps_identity(stdout: &str) -> Option<KernelProcessIdentity> {
    let mut parts = stdout.split_whitespace();
    let ppid = parts.next()?.parse::<u32>().ok()?;
    let stat = parts.next()?;
    let (_weekday, month, day, time, year) = (
        parts.next()?,
        parts.next()?,
        parts.next()?,
        parts.next()?,
        parts.next()?,
    );
    if parts.next().is_some() {
        return None;
    }
    Some(KernelProcessIdentity {
        ppid,
        started_at_ms: utc_epoch_ms(
            year.parse().ok()?,
            month_number(month)?,
            day.parse().ok()?,
            time,
        )?,
        boot_id: None,
        zombie: stat.starts_with('Z'),
    })
}

fn month_number(name: &str) -> Option<u32> {
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let index = months.iter().position(|month| *month == name)?;
    u32::try_from(index + 1).ok()
}

fn utc_epoch_ms(year: i64, month: u32, day: u32, time: &str) -> Option<u64> {
    let mut clock = time.split(':');
    let hours = clock.next()?.parse::<u64>().ok()?;
    let minutes = clock.next()?.parse::<u64>().ok()?;
    let seconds = clock.next()?.parse::<u64>().ok()?;
    if clock.next().is_some()
        || hours > 23
        || minutes > 59
        || seconds > 60
        || !(1..=31).contains(&day)
    {
        return None;
    }
    let days = u64::try_from(days_from_civil(year, month, day)).ok()?;
    Some((days * 86_400 + hours * 3_600 + minutes * 60 + seconds) * 1000)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let month_from_march = i64::from((month + 9) % 12);
    let day_of_year = (153 * month_from_march + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT_TAIL: &str = "S 1 1234 1234 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 987654 12345678 1500 18446744073709551615";

    #[test]
    fn a_stat_line_yields_the_parent_and_the_boot_relative_start() {
        let identity =
            parse_linux_stat(&format!("1234 (node) {STAT_TAIL}"), "boot-1").expect("parsed");
        assert_eq!(
            identity,
            KernelProcessIdentity {
                ppid: 1,
                started_at_ms: 9_876_540,
                boot_id: Some("boot-1".to_owned()),
                zombie: false,
            }
        );
    }

    #[test]
    fn a_comm_with_spaces_and_parentheses_does_not_shift_the_fields() {
        let identity =
            parse_linux_stat(&format!("77 (my (odd) comm) {STAT_TAIL}"), "boot-1").expect("parsed");
        assert_eq!((identity.ppid, identity.started_at_ms), (1, 9_876_540));
    }

    #[test]
    fn a_zombie_state_is_reported() {
        let line = format!("1234 (defunct) Z{}", &STAT_TAIL[1..]);
        assert!(parse_linux_stat(&line, "boot-1").expect("parsed").zombie);
    }

    #[test]
    fn a_truncated_or_non_numeric_stat_line_is_unidentified() {
        assert_eq!(parse_linux_stat("1234 (node) S 1 2", "boot-1"), None);
        assert_eq!(parse_linux_stat("no parenthesis", "boot-1"), None);
        let line = "1234 (node) S x 1234 1234 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 987654 1";
        assert_eq!(parse_linux_stat(line, "boot-1"), None);
    }

    #[test]
    fn a_proc_tree_is_read_with_its_boot_id_and_a_missing_pid_or_boot_is_unidentified() {
        let root = tempfile::tempdir().expect("proc root");
        std::fs::create_dir_all(root.path().join("42")).expect("pid dir");
        std::fs::write(
            root.path().join("42/stat"),
            format!("42 (sleep) {STAT_TAIL}\n"),
        )
        .expect("stat");
        assert_eq!(read_linux_identity(root.path(), 42), None, "no boot id yet");
        std::fs::create_dir_all(root.path().join("sys/kernel/random")).expect("boot dir");
        std::fs::write(root.path().join("sys/kernel/random/boot_id"), "0a1b2c3d\n")
            .expect("boot id");

        let identity = read_linux_identity(root.path(), 42).expect("identified");
        assert_eq!(identity.boot_id.as_deref(), Some("0a1b2c3d"));
        assert_eq!(read_linux_identity(root.path(), 43), None);
        assert_eq!(read_linux_identity(root.path(), 0), None);
    }

    #[test]
    fn a_ps_line_is_read_as_utc_whole_seconds() {
        for (line, started_at_ms) in [
            ("    1 Ss   Thu Jan  1 00:00:01 1970\n", 1_000),
            ("  842 S+   Sat Nov 21 12:00:00 2026\n", 1_795_262_400_000),
            ("  842 S    Tue Feb 29 23:59:59 2028\n", 1_835_481_599_000),
            ("  842 S    Mon Mar  1 00:00:00 2100\n", 4_107_542_400_000),
        ] {
            let identity = parse_ps_identity(line).expect(line);
            assert_eq!(identity.started_at_ms, started_at_ms, "{line}");
            assert_eq!(identity.boot_id, None);
            assert!(!identity.zombie);
        }
        assert_eq!(
            parse_ps_identity("  842 Z  Sat Nov 21 12:00:00 2026").map(|identity| identity.zombie),
            Some(true)
        );
        assert_eq!(
            parse_ps_identity("  842 Z  Sat Nov 21 12:00:00 2026").map(|identity| identity.ppid),
            Some(842)
        );
    }

    #[test]
    fn a_malformed_ps_line_is_unidentified() {
        for line in [
            "",
            "842",
            "842 S Sat Nov 21 12:00:00",
            "842 S Sat Nov 21 12:00:00 2026 extra",
            "842 S Sat Nul 21 12:00:00 2026",
            "842 S Sat Nov 99 12:00:00 2026",
            "842 S Sat Nov 21 25:00:00 2026",
            "x S Sat Nov 21 12:00:00 2026",
        ] {
            assert_eq!(parse_ps_identity(line), None, "{line:?}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_running_process_and_its_child_are_identified_by_the_real_proc() {
        let me = read_kernel_process_identity(std::process::id()).expect("own identity");
        assert_eq!(me.ppid, std::os::unix::process::parent_id());
        assert!(!me.zombie);
        assert!(me.boot_id.is_some());

        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("sleep starts");
        let identity = read_kernel_process_identity(child.id()).expect("child identity");
        let _killed = child.kill();
        let _waited = child.wait();
        assert_eq!(identity.ppid, std::process::id());
        assert!(identity.started_at_ms >= me.started_at_ms);
        assert_eq!(identity.boot_id, me.boot_id);
    }
}
