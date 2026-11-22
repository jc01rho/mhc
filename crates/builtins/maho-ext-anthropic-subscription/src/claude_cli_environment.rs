//! Environment for every `claude auth *` spawn of a CLI-store account (senpi `claude-cli-environment.ts`, senpi#4437).
use crate::auth_environment::strip_managed_auth_environment;
use std::{collections::BTreeMap, path::Path};

/// Host variables that would redirect a `claude` CLI-store account away from its own dir: the secure-store redirect
/// (undocumented, moves only the keychain item), an inherited config dir, and the host's browser.
const HOST_REDIRECT_NAMES: [&str; 3] = [
    "CLAUDE_SECURESTORAGE_CONFIG_DIR",
    "CLAUDE_CONFIG_DIR",
    "BROWSER",
];

/// Inherited credentials the CLI reads from a file descriptor; they would beat the account's own login (senpi#4436).
const INHERITED_CREDENTIAL_DESCRIPTORS: [&str; 2] = [
    "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
    "CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR",
];

pub struct ClaudeCliEnvironmentOptions<'a> {
    /// The account's own config dir; the CLI keys its credential store on this path.
    pub config_dir: &'a Path,
    /// The login's browser helper; omitted for status and logout.
    pub browser: Option<&'a Path>,
}

/// Environment for every `claude auth *` spawn of a CLI-store account. An inherited `CLAUDE_CODE_OAUTH_TOKEN` or
/// `ANTHROPIC_API_KEY` takes precedence over the stored claude.ai login inside the CLI, so they are stripped with the
/// rest of the managed auth set before the account dir is pinned.
pub fn build_claude_cli_environment(
    parent: &BTreeMap<String, String>,
    options: &ClaudeCliEnvironmentOptions<'_>,
) -> BTreeMap<String, String> {
    let mut environment = strip_managed_auth_environment(parent);
    for name in HOST_REDIRECT_NAMES
        .iter()
        .chain(INHERITED_CREDENTIAL_DESCRIPTORS.iter())
    {
        environment.remove(*name);
    }
    environment.insert(
        "CLAUDE_CONFIG_DIR".to_owned(),
        options.config_dir.to_string_lossy().into_owned(),
    );
    environment.insert("NO_COLOR".to_owned(), "1".to_owned());
    if let Some(browser) = options.browser {
        environment.insert("BROWSER".to_owned(), browser.to_string_lossy().into_owned());
    }
    environment
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn the_account_dir_is_pinned_and_inherited_credentials_and_redirects_are_dropped() {
        let parent = environment(&[
            ("PATH", "/usr/bin"),
            ("HOME", "/home/user"),
            ("CLAUDE_CODE_OAUTH_TOKEN", "token"),
            ("CLAUDE_CODE_OAUTH_TOKEN_2", "other"),
            ("ANTHROPIC_API_KEY", "key"),
            ("SENPI_PRIVATE", "marker"),
            ("CLAUDE_CONFIG_DIR", "/host/claude"),
            ("CLAUDE_SECURESTORAGE_CONFIG_DIR", "/host/secure"),
            ("BROWSER", "/usr/bin/firefox"),
            ("CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR", "3"),
            ("CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR", "4"),
        ]);

        let child = build_claude_cli_environment(
            &parent,
            &ClaudeCliEnvironmentOptions {
                config_dir: Path::new("/accounts/work"),
                browser: None,
            },
        );

        assert_eq!(
            child,
            environment(&[
                ("PATH", "/usr/bin"),
                ("HOME", "/home/user"),
                ("CLAUDE_CONFIG_DIR", "/accounts/work"),
                ("NO_COLOR", "1"),
            ])
        );
    }

    #[test]
    fn a_login_gets_its_own_browser_helper_instead_of_the_hosts() {
        let parent = environment(&[("BROWSER", "/usr/bin/firefox")]);

        let child = build_claude_cli_environment(
            &parent,
            &ClaudeCliEnvironmentOptions {
                config_dir: Path::new("/accounts/work"),
                browser: Some(Path::new("/tmp/helper")),
            },
        );

        assert_eq!(child["BROWSER"], "/tmp/helper");
    }
}
