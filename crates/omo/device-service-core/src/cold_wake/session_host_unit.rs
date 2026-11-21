//! Port of `cold-wake/session-host-unit.ts` (omo#13594, #13669): the checked start of the session host
//! and the systemd unit / launchd plist both platforms write for it, plus mhc's device service unit.
//!
//! The session host unit runs one ensure per start (systemd `Type=oneshot`, launchd `RunAtLoad`): at
//! login and on every start the device service asks for. Nothing re-runs it on a timer (a rerun of an
//! ensure that exits 0 was a restart loop, #13668), and a hung ensure ends after
//! [`ENSURE_TIMEOUT_SECONDS`]. `KillMode=process` / `AbandonProcessGroup` keep the host running when
//! the ensure ends, so a stop never ends a session.

use std::fmt;
use std::path::Path;

pub const ENSURE_TIMEOUT_SECONDS: u32 = 300;
pub const SESSION_HOST_SYSTEMD_UNIT: &str = "mhc-session-host.service";
pub const SESSION_HOST_LAUNCHD_LABEL: &str = "ai.mhc.session-host";
pub const DEVICE_SERVICE_SYSTEMD_UNIT: &str = "mhc-relayd.service";
pub const DEVICE_SERVICE_LAUNCHD_LABEL: &str = "ai.mhc.relayd";
/// The agent dir the host serves, named for the launched process.
pub const AGENT_DIR_ENV: &str = "MAHO_CODING_AGENT_DIR";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColdWakeUnitError(pub String);

impl fmt::Display for ColdWakeUnitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ColdWakeUnitError {}

/// What a unit launches: the mhc binary, the agent dir and the PATH it runs with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UnitLaunchOptions {
    pub mhc_bin: String,
    pub agent_dir: String,
    pub path: Option<String>,
}

fn has_control_character(value: &str) -> bool {
    value
        .chars()
        .any(|char| (char as u32) < 0x20 || char as u32 == 0x7f)
}

// A control character (a newline above all) would end the quoted value and start a unit line of its
// own.
fn checked(name: &str, value: &str, absolute: bool) -> Result<String, ColdWakeUnitError> {
    if value.is_empty() || has_control_character(value) {
        return Err(ColdWakeUnitError(format!(
            "the {name} has a control character"
        )));
    }
    if absolute && !Path::new(value).is_absolute() {
        return Err(ColdWakeUnitError(format!(
            "the {name} is not an absolute path"
        )));
    }
    Ok(value.to_owned())
}

/// A checked launch: the argv and environment both unit renderers write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionHostLaunch {
    program: String,
    argv: Vec<String>,
    env: Vec<(String, String)>,
}

impl SessionHostLaunch {
    fn build(options: &UnitLaunchOptions, tail: &[&str]) -> Result<Self, ColdWakeUnitError> {
        let program = checked("mhc binary", &options.mhc_bin, true)?;
        let mut env = vec![(
            AGENT_DIR_ENV.to_owned(),
            checked("agent dir", &options.agent_dir, true)?,
        )];
        if let Some(path) = options.path.as_deref().filter(|path| !path.is_empty()) {
            env.push(("PATH".to_owned(), checked("PATH", path, false)?));
        }
        let mut argv = vec![program.clone()];
        argv.extend(tail.iter().map(|arg| (*arg).to_owned()));
        Ok(Self { program, argv, env })
    }

    /// `<mhc> host ensure`: ensures this agent dir's host and exits; the host stays up.
    pub fn session_host(options: &UnitLaunchOptions) -> Result<Self, ColdWakeUnitError> {
        Self::build(options, &["host", "ensure"])
    }

    /// `<mhc> remote daemon`: the open device service, kept running.
    pub fn device_service(options: &UnitLaunchOptions) -> Result<Self, ColdWakeUnitError> {
        Self::build(options, &["remote", "daemon"])
    }

    pub fn program(&self) -> &str {
        &self.program
    }

    pub fn argv(&self) -> &[String] {
        &self.argv
    }

    pub fn env(&self) -> &[(String, String)] {
        &self.env
    }

    /// Another mhc command for the same agent dir, run the way the unit runs it (through env, no
    /// shell).
    pub fn mhc_command(&self, args: &[&str]) -> Vec<String> {
        let mut argv = vec!["/usr/bin/env".to_owned()];
        argv.extend(self.env.iter().map(|(key, value)| format!("{key}={value}")));
        argv.push(self.program.clone());
        argv.extend(args.iter().map(|arg| (*arg).to_owned()));
        argv
    }
}

/// systemd expands `%`-specifiers in `ExecStart=` and `Environment=`, and `$VAR` in `ExecStart=`,
/// even inside quotes: both are doubled.
pub fn systemd_quote(value: &str, dollars: bool) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    let escaped = if dollars {
        escaped.replace('$', "$$")
    } else {
        escaped
    };
    format!("\"{escaped}\"")
}

fn exec_start(launch: &SessionHostLaunch) -> String {
    launch
        .argv
        .iter()
        .map(|arg| systemd_quote(arg, true))
        .collect::<Vec<_>>()
        .join(" ")
}

fn environment_lines(launch: &SessionHostLaunch) -> Vec<String> {
    launch
        .env
        .iter()
        .map(|(key, value)| {
            format!(
                "Environment={}",
                systemd_quote(&format!("{key}={value}"), false)
            )
        })
        .collect()
}

pub fn systemd_session_host_unit(launch: &SessionHostLaunch) -> String {
    let mut lines = vec![
        "[Unit]".to_owned(),
        "Description=mhc session host".to_owned(),
        "StartLimitIntervalSec=0".to_owned(),
        String::new(),
        "[Service]".to_owned(),
        "Type=oneshot".to_owned(),
        format!("TimeoutStartSec={ENSURE_TIMEOUT_SECONDS}"),
        format!("ExecStart={}", exec_start(launch)),
    ];
    lines.extend(environment_lines(launch));
    lines.extend([
        "KillMode=process".to_owned(),
        String::new(),
        "[Install]".to_owned(),
        "WantedBy=default.target".to_owned(),
        String::new(),
    ]);
    lines.join("\n")
}

/// The device service runs for as long as the user's manager does, restarted after a failure.
pub fn systemd_device_service_unit(launch: &SessionHostLaunch) -> String {
    let mut lines = vec![
        "[Unit]".to_owned(),
        "Description=mhc device service (mhc-relayd)".to_owned(),
        String::new(),
        "[Service]".to_owned(),
        "Type=simple".to_owned(),
        format!("ExecStart={}", exec_start(launch)),
    ];
    lines.extend(environment_lines(launch));
    lines.extend([
        "Restart=on-failure".to_owned(),
        "RestartSec=5".to_owned(),
        String::new(),
        "[Install]".to_owned(),
        "WantedBy=default.target".to_owned(),
        String::new(),
    ]);
    lines.join("\n")
}

pub fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn launchd_plist(label: &str, launch: &SessionHostLaunch, keep_alive: bool) -> String {
    let argv = launch
        .argv
        .iter()
        .map(|arg| format!("    <string>{}</string>", xml_escape(arg)))
        .collect::<Vec<_>>()
        .join("\n");
    let env = launch
        .env
        .iter()
        .map(|(key, value)| {
            format!(
                "    <key>{}</key>\n    <string>{}</string>",
                xml_escape(key),
                xml_escape(value)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let lifetime = if keep_alive {
        "  <key>KeepAlive</key>\n  <true/>"
    } else {
        "  <key>AbandonProcessGroup</key>\n  <true/>"
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<plist version=\"1.0\">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
{argv}
  </array>
  <key>EnvironmentVariables</key>
  <dict>
{env}
  </dict>
  <key>RunAtLoad</key>
  <true/>
{lifetime}
  <key>ProcessType</key>
  <string>Interactive</string>
</dict>
</plist>
"
    )
}

/// Runs its ensure once at load and again on every `launchctl kickstart`; never kept alive.
pub fn launchd_session_host_plist(launch: &SessionHostLaunch) -> String {
    launchd_plist(SESSION_HOST_LAUNCHD_LABEL, launch, false)
}

/// The device service, kept alive by launchd.
pub fn launchd_device_service_plist(launch: &SessionHostLaunch) -> String {
    launchd_plist(DEVICE_SERVICE_LAUNCHD_LABEL, launch, true)
}
