use super::{
    args::{Args, is_valid_thinking_level},
    file_processor::process_file_arguments,
};
use maho_interactive::attach_startup::{AttachPrompt, AttachStartup, AttachStartupModel};
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub enum HeldSessionAttachPlan {
    Startup(AttachStartup),
    Refusal(String),
}

type UnappliedFlag = (&'static str, fn(&Args) -> bool);

const UNAPPLIED_FLAGS: &[UnappliedFlag] = &[
    ("--system-prompt", |parsed| parsed.system_prompt.is_some()),
    ("--append-system-prompt", |parsed| {
        !parsed.append_system_prompt.is_empty()
    }),
    ("--api-key", |parsed| parsed.api_key.is_some()),
    ("--models", |parsed| parsed.models.is_some()),
    ("--tools", |parsed| parsed.tools.is_some()),
    ("--exclude-tools", |parsed| parsed.exclude_tools.is_some()),
    ("--no-tools", |parsed| parsed.no_tools),
    ("--no-builtin-tools", |parsed| parsed.no_builtin_tools),
    ("--tool-runtime", |parsed| parsed.tool_runtime.is_some()),
    ("--extension", |parsed| {
        !user_extension_arguments(parsed).is_empty()
    }),
    ("--no-extensions", |parsed| parsed.no_extensions),
    ("--skill", |parsed| !parsed.skills.is_empty()),
    ("--no-skills", |parsed| parsed.no_skills),
    ("--prompt-template", |parsed| {
        !parsed.prompt_templates.is_empty()
    }),
    ("--no-prompt-templates", |parsed| parsed.no_prompt_templates),
    ("--theme", |parsed| !parsed.themes.is_empty()),
    ("--no-themes", |parsed| parsed.no_themes),
    ("--no-context-files", |parsed| parsed.no_context_files),
    ("--name", |parsed| parsed.name.is_some()),
    ("--offline", |parsed| parsed.offline),
    ("--approve/--no-approve", |parsed| {
        parsed.project_trust_override.is_some()
    }),
    ("--verbose", |parsed| parsed.verbose),
    ("--grok-neo", |parsed| parsed.grok_neo),
    ("- (stdin)", |parsed| parsed.read_stdin),
];
/// The `--extension` paths that are not this distribution's own (omo#14465).
///
/// A launcher prepends its plugin with `--extension` on every start, and an attaching start applies the
/// command line to a session another process already runs, so that flag was refused. The brand profile
/// naming the same plugin is how the engine tells the distribution's own extension from a user's: a
/// path the profile lists is not a user flag, and a launch carrying only that path attaches.
pub fn user_extension_arguments(parsed: &Args) -> Vec<&String> {
    let branded = maho_core::brand::brand_profile()
        .extensions
        .unwrap_or_default();
    extensions_except(parsed, &branded)
}

/// `parsed`'s `--extension` paths minus `branded`; the pure form of the check above.
pub fn extensions_except<'a>(parsed: &'a Args, branded: &[String]) -> Vec<&'a String> {
    parsed
        .extensions
        .iter()
        .filter(|path| !branded.iter().any(|own| same_extension_path(own, path)))
        .collect()
}

/// A profile names absolute paths; a launch may spell one with a trailing separator or through a
/// symlinked parent, so both sides compare resolved where the filesystem can resolve them.
fn same_extension_path(branded: &str, given: &str) -> bool {
    let trimmed = |value: &str| value.trim_end_matches(['/', '\\']).to_owned();
    if trimmed(branded) == trimmed(given) {
        return true;
    }
    let resolve = |value: &str| std::fs::canonicalize(value).ok();
    matches!((resolve(branded), resolve(given)), (Some(own), Some(given)) if own == given)
}

fn unapplied_flags(parsed: &Args) -> Vec<String> {
    let mut flags: Vec<String> = UNAPPLIED_FLAGS
        .iter()
        .filter(|(_, is_given)| is_given(parsed))
        .map(|(flag, _)| (*flag).to_owned())
        .collect();
    flags.extend(parsed.unknown_flags.keys().map(|flag| format!("--{flag}")));
    flags
}

fn refusal(reason: impl AsRef<str>) -> HeldSessionAttachPlan {
    HeldSessionAttachPlan::Refusal(format!(
        "{} This session runs in a multi-session host, so this terminal attaches to it and applies the command line through the session's own commands.",
        reason.as_ref()
    ))
}

pub async fn held_session_attach_plan(
    parsed: &Args,
    cwd: &Path,
    auto_resize_images: Option<bool>,
) -> Result<HeldSessionAttachPlan, String> {
    let unapplied = unapplied_flags(parsed);
    if !unapplied.is_empty() {
        let pronoun = if unapplied.len() == 1 { "it" } else { "them" };
        return Ok(refusal(format!(
            "{} cannot be applied to a session another process already runs; start without {pronoun}.",
            unapplied.join(", ")
        )));
    }
    let mut thinking = parsed.thinking.clone();
    let mut model = None;
    if let Some(raw_model) = &parsed.model {
        let mut provider = parsed.provider.clone();
        let mut model_id = raw_model.clone();
        if provider.is_none() {
            let Some(slash) = model_id.find('/').filter(|&idx| idx > 0) else {
                return Ok(refusal(format!(
                    "--model {raw_model} names no provider: use --provider <name> --model <id> or --model <provider>/<id>."
                )));
            };
            provider = Some(model_id[..slash].to_owned());
            model_id = model_id[slash + 1..].to_owned();
        }
        if let Some(colon) = model_id.rfind(':').filter(|&idx| idx > 0) {
            let suffix = &model_id[colon + 1..];
            if is_valid_thinking_level(suffix) {
                if thinking.is_none() {
                    thinking = Some(suffix.to_owned());
                }
                model_id.truncate(colon);
            }
        }
        if model_id.is_empty() || model_id.contains('*') {
            return Ok(refusal(format!(
                "--model {raw_model} is a pattern; name one model as <provider>/<id>."
            )));
        }
        model = Some(AttachStartupModel {
            provider: provider.unwrap_or_default(),
            model_id,
        });
    } else if let Some(provider) = &parsed.provider {
        return Ok(refusal(format!("--provider {provider} needs --model.")));
    }
    let prompts = attach_prompts(parsed, cwd, auto_resize_images).await?;
    Ok(HeldSessionAttachPlan::Startup(AttachStartup {
        model,
        thinking,
        prompts,
    }))
}

async fn attach_prompts(
    parsed: &Args,
    cwd: &Path,
    auto_resize_images: Option<bool>,
) -> Result<Vec<AttachPrompt>, String> {
    let mut iter = parsed.messages.iter();
    let first = iter.next();
    let rest: Vec<AttachPrompt> = iter
        .map(|text| AttachPrompt {
            text: text.clone(),
            images: Vec::new(),
        })
        .collect();
    if parsed.file_args.is_empty() {
        let mut out = Vec::new();
        if let Some(first) = first {
            out.push(AttachPrompt {
                text: first.clone(),
                images: Vec::new(),
            });
        }
        out.extend(rest);
        return Ok(out);
    }
    let processed = process_file_arguments(&parsed.file_args, cwd, auto_resize_images).await?;
    let mut out = vec![AttachPrompt {
        text: format!("{}{}", processed.text, first.map_or("", String::as_str)),
        images: processed.images,
    }];
    out.extend(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::{ParseArgsOptions, parse_args_with_options};

    fn parse(raw: &[&str]) -> Args {
        let owned: Vec<String> = raw.iter().map(|s| (*s).to_owned()).collect();
        parse_args_with_options(&owned, ParseArgsOptions::default()).expect("parse_args")
    }

    #[tokio::test]
    async fn refuses_start_flags_an_attaching_start_cannot_apply() {
        let cases: &[(&[&str], &str)] = &[
            (&["--append-system-prompt", "x"], "--append-system-prompt"),
            (&["--api-key", "k"], "--api-key"),
            (&["--models", "a/b,c/d"], "--models"),
            (
                &["--tools", "read", "--no-builtin-tools"],
                "--tools, --no-builtin-tools",
            ),
            (&["--tool-runtime", "child"], "--tool-runtime"),
            (
                &["--extension", "e.ts", "--no-skills"],
                "--extension, --no-skills",
            ),
            (
                &["--no-context-files", "--approve"],
                "--no-context-files, --approve/--no-approve",
            ),
            (&["--name", "n", "--offline"], "--name, --offline"),
            (&["--my-flag", "v"], "--my-flag"),
            (&["-"], "- (stdin)"),
        ];
        for (args, expected_flag) in cases {
            let mut full = args.to_vec();
            full.push("hello");
            let plan = held_session_attach_plan(&parse(&full), Path::new("/tmp"), Some(false))
                .await
                .expect("plan");
            let HeldSessionAttachPlan::Refusal(msg) = plan else {
                panic!("expected refusal for {expected_flag}");
            };
            assert!(
                msg.contains(&format!(
                    "{expected_flag} cannot be applied to a session another process already runs"
                )),
                "{msg}"
            );
        }
    }

    #[tokio::test]
    async fn builds_startup_for_supported_flags_and_refuses_patterns_or_missing_provider() {
        let plan = held_session_attach_plan(
            &parse(&[
                "--model",
                "p/m:low",
                "--use-theme",
                "light",
                "--tui-mode",
                "fullscreen",
                "hi",
            ]),
            Path::new("/tmp"),
            Some(false),
        )
        .await
        .expect("plan");
        assert_eq!(
            plan,
            HeldSessionAttachPlan::Startup(AttachStartup {
                model: Some(AttachStartupModel {
                    provider: "p".into(),
                    model_id: "m".into(),
                }),
                thinking: Some("low".into()),
                prompts: vec![AttachPrompt {
                    text: "hi".into(),
                    images: Vec::new(),
                }],
            })
        );

        let no_provider = held_session_attach_plan(
            &parse(&["--model", "claude*", "hello"]),
            Path::new("/tmp"),
            Some(false),
        )
        .await
        .expect("plan");
        let HeldSessionAttachPlan::Refusal(msg) = no_provider else {
            panic!("expected refusal");
        };
        assert!(msg.contains("--model claude* names no provider"), "{msg}");
    }

    #[tokio::test]
    async fn an_extension_the_brand_profile_names_is_not_a_user_flag() {
        // given a plugin directory a distribution's launcher names in its brand profile (#14465)
        let home = tempfile::tempdir().expect("tempdir");
        let plugin = home.path().join("plugin");
        std::fs::create_dir(&plugin).expect("plugin dir");
        let branded_path = plugin.to_string_lossy().into_owned();
        let branded = parse(&["--extension", &branded_path, "hello"]);
        let with_user_flag = parse(&[
            "--extension",
            &branded_path,
            "--extension",
            "/tmp/user-ext.ts",
            "hello",
        ]);
        // then the branded path alone counts as the distribution's own, and a user path beside it does not
        assert!(extensions_except(&branded, std::slice::from_ref(&branded_path)).is_empty());
        assert_eq!(
            extensions_except(&with_user_flag, std::slice::from_ref(&branded_path)),
            vec![&"/tmp/user-ext.ts".to_owned()]
        );
        // a trailing separator and a symlinked parent name the same plugin
        assert!(
            extensions_except(&branded, std::slice::from_ref(&format!("{branded_path}/")))
                .is_empty()
        );
        #[cfg(unix)]
        {
            let link = home.path().join("link");
            std::os::unix::fs::symlink(&plugin, &link).expect("symlink");
            let through_link = parse(&["--extension", &link.join(".").to_string_lossy(), "hello"]);
            assert!(
                extensions_except(&through_link, std::slice::from_ref(&branded_path)).is_empty(),
                "a resolved path names the same plugin"
            );
        }
        // and with no injected profile nothing is the distribution's own
        assert_eq!(extensions_except(&branded, &[]), vec![&branded_path]);
    }
}
