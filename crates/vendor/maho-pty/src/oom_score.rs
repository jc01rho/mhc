pub const TOOL_CHILD_OOM_SCORE_ADJ: i32 = 1000;

pub fn prefer_as_oom_victim(pid: Option<u32>) {
    #[cfg(target_os = "linux")]
    if let Some(pid) = pid {
        let _ = std::fs::write(
            format!("/proc/{pid}/oom_score_adj"),
            TOOL_CHILD_OOM_SCORE_ADJ.to_string(),
        );
    }
    #[cfg(not(target_os = "linux"))]
    let _ = pid;
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn a_spawned_child_starts_as_the_preferred_oom_victim() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn");
        let pid = child.id();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            prefer_as_oom_victim(Some(pid));
            std::fs::read_to_string(format!("/proc/{pid}/oom_score_adj"))
                .expect("oom_score_adj")
                .trim()
                .to_owned()
        }));
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(result.expect("read score"), TOOL_CHILD_OOM_SCORE_ADJ.to_string());
    }

    #[test]
    fn an_absent_or_dead_pid_is_a_noop() {
        prefer_as_oom_victim(None);
        prefer_as_oom_victim(Some(u32::MAX - 1));
    }
}
