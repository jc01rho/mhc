pub const WAKE_MEMORY_FLOOR_BYTES: u64 = 1024 * 1024 * 1024;
pub const WAKE_SESSION_COST_BYTES: u64 = 409 * 1024 * 1024;
const FLOOR_SHARE_OF_LIMIT: u64 = 4;

pub struct MemoryProbeSources<'a> {
    pub platform: &'a str,
    pub read_file: &'a dyn Fn(&str) -> Option<String>,
}

impl Default for MemoryProbeSources<'static> {
    fn default() -> Self {
        Self {
            platform: std::env::consts::OS,
            read_file: &default_read_file,
        }
    }
}

fn default_read_file(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

struct CgroupMemory {
    limit: u64,
    current: u64,
}

fn cgroup_memory(sources: &MemoryProbeSources<'_>) -> Option<CgroupMemory> {
    let cgroup_text = (sources.read_file)("/proc/self/cgroup")?;
    let entry = cgroup_text.lines().find(|line| line.starts_with("0::"))?;
    let rel = entry["0::".len()..].trim().trim_end_matches('/');
    let base = format!("/sys/fs/cgroup{rel}");
    let max_raw = (sources.read_file)(&format!("{base}/memory.max"))?;
    let max = max_raw.trim();
    if max.is_empty() || max == "max" {
        return None;
    }
    let current_raw = (sources.read_file)(&format!("{base}/memory.current"))?;
    let limit: u64 = max.parse().ok()?;
    let current: u64 = current_raw.trim().parse().ok()?;
    Some(CgroupMemory { limit, current })
}

fn cgroup_room(sources: &MemoryProbeSources<'_>) -> Option<u64> {
    let cgroup = cgroup_memory(sources)?;
    Some(cgroup.limit.saturating_sub(cgroup.current))
}

fn meminfo(sources: &MemoryProbeSources<'_>, field: &str) -> Option<u64> {
    let text = (sources.read_file)("/proc/meminfo")?;
    let prefix = format!("{field}:");
    let line = text.lines().find(|row| row.starts_with(&prefix))?;
    let after = line[prefix.len()..].trim();
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    let rest = after[digits.len()..].trim_start();
    if !rest.starts_with("kB") {
        return None;
    }
    let kib: u64 = digits.parse().ok()?;
    kib.checked_mul(1024)
}

pub fn read_host_memory_headroom(sources: &MemoryProbeSources<'_>) -> Option<u64> {
    if sources.platform != "linux" {
        return None;
    }
    let cgroup = cgroup_room(sources);
    let available = meminfo(sources, "MemAvailable");
    match (cgroup, available) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

pub fn read_default_host_memory_headroom() -> Option<u64> {
    read_host_memory_headroom(&MemoryProbeSources::default())
}

pub fn read_wake_memory_floor(sources: &MemoryProbeSources<'_>) -> u64 {
    if sources.platform != "linux" {
        return WAKE_MEMORY_FLOOR_BYTES;
    }
    let limit = cgroup_memory(sources).map(|cgroup| cgroup.limit);
    let total = meminfo(sources, "MemTotal");
    let smallest = match (limit, total) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => return WAKE_MEMORY_FLOOR_BYTES,
    };
    let share = smallest / FLOOR_SHARE_OF_LIMIT;
    WAKE_MEMORY_FLOOR_BYTES.min(WAKE_SESSION_COST_BYTES.max(share))
}

pub fn read_default_wake_memory_floor() -> u64 {
    read_wake_memory_floor(&MemoryProbeSources::default())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;

    type ReadFileFn = Box<dyn Fn(&str) -> Option<String>>;

    fn files(entries: &[(&str, String)]) -> ReadFileFn {
        let map: BTreeMap<String, String> = entries
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect();
        Box::new(move |path: &str| map.get(path).cloned())
    }

    fn meminfo_text(available_kb: u64, total_kb: u64) -> String {
        format!(
            "MemTotal:       {total_kb} kB\nMemFree:         1000000 kB\nMemAvailable:   {available_kb} kB\n"
        )
    }

    #[test]
    fn given_the_floor_then_it_is_1_gib() {
        assert_eq!(WAKE_MEMORY_FLOOR_BYTES, GIB);
    }

    #[test]
    fn given_a_cgroup_v2_limit_and_host_mem_available_when_read_on_linux_then_headroom_is_the_smaller()
     {
        let read = files(&[
            ("/proc/self/cgroup", "0::/user.slice/omo.scope\n".into()),
            (
                "/sys/fs/cgroup/user.slice/omo.scope/memory.max",
                format!("{}\n", 4 * GIB),
            ),
            (
                "/sys/fs/cgroup/user.slice/omo.scope/memory.current",
                format!("{}\n", 3 * GIB + 512 * MIB),
            ),
            ("/proc/meminfo", meminfo_text(8 * 1024 * 1024, 65_536_000)),
        ]);
        let sources = MemoryProbeSources {
            platform: "linux",
            read_file: &read,
        };
        assert_eq!(read_host_memory_headroom(&sources), Some(512 * MIB));
    }

    #[test]
    fn given_a_cgroup_whose_memory_max_is_max_when_read_on_linux_then_mem_available_alone_is_headroom()
     {
        let read = files(&[
            ("/proc/self/cgroup", "0::/\n".into()),
            ("/sys/fs/cgroup/memory.max", "max\n".into()),
            ("/sys/fs/cgroup/memory.current", format!("{GIB}\n")),
            ("/proc/meminfo", meminfo_text(2 * 1024 * 1024, 65_536_000)),
        ]);
        let sources = MemoryProbeSources {
            platform: "linux",
            read_file: &read,
        };
        assert_eq!(read_host_memory_headroom(&sources), Some(2 * GIB));
    }

    #[test]
    fn given_no_cgroup_v2_entry_but_readable_meminfo_when_read_on_linux_then_mem_available_is_headroom()
     {
        let read = files(&[
            ("/proc/self/cgroup", "12:memory:/x\n".into()),
            ("/proc/meminfo", meminfo_text(1024, 65_536_000)),
        ]);
        let sources = MemoryProbeSources {
            platform: "linux",
            read_file: &read,
        };
        assert_eq!(read_host_memory_headroom(&sources), Some(MIB));
    }

    #[test]
    fn given_unreadable_or_non_linux_platform_then_there_is_no_headroom_reading() {
        let empty = files(&[]);
        assert_eq!(
            read_host_memory_headroom(&MemoryProbeSources {
                platform: "linux",
                read_file: &empty,
            }),
            None
        );
        let with_mem = files(&[("/proc/meminfo", meminfo_text(1, 65_536_000))]);
        for platform in ["darwin", "win32"] {
            assert_eq!(
                read_host_memory_headroom(&MemoryProbeSources {
                    platform,
                    read_file: &with_mem,
                }),
                None
            );
        }
    }

    #[test]
    fn given_the_measured_cost_of_one_woken_session_then_it_is_409_mib() {
        assert_eq!(WAKE_SESSION_COST_BYTES, 409 * MIB);
    }

    #[test]
    fn given_a_cgroup_v2_limit_of_768_mib_whose_quarter_is_below_one_wake_cost_when_read_on_linux_then_floor_is_409_mib()
     {
        let read = files(&[
            ("/proc/self/cgroup", "0::/omo.scope\n".into()),
            (
                "/sys/fs/cgroup/omo.scope/memory.max",
                format!("{}\n", 768 * MIB),
            ),
            (
                "/sys/fs/cgroup/omo.scope/memory.current",
                format!("{}\n", 500 * MIB),
            ),
            ("/proc/meminfo", meminfo_text(8 * 1024 * 1024, 65_536_000)),
        ]);
        let sources = MemoryProbeSources {
            platform: "linux",
            read_file: &read,
        };
        assert_eq!(read_wake_memory_floor(&sources), 409 * MIB);
    }

    #[test]
    fn given_a_cgroup_v2_limit_of_256_mib_when_read_on_linux_then_floor_is_still_one_wake_cost() {
        let read = files(&[
            ("/proc/self/cgroup", "0::/omo.scope\n".into()),
            (
                "/sys/fs/cgroup/omo.scope/memory.max",
                format!("{}\n", 256 * MIB),
            ),
            (
                "/sys/fs/cgroup/omo.scope/memory.current",
                format!("{}\n", 16 * MIB),
            ),
            ("/proc/meminfo", meminfo_text(8 * 1024 * 1024, 65_536_000)),
        ]);
        let sources = MemoryProbeSources {
            platform: "linux",
            read_file: &read,
        };
        let floor = read_wake_memory_floor(&sources);
        assert_eq!(floor, 409 * MIB);
        let headroom = read_host_memory_headroom(&sources).expect("headroom");
        assert!(headroom < floor);
    }

    #[test]
    fn given_no_cgroup_limit_on_a_2_gib_host_when_read_on_linux_then_floor_is_512_mib() {
        let read = files(&[
            ("/proc/self/cgroup", "0::/\n".into()),
            ("/sys/fs/cgroup/memory.max", "max\n".into()),
            ("/sys/fs/cgroup/memory.current", "1\n".into()),
            ("/proc/meminfo", meminfo_text(1024, 2 * 1024 * 1024)),
        ]);
        let sources = MemoryProbeSources {
            platform: "linux",
            read_file: &read,
        };
        assert_eq!(read_wake_memory_floor(&sources), 512 * MIB);
    }

    #[test]
    fn given_a_large_host_with_a_large_cgroup_limit_when_read_on_linux_then_floor_stays_1_gib() {
        let read = files(&[
            ("/proc/self/cgroup", "0::/omo.scope\n".into()),
            (
                "/sys/fs/cgroup/omo.scope/memory.max",
                format!("{}\n", 16 * GIB),
            ),
            (
                "/sys/fs/cgroup/omo.scope/memory.current",
                format!("{GIB}\n"),
            ),
            ("/proc/meminfo", meminfo_text(8 * 1024 * 1024, 65_536_000)),
        ]);
        let sources = MemoryProbeSources {
            platform: "linux",
            read_file: &read,
        };
        assert_eq!(read_wake_memory_floor(&sources), GIB);
    }

    #[test]
    fn given_nothing_readable_then_floor_is_1_gib() {
        let empty = files(&[]);
        assert_eq!(
            read_wake_memory_floor(&MemoryProbeSources {
                platform: "linux",
                read_file: &empty,
            }),
            GIB
        );
    }
}
