//! Port of the argument parsing in `remote-local.ts` and `remote-session-args.ts`: every verb's
//! arguments, parsed before the service is contacted. `--` ends the options, so a text that starts
//! with dashes can still be sent.

use crate::exit::UsageError;

/// `mhc remote send <target> -`: the message is read from stdin (omo#13905).
pub const STDIN_TEXT: &str = "-";
pub const SESSION_VERBS: [&str; 5] = ["open", "send", "model", "interrupt", "close"];
pub const ROW_STATES: [&str; 3] = ["running", "idle", "ended"];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionFilter {
    pub device: Option<String>,
    pub project: Option<String>,
    pub state: Option<String>,
}

/// A session, optionally qualified with the device that holds it (`<device>/<session>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub device: Option<String>,
    pub session: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Auto,
    Steer,
    FollowUp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakeValue {
    On,
    Off,
    /// Removes the stored value: the build default applies.
    Default,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionArgs {
    Open {
        device: String,
        project: Option<String>,
        model: Option<String>,
    },
    Send {
        target: Target,
        text: String,
        delivery: Delivery,
        wait: bool,
    },
    Model {
        target: Target,
        model: String,
    },
    Interrupt {
        target: Target,
    },
    /// `force`: close even while a turn runs.
    Close {
        target: Target,
        force: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parsed {
    pub command: String,
    pub json: bool,
    pub filter: SessionFilter,
    pub control: Option<bool>,
    pub wake: Option<WakeValue>,
    pub session: Option<SessionArgs>,
}

fn usage(message: impl Into<String>) -> UsageError {
    UsageError(message.into())
}

pub fn parse_target(verb: &str, value: Option<&str>) -> Result<Target, UsageError> {
    let invalid = || {
        usage(format!(
            "mhc remote {verb}: name the session as <device>/<session> (the SESSION column of mhc remote sessions) or by its id"
        ))
    };
    let value = value
        .filter(|value| !value.is_empty())
        .ok_or_else(invalid)?;
    match value.split_once('/') {
        None => Ok(Target {
            device: None,
            session: value.to_owned(),
        }),
        Some((device, session)) if !device.is_empty() && !session.is_empty() => Ok(Target {
            device: Some(device.to_owned()),
            session: session.to_owned(),
        }),
        Some(_) => Err(invalid()),
    }
}

fn session_flags(verb: &str) -> (&'static [&'static str], &'static [&'static str]) {
    match verb {
        "open" => (&["--project", "--model"], &[]),
        "send" => (&["--timeout"], &["--steer", "--follow-up", "--no-wait"]),
        "close" => (&[], &["--force"]),
        _ => (&[], &[]),
    }
}

pub fn parse_session(verb: &str, rest: &[String]) -> Result<(bool, SessionArgs), UsageError> {
    let (value_flags, switch_flags) = session_flags(verb);
    let mut json = false;
    let mut values: Vec<(String, String)> = Vec::new();
    let mut switches: Vec<String> = Vec::new();
    let mut positional: Vec<String> = Vec::new();
    let mut index = 0;
    while index < rest.len() {
        let arg = &rest[index];
        if arg == "--" {
            positional.extend(rest[index + 1..].iter().cloned());
            break;
        }
        if arg == "--json" {
            json = true;
        } else if value_flags.contains(&arg.as_str()) {
            let value = rest
                .get(index + 1)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| usage(format!("mhc remote {verb}: {arg} needs a value")))?;
            values.push((arg.clone(), value.clone()));
            index += 1;
        } else if switch_flags.contains(&arg.as_str()) {
            switches.push(arg.clone());
        } else if arg.starts_with("--") {
            return Err(usage(format!("mhc remote {verb}: unknown argument {arg}")));
        } else {
            positional.push(arg.clone());
        }
        index += 1;
    }
    let arity = if verb == "send" || verb == "model" {
        2
    } else {
        1
    };
    if let Some(extra) = positional.get(arity) {
        return Err(usage(format!(
            "mhc remote {verb}: unknown argument {extra}"
        )));
    }
    let value_of = |flag: &str| {
        values
            .iter()
            .find(|(name, _)| name == flag)
            .map(|(_, value)| value.clone())
    };
    let has = |flag: &str| switches.iter().any(|name| name == flag);
    let session = match verb {
        "open" => {
            let device = positional.first().cloned().ok_or_else(|| {
                usage("mhc remote open: the device is required (the DEVICE column of mhc remote devices)")
            })?;
            let model = value_of("--model");
            if let Some(model) = &model
                && !matches!(model.split_once('/'), Some((provider, id)) if !provider.is_empty() && !id.is_empty())
            {
                return Err(usage("mhc remote open: --model is provider/id"));
            }
            SessionArgs::Open {
                device,
                project: value_of("--project"),
                model,
            }
        }
        "send" => {
            let target = parse_target(verb, positional.first().map(String::as_str))?;
            let text = positional
                .get(1)
                .filter(|text| !text.is_empty())
                .cloned()
                .ok_or_else(|| usage("mhc remote send: the message text is required"))?;
            if has("--steer") && has("--follow-up") {
                return Err(usage(
                    "mhc remote send: pass --steer or --follow-up, not both",
                ));
            }
            if let Some(timeout) = value_of("--timeout")
                && !timeout
                    .parse::<u32>()
                    .is_ok_and(|seconds| (1..=600).contains(&seconds))
            {
                return Err(usage("mhc remote send: --timeout is seconds, 1 to 600"));
            }
            let delivery = if has("--steer") {
                Delivery::Steer
            } else if has("--follow-up") {
                Delivery::FollowUp
            } else {
                Delivery::Auto
            };
            SessionArgs::Send {
                target,
                text,
                delivery,
                wait: !has("--no-wait"),
            }
        }
        "model" => {
            let target = parse_target(verb, positional.first().map(String::as_str))?;
            let model = positional
                .get(1)
                .filter(|model| !model.is_empty())
                .cloned()
                .ok_or_else(|| usage("mhc remote model: the model is required (provider/id)"))?;
            if !matches!(model.split_once('/'), Some((provider, id)) if !provider.is_empty() && !id.is_empty())
            {
                return Err(usage("mhc remote model: the model is provider/id"));
            }
            SessionArgs::Model { target, model }
        }
        "interrupt" => SessionArgs::Interrupt {
            target: parse_target(verb, positional.first().map(String::as_str))?,
        },
        _ => SessionArgs::Close {
            target: parse_target(verb, positional.first().map(String::as_str))?,
            force: has("--force"),
        },
    };
    Ok((json, session))
}

pub fn parse(args: &[String]) -> Result<Parsed, UsageError> {
    let command = args.first().cloned().unwrap_or_default();
    let rest = args.get(1..).unwrap_or_default();
    let mut parsed = Parsed {
        command: command.clone(),
        json: false,
        filter: SessionFilter::default(),
        control: None,
        wake: None,
        session: None,
    };
    if SESSION_VERBS.contains(&command.as_str()) {
        let (json, session) = parse_session(&command, rest)?;
        parsed.json = json;
        parsed.session = Some(session);
        return Ok(parsed);
    }
    let mut index = 0;
    while index < rest.len() {
        let flag = rest[index].as_str();
        if flag == "--json" {
            parsed.json = true;
        } else if command == "control" {
            if parsed.control.is_some() || !matches!(flag, "on" | "off") {
                return Err(usage("mhc remote control: the value is on or off"));
            }
            parsed.control = Some(flag == "on");
        } else if command == "wake" {
            let value = match flag {
                "on" => WakeValue::On,
                "off" => WakeValue::Off,
                "default" => WakeValue::Default,
                _ => return Err(usage("mhc remote wake: the value is on, off or default")),
            };
            if parsed.wake.replace(value).is_some() {
                return Err(usage("mhc remote wake: the value is on, off or default"));
            }
        } else if command == "sessions" && matches!(flag, "--device" | "--project" | "--state") {
            let value = rest
                .get(index + 1)
                .filter(|value| !value.is_empty())
                .cloned()
                .ok_or_else(|| usage(format!("mhc remote sessions: {flag} needs a value")))?;
            index += 1;
            match flag {
                "--device" => parsed.filter.device = Some(value),
                "--project" => parsed.filter.project = Some(value),
                _ if ROW_STATES.contains(&value.as_str()) => parsed.filter.state = Some(value),
                _ => {
                    return Err(usage(format!(
                        "mhc remote sessions: --state is one of {}",
                        ROW_STATES.join(", ")
                    )));
                }
            }
        } else {
            return Err(usage(format!(
                "mhc remote {command}: unknown argument {flag}"
            )));
        }
        index += 1;
    }
    Ok(parsed)
}
