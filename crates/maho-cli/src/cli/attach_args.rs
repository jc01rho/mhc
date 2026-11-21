//! Port of senpi `cli/attach-args.ts`: the `--attach` flag is resolved before mode selection or any
//! local session initialization.
use super::args::{Args, Diagnostic, FlagValue, Mode};

/// Takes `--attach <address>` out of the unknown flags and records every refusal of the
/// combinations an attaching start cannot honour.
pub fn apply_attach_args(args: &mut Args) {
    let Some(value) = args.unknown_flags.remove("attach") else {
        return;
    };
    let mut errors: Vec<&str> = Vec::new();
    let address = match value {
        FlagValue::String(address) if !address.is_empty() => Some(address),
        _ => None,
    };
    let Some(address) = address else {
        push_error(args, "--attach requires a local RPC endpoint address");
        return;
    };
    args.attach = Some(address);
    if args.mode.as_ref().is_some_and(|mode| *mode != Mode::Text) {
        errors.push("--attach is interactive and cannot be combined with --mode rpc or json");
    }
    if args.session.is_none() && !args.help && !args.version {
        errors.push("--attach requires --session <ref>");
    }
    if args.print || args.read_stdin || !args.messages.is_empty() || !args.file_args.is_empty() {
        errors.push(
            "--attach needs interactive input; omit --print, stdin input, prompt arguments and @files",
        );
    }
    if args.continue_session
        || args.resume
        || args.fork.is_some()
        || args.rebind.is_some()
        || args.session_id.is_some()
        || args.no_session
        || args.listen.is_some()
        || args.multi_session
    {
        errors.push(
            "--attach selects an existing session; omit session creation, resume and listener options",
        );
    }
    if args.provider.is_some()
        || args.model.is_some()
        || args.api_key.is_some()
        || args.system_prompt.is_some()
        || !args.append_system_prompt.is_empty()
        || args.export.is_some()
    {
        errors.push(
            "--attach uses the endpoint's session configuration; omit model, prompt configuration and export options",
        );
    }
    for message in errors {
        push_error(args, message);
    }
}

fn push_error(args: &mut Args, message: &str) {
    args.diagnostics.push(Diagnostic {
        error: true,
        message: message.to_owned(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::parse_args;

    fn parse(args: &[&str]) -> Args {
        let argv: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        parse_args(&argv, false).unwrap()
    }

    fn errors(args: &Args) -> Vec<&str> {
        args.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.error)
            .map(|diagnostic| diagnostic.message.as_str())
            .collect()
    }

    #[test]
    fn attach_with_a_session_is_accepted_and_leaves_the_unknown_flags() {
        let args = parse(&["--attach", "/tmp/host.sock", "--session", "abc"]);
        assert_eq!(args.attach.as_deref(), Some("/tmp/host.sock"));
        assert_eq!(args.session.as_deref(), Some("abc"));
        assert!(errors(&args).is_empty());
        assert!(!args.unknown_flags.contains_key("attach"));
    }

    #[test]
    fn attach_without_a_value_or_session_is_refused() {
        let args = parse(&["--attach"]);
        assert_eq!(args.attach, None);
        assert_eq!(
            errors(&args),
            ["--attach requires a local RPC endpoint address"]
        );
        let args = parse(&["--attach", "/tmp/host.sock"]);
        assert_eq!(errors(&args), ["--attach requires --session <ref>"]);
    }

    #[test]
    fn attach_session_requirement_does_not_block_help_or_version() {
        assert!(errors(&parse(&["--attach", "/tmp/host.sock", "--help"])).is_empty());
        assert!(errors(&parse(&["--attach", "/tmp/host.sock", "--version"])).is_empty());
    }

    #[test]
    fn attach_refuses_every_option_that_would_start_something_local() {
        let session = ["--attach", "/tmp/host.sock", "--session", "abc"];
        let interactive = "--attach needs interactive input; omit --print, stdin input, prompt arguments and @files";
        let existing = "--attach selects an existing session; omit session creation, resume and listener options";
        for (extra, message) in [
            (
                &["--mode", "rpc"][..],
                "--attach is interactive and cannot be combined with --mode rpc or json",
            ),
            (&["--print"][..], interactive),
            (&["hello"][..], interactive),
            (&["--continue"][..], existing),
            (&["--no-session"][..], existing),
            (
                &["--model", "m"][..],
                "--attach uses the endpoint's session configuration; omit model, prompt configuration and export options",
            ),
        ] {
            let mut argv = session.to_vec();
            argv.extend_from_slice(extra);
            let args = parse(&argv);
            assert!(
                errors(&args).contains(&message),
                "{extra:?} -> {:?}",
                errors(&args)
            );
        }
    }
}
