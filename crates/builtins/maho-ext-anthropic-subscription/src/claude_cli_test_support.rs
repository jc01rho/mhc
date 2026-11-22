//! A stand-in for the bundled Claude Code CLI `auth` commands (the fake-claude fixture of senpi#4437): it replays the
//! stdout lines, exit codes and `$BROWSER` call that `claude auth login|status|logout` make, and records what it was
//! given to `FAKE_CLAUDE_RECORD`. `FAKE_CLAUDE_MODE` picks the scenario. No network, no credential store.
use crate::claude_cli_process::ClaudeCliRun;
use std::{collections::BTreeMap, os::unix::fs::PermissionsExt, path::PathBuf};

const FAKE_CLAUDE: &str = r##"#!/bin/sh
mode="${FAKE_CLAUDE_MODE:-login-paste}"
if [ -n "$FAKE_CLAUDE_RECORD" ]; then
  printf 'pid=%s\nargv=%s\nconfig_dir=%s\nno_color=%s\n' "$$" "$*" "$CLAUDE_CONFIG_DIR" "$NO_COLOR" >> "$FAKE_CLAUDE_RECORD"
fi
MANUAL_URL="${FAKE_CLAUDE_MANUAL_URL:-https://claude.ai/oauth/authorize?code=true&client_id=fixture&response_type=code&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&scope=user%3Aprofile+user%3Ainference&state=fixture-state}"
AUTOMATIC_URL='https://claude.ai/oauth/authorize?code=true&client_id=fixture&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A54545%2Fcallback&state=fixture-state'

if [ "$1" = auth ] && [ "$2" = status ]; then
  case "${FAKE_CLAUDE_STATUS_MODE:-$mode}" in
    status-claudeai) printf '{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","email":"user@example.com","orgName":"Fixture Org","subscriptionType":"max"}\n'; exit 0 ;;
    status-apikey) printf '{"loggedIn":true,"authMethod":"api_key","apiProvider":"firstParty"}\n'; exit 0 ;;
    status-bedrock) printf '{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"bedrock"}\n'; exit 0 ;;
    status-garbage) printf 'not json\n'; exit 0 ;;
    *) printf '{"loggedIn":false,"authMethod":"none","apiProvider":"firstParty"}\n'; exit 1 ;;
  esac
fi

if [ "$1" = auth ] && [ "$2" = logout ]; then
  case "$mode" in
    logout-hang) exec sleep 30 ;;
    logout-fail) exit 1 ;;
    *) printf 'Successfully logged out from your Anthropic account.\n'; exit 0 ;;
  esac
fi

[ "$1" = auth ] && [ "$2" = login ] || exit 2

case "$mode" in
  *-terminal) manual=$(printf '\033]8;;%s\033\\%s\033]8;;\033\\' "$MANUAL_URL" "$MANUAL_URL") ;;
  *-bel) manual=$(printf '\033]8;;%s\007%s\033]8;;\007' "$MANUAL_URL" "$MANUAL_URL") ;;
  *) manual="$MANUAL_URL" ;;
esac

print_success() {
  case "$mode" in
    *-terminal) printf '\033[32mLogin successful.\033[0m\r\n' ;;
    *-wrong-prefix) printf '\nError > Login successful.\n' ;;
    *-inline|*-bel) printf 'Login successful.\n' ;;
    *) printf '\nLogin successful.\n' ;;
  esac
}

printf 'Opening browser to sign in…\n'
printf "If the browser didn't open, visit: %s\n" "$manual"
if [ -n "$BROWSER" ]; then "$BROWSER" "$AUTOMATIC_URL" >/dev/null 2>&1; fi
printf 'Paste code here if prompted > '

case "$mode" in
  login-hang) exec sleep 30 ;;
  login-auto*|login-exit1-success)
    print_success
    if [ "$mode" = login-exit1-success ]; then exit 1; fi
    exit 0 ;;
  login-org-denied)
    printf 'OAuth login failed: cli_auth_login_org_not_allowed\n' >&2
    exit 1 ;;
  *)
    IFS= read -r line
    printf 'stdin=%s\n' "$line" >> "$FAKE_CLAUDE_RECORD"
    if [ "$mode" = login-exit0-silent ]; then exit 0; fi
    print_success
    exit 0 ;;
esac
"##;

pub(crate) struct Fixture {
    _root: tempfile::TempDir,
    pub(crate) run: ClaudeCliRun,
    record: PathBuf,
}

impl Fixture {
    pub(crate) fn new(mode: &str, extra_env: &[(&str, &str)]) -> Self {
        let root = tempfile::tempdir().expect("fixture root");
        let executable = root.path().join("claude");
        std::fs::write(&executable, FAKE_CLAUDE).expect("fixture script");
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
            .expect("fixture mode");
        let record = root.path().join("record.txt");
        let mut parent_env = BTreeMap::new();
        parent_env.insert("PATH".to_owned(), std::env::var("PATH").unwrap_or_default());
        parent_env.insert(
            "HOME".to_owned(),
            root.path().to_string_lossy().into_owned(),
        );
        parent_env.insert("FAKE_CLAUDE_MODE".to_owned(), mode.to_owned());
        parent_env.insert(
            "FAKE_CLAUDE_RECORD".to_owned(),
            record.to_string_lossy().into_owned(),
        );
        for (name, value) in extra_env {
            parent_env.insert((*name).to_owned(), (*value).to_owned());
        }
        let run = ClaudeCliRun {
            executable,
            config_dir: root.path().join("account"),
            parent_env: Some(parent_env),
        };
        Self {
            _root: root,
            run,
            record,
        }
    }

    pub(crate) fn recorded(&self) -> String {
        std::fs::read_to_string(&self.record).unwrap_or_default()
    }

    pub(crate) fn recorded_pid(&self) -> String {
        self.recorded()
            .lines()
            .find_map(|line| line.strip_prefix("pid="))
            .unwrap_or_default()
            .to_owned()
    }
}
