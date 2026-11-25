//! The wake's entry: what a thread component calls at a session start, local send, and drain admission check.
//!
//! Upstream counterpart: `thread/wake/wake-runtime.ts`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use super::carrier::AdmitIntake;
use super::consumer_start::Log;
use super::runtime::{
    ProcessSameDeviceWakeInput, RunningThreadWake, ThreadWakeLatch, ThreadWakeOptions,
    ThreadWakeStart, process_intake_admission, process_same_device_wake,
    start_thread_wake_at_session, wake_settings_reader,
};
use crate::thread::same_device_wake::SameDeviceWake;
use crate::thread::store::SqliteGatewayStore;

pub struct WakeAtSessionInput {
    pub options: ThreadWakeOptions,
    pub agent_dir: PathBuf,
    pub sessions_directory: PathBuf,
    /// The caller's workspace root: the fallback cwd every non-target read (such as `wake_limit`) uses.
    pub cwd: PathBuf,
    pub engine_version: Option<String>,
    pub warn: Log,
}

pub struct CreateSameDeviceWakeInput {
    pub latch: Option<Arc<ThreadWakeLatch>>,
    pub env: BTreeMap<String, String>,
    pub store: Arc<SqliteGatewayStore>,
    pub agent_dir: PathBuf,
    pub sessions_directory: PathBuf,
    pub cwd: PathBuf,
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

pub fn create_same_device_wake(input: CreateSameDeviceWakeInput) -> SameDeviceWake {
    let settings = wake_settings_reader(input.env, input.cwd);
    process_same_device_wake(ProcessSameDeviceWakeInput {
        latch: input.latch,
        store: input.store,
        agent_dir: input.agent_dir,
        sessions_directory: input.sessions_directory,
        settings,
    })
}

pub fn intake_admission(latch: Option<Arc<ThreadWakeLatch>>, agent_dir: PathBuf) -> AdmitIntake {
    Arc::new(move |device: &str| process_intake_admission(latch.as_deref(), &agent_dir, device))
}
