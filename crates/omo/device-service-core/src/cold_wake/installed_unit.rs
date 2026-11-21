//! Port of `cold-wake/installed-unit.ts` (omo#14191): what an installed unit runs and for whom, read
//! back from the text the renderers write. A refresh carries the agent dir and PATH over from here,
//! never from the process that happens to run it.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::{Captures, Regex};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledSessionHostUnit {
    pub program: String,
    pub env: BTreeMap<String, String>,
}

fn compiled(pattern: &str) -> Option<Regex> {
    Regex::new(pattern).ok()
}

static EXEC_START: LazyLock<Option<Regex>> = LazyLock::new(|| compiled(r"(?m)^ExecStart=(.*)$"));
static SYSTEMD_QUOTED: LazyLock<Option<Regex>> =
    LazyLock::new(|| compiled(r#""((?:[^"\\]|\\.)*)""#));
static ENVIRONMENT: LazyLock<Option<Regex>> =
    LazyLock::new(|| compiled(r#"(?m)^Environment="((?:[^"\\]|\\.)*)"$"#));
static UNQUOTE: LazyLock<Option<Regex>> = LazyLock::new(|| compiled(r"\\(.)|%%|\$\$"));
static PROGRAM_ARGUMENTS: LazyLock<Option<Regex>> =
    LazyLock::new(|| compiled(r"<key>ProgramArguments</key>\s*<array>\s*<string>([^<]*)</string>"));
static ENVIRONMENT_DICT: LazyLock<Option<Regex>> =
    LazyLock::new(|| compiled(r"<key>EnvironmentVariables</key>\s*<dict>([\s\S]*?)</dict>"));
static DICT_PAIR: LazyLock<Option<Regex>> =
    LazyLock::new(|| compiled(r"<key>([^<]*)</key>\s*<string>([^<]*)</string>"));

/// The inverse of `systemd_quote`: `\\` and `\"` are C escapes, `%%` and `$$` undo the doubling.
fn systemd_unquote(quoted: &str) -> String {
    let Some(unquote) = UNQUOTE.as_ref() else {
        return quoted.to_owned();
    };
    unquote
        .replace_all(quoted, |captures: &Captures<'_>| match captures.get(1) {
            Some(escaped) => escaped.as_str().to_owned(),
            None => captures[0][1..].to_owned(),
        })
        .into_owned()
}

fn read_systemd(text: &str) -> Option<InstalledSessionHostUnit> {
    let exec_start = EXEC_START.as_ref()?.captures(text)?.get(1)?.as_str();
    let program = SYSTEMD_QUOTED
        .as_ref()?
        .captures(exec_start)?
        .get(1)?
        .as_str();
    let mut env = BTreeMap::new();
    for captures in ENVIRONMENT.as_ref()?.captures_iter(text) {
        let assignment = systemd_unquote(captures.get(1).map_or("", |quoted| quoted.as_str()));
        if let Some((key, value)) = assignment.split_once('=')
            && !key.is_empty()
        {
            env.insert(key.to_owned(), value.to_owned());
        }
    }
    Some(InstalledSessionHostUnit {
        program: systemd_unquote(program),
        env,
    })
}

fn xml_unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

fn read_launchd(text: &str) -> Option<InstalledSessionHostUnit> {
    let program = PROGRAM_ARGUMENTS.as_ref()?.captures(text)?.get(1)?.as_str();
    let mut env = BTreeMap::new();
    let dict = ENVIRONMENT_DICT
        .as_ref()?
        .captures(text)
        .and_then(|captures| captures.get(1))
        .map_or("", |dict| dict.as_str());
    for captures in DICT_PAIR.as_ref()?.captures_iter(dict) {
        let key = captures.get(1).map_or("", |key| key.as_str());
        let value = captures.get(2).map_or("", |value| value.as_str());
        env.insert(xml_unescape(key), xml_unescape(value));
    }
    Some(InstalledSessionHostUnit {
        program: xml_unescape(program),
        env,
    })
}

pub fn read_installed_session_host_unit(
    text: &str,
    darwin: bool,
) -> Option<InstalledSessionHostUnit> {
    if darwin {
        read_launchd(text)
    } else {
        read_systemd(text)
    }
}
