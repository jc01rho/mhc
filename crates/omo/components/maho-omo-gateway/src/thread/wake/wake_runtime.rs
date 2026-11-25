//! The wake's entry: what a thread component calls at a session start and at a drain's admission check.
//!
//! Upstream counterpart: `thread/wake/wake-runtime.ts` (the `extensions/omo-wake.js` sidecar). Its `createSameDeviceWake`
//! wraps `gateway/same-device-wake.ts`, which is not ported; the start and the admission check are.

use std::path::PathBuf;
use std::sync::Arc;

use super::carrier::AdmitIntake;
use super::consumer_start::Log;
use super::runtime::{
    RunningThreadWake, ThreadWakeLatch, ThreadWakeOptions, ThreadWakeStart,
    process_intake_admission, start_thread_wake_at_session, wake_settings_reader,
};

pub struct WakeAtSessionInput {
    pub options: ThreadWakeOptions,
    pub agent_dir: PathBuf,
    pub sessions_directory: PathBuf,
    /// The caller's workspace root: the fallback cwd every non-target read (such as `wake_limit`) uses.
    pub cwd: PathBuf,
    pub engine_version: Option<String>,
    pub warn: Log,
}

pub fn start_wake_at_session(
    mode: &str,
    input: WakeAtSessionInput,
) -> Option<Arc<RunningThreadWake>> {
    let settings = wake_settings_reader(input.options.env.clone(), input.cwd);
    start_thread_wake_at_session(
        mode,
        ThreadWakeStart {
            options: input.options,
            agent_dir: input.agent_dir,
            sessions_directory: input.sessions_directory,
            settings,
            engine_version: input.engine_version,
            warn: input.warn,
        },
    )
}

pub fn intake_admission(latch: Option<Arc<ThreadWakeLatch>>, agent_dir: PathBuf) -> AdmitIntake {
    Arc::new(move |device: &str| process_intake_admission(latch.as_deref(), &agent_dir, device))
}
