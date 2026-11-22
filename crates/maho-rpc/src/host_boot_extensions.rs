use crate::host_warm::{HostWarm, HostWarmProfile};
use std::sync::Arc;

pub const PRIVATE_FDS_ENV: &str = "SENPI_PRIVATE_FDS";

pub fn private_table_present() -> bool {
    std::env::var(PRIVATE_FDS_ENV).is_ok_and(|value| !value.trim().is_empty())
}

pub fn warm_at_boot_for_private_fds(
    warm: Option<&HostWarm>,
    profile: HostWarmProfile,
    log: Arc<dyn Fn(&str) + Send + Sync>,
) -> Option<tokio::task::JoinHandle<()>> {
    warm_at_boot_for_private_fds_with_table(warm, profile, private_table_present(), log)
}

pub fn warm_at_boot_for_private_fds_with_table(
    warm: Option<&HostWarm>,
    profile: HostWarmProfile,
    table_present: bool,
    log: Arc<dyn Fn(&str) + Send + Sync>,
) -> Option<tokio::task::JoinHandle<()>> {
    let warm = warm.filter(|_| table_present)?.clone();
    Some(tokio::spawn(async move {
        if let Err(cause) = warm(profile).await {
            log(&format!("boot extension load failed: {cause}"));
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_warm::{HostWarmState, HostWarmer};
    use futures::FutureExt;
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    #[tokio::test]
    async fn warms_default_profile_once_when_private_table_present_and_coalesces_client_warm() {
        let loads = Arc::new(AtomicUsize::new(0));
        let loads_for_warmer = loads.clone();
        let warmer = HostWarmer::new(move |_profile| {
            let loads = loads_for_warmer.clone();
            async move {
                loads.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
            .boxed()
        });
        let warm: HostWarm = Arc::new(move |profile| {
            let warmer = warmer.clone();
            async move { warmer.run(profile).await }.boxed()
        });
        let logs = Arc::new(Mutex::new(Vec::<String>::new()));
        let logs_sink = logs.clone();
        let log: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |msg| {
            logs_sink
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(msg.to_owned());
        });

        let profile = HostWarmProfile {
            cwd: "/work".into(),
            session_kind: None,
            session_context: None,
        };
        assert!(
            warm_at_boot_for_private_fds_with_table(
                Some(&warm),
                profile.clone(),
                false,
                log.clone()
            )
            .is_none()
        );
        assert_eq!(loads.load(Ordering::SeqCst), 0);

        let task = warm_at_boot_for_private_fds_with_table(
            Some(&warm),
            profile.clone(),
            true,
            log.clone(),
        )
        .expect("spawned boot warm");
        task.await.expect("boot warm settled");
        assert_eq!(loads.load(Ordering::SeqCst), 1);

        let second = warm(profile).await.expect("second warm");
        assert_eq!(second, HostWarmState::AlreadyWarm);
        assert_eq!(loads.load(Ordering::SeqCst), 1);
        assert!(
            logs.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );
    }

    #[tokio::test]
    async fn logs_boot_extension_load_failure_without_panicking() {
        let warm: HostWarm = Arc::new(|_profile| async { Err("boom".to_owned()) }.boxed());
        let logs = Arc::new(Mutex::new(Vec::<String>::new()));
        let logs_sink = logs.clone();
        let log: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |msg| {
            logs_sink
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(msg.to_owned());
        });
        let task = warm_at_boot_for_private_fds_with_table(
            Some(&warm),
            HostWarmProfile::default(),
            true,
            log,
        )
        .expect("spawned boot warm");
        task.await.expect("boot warm settled");
        assert_eq!(
            *logs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec!["boot extension load failed: boom".to_owned()]
        );
    }
}
