use crate::todo_types::{TodoAsk, TodoItem, TodoPhase, TodoStatus};
pub use maho_ext_prompt_preset::contract::HandoffCueKind;
use regex::Regex;
use std::sync::LazyLock;

pub fn sanitize_todo_text(text: &str) -> String {
    static ANSI: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
        r"(?:\x1b\][\s\S]*?(?:\x07|\x1b\\|\x{009c}))|[\x1b\x{009b}][\[\]()#;?]*(?:[0-9]{1,4}(?:[;:][0-9]{0,4})*)?[0-9A-PR-TZcf-nq-uy=><~]"
    ).unwrap_or_else(|error| panic!("invalid static ANSI expression: {error}"))
    });
    let stripped = ANSI.replace_all(text, "");
    let cleaned: String = stripped
        .chars()
        .map(|c| {
            if c <= '\u{001f}' || ('\u{007f}'..='\u{009f}').contains(&c) {
                ' '
            } else {
                c
            }
        })
        .collect();
    cleaned
        .split(|c: char| c.is_whitespace() || c == '\u{feff}')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn is_terminal_todo_status(status: &str) -> bool {
    matches!(status, "completed" | "abandoned" | "cancelled")
}
pub fn is_incomplete_todo(todo: &TodoItem) -> bool {
    matches!(todo.status, TodoStatus::Pending | TodoStatus::InProgress)
}
pub fn get_todo_marker(status: &str) -> &'static str {
    match status {
        "completed" => "[✓]",
        "in_progress" => "[•]",
        "abandoned" | "cancelled" => "[×]",
        _ => "[ ]",
    }
}
fn status_text(status: TodoStatus) -> &'static str {
    match status {
        TodoStatus::Pending => "pending",
        TodoStatus::InProgress => "in_progress",
        TodoStatus::Completed => "completed",
        TodoStatus::Abandoned => "abandoned",
    }
}
pub fn get_todo_result_lines(phases: &[TodoPhase]) -> Vec<String> {
    let mut lines = vec![format!(
        "{} todos",
        phases
            .iter()
            .flat_map(|p| &p.tasks)
            .filter(|t| is_incomplete_todo(t))
            .count()
    )];
    for phase in phases {
        lines.push(format!("{}:", sanitize_todo_text(&phase.name)));
        lines.extend(phase.tasks.iter().map(|t| {
            format!(
                "{} {}",
                get_todo_marker(status_text(t.status)),
                sanitize_todo_text(&t.content)
            )
        }));
    }
    lines
}
pub fn describe_ask_now_next(
    phases: &[TodoPhase],
    ask: Option<&TodoAsk>,
) -> (String, String, String) {
    let ask = ask
        .map_or("(no user request captured)", |value| value.text.as_str())
        .to_owned();
    let located = phases
        .iter()
        .flat_map(|phase| phase.tasks.iter().map(move |task| (phase, task)))
        .collect::<Vec<_>>();
    if located.is_empty() {
        return (ask, "none - no todo list".into(), "none".into());
    }
    let current = crate::todo_query::next_actionable_task(phases);
    let now = located
        .iter()
        .find(|(_, task)| current.is_some_and(|current| std::ptr::eq(*task, current)))
        .map_or_else(
            || "none - all tasks closed".into(),
            |(phase, task)| format!("{} ({})", task.content, phase.name),
        );
    let next = located
        .iter()
        .find(|(_, task)| {
            task.status == TodoStatus::Pending
                && current.is_none_or(|current| !std::ptr::eq(*task, current))
        })
        .map_or_else(
            || "none".into(),
            |(phase, task)| format!("{} ({})", task.content, phase.name),
        );
    (ask, now, next)
}
/// senpi `HandoffMoment`: the todo transitions that owe the user a handoff block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandoffMoment {
    ListCreated,
    PhaseClosed,
    AllClosed,
}
/// senpi `handoffMomentOf`: which handoff moment a todo mutation reached, if any.
pub fn handoff_moment_of(
    before: &[TodoPhase],
    after: &[TodoPhase],
    creates_list: bool,
) -> Option<HandoffMoment> {
    let has_open = |tasks: &[TodoItem]| tasks.iter().any(is_incomplete_todo);
    let after_tasks = after
        .iter()
        .flat_map(|phase| phase.tasks.iter().cloned())
        .collect::<Vec<_>>();
    if after_tasks.len() < 2 {
        return None;
    }
    if creates_list {
        return Some(HandoffMoment::ListCreated);
    }
    if !has_open(&after_tasks) {
        let before_tasks = before
            .iter()
            .flat_map(|phase| phase.tasks.iter().cloned())
            .collect::<Vec<_>>();
        return has_open(&before_tasks).then_some(HandoffMoment::AllClosed);
    }
    let closed_phase = after.iter().any(|phase| {
        !phase.tasks.is_empty()
            && !has_open(&phase.tasks)
            && before
                .iter()
                .find(|prior| prior.name == phase.name)
                .is_some_and(|prior| has_open(&prior.tasks))
    });
    closed_phase.then_some(HandoffMoment::PhaseClosed)
}
pub fn handoff_cue_text(moment: HandoffMoment) -> &'static str {
    match moment {
        HandoffMoment::ListCreated => {
            "Handoff due: the plan exists. Before your next tool call, write the Ask / For you / Now / Next block."
        }
        HandoffMoment::PhaseClosed => {
            "Handoff due: a phase closed. Before your next tool call, write the Ask / For you / Now / Next block."
        }
        HandoffMoment::AllClosed => {
            "Handoff due: every task is closed. Your final message is the Ask / For you / Now / Next block, with Now and Next saying no task remains; any other report an instruction asks for (a self-review, a checklist, a summary) goes inside For you."
        }
    }
}
/// The labels of the handoff block every GPT block preset renders (gpt-5.5, gpt-5.6, gpt-6-astra).
pub const GPT_HANDOFF_LABELS: &str = "Outcome so far / You need / Now / Next";

pub fn gpt_handoff_cue_text(moment: HandoffMoment) -> String {
    match moment {
        HandoffMoment::ListCreated => format!(
            "Handoff due: the plan exists. Before your next tool call, write the {GPT_HANDOFF_LABELS} handoff block."
        ),
        HandoffMoment::PhaseClosed => format!(
            "Handoff due: a phase closed. Before your next tool call, write the {GPT_HANDOFF_LABELS} handoff block."
        ),
        HandoffMoment::AllClosed => "Handoff due: every task is closed. Your final message is the Outcome so far / You need / Now / Next handoff block, with Now and Next saying no task remains; any other report an instruction asks for (a self-review, a checklist, a summary) goes inside You need.".to_owned(),
    }
}

/// senpi `formatHandoffCue`: the cue line appended to a todo result, or nothing when no handoff moment
/// was reached or the active preset defines no handoff block to cue (senpi#6977).
pub fn format_handoff_cue_with_kind(moment: Option<HandoffMoment>, kind: HandoffCueKind) -> String {
    let Some(moment) = moment else {
        return String::new();
    };
    match kind {
        HandoffCueKind::None => String::new(),
        HandoffCueKind::GptBlock => format!("\n\n{}", gpt_handoff_cue_text(moment)),
        HandoffCueKind::Default => format!("\n\n{}", handoff_cue_text(moment)),
    }
}

pub fn format_handoff_cue(moment: Option<HandoffMoment>) -> String {
    format_handoff_cue_with_kind(moment, HandoffCueKind::Default)
}
pub fn format_ask_now_next_header(phases: &[TodoPhase], ask: Option<&TodoAsk>) -> String {
    let (ask, now, next) = describe_ask_now_next(phases, ask);
    format!("Ask: {ask}\nNow: {now}\nNext: {next}\n\n")
}
pub fn format_summary(phases: &[TodoPhase], errors: &[String], read_only: bool) -> String {
    let total = phases.iter().map(|p| p.tasks.len()).sum::<usize>();
    if total == 0 {
        return if !errors.is_empty() {
            format!("Errors: {}", errors.join("; "))
        } else if read_only {
            "Todo list is empty.".into()
        } else {
            "Todo list cleared.".into()
        };
    }
    let remaining: Vec<_> = phases
        .iter()
        .flat_map(|p| {
            p.tasks
                .iter()
                .filter(|t| is_incomplete_todo(t))
                .map(move |t| (p, t))
        })
        .collect();
    let current_idx = phases
        .iter()
        .position(|p| p.tasks.iter().any(is_incomplete_todo))
        .unwrap_or(phases.len() - 1);
    let current = &phases[current_idx];
    let closed = |p: &TodoPhase| p.tasks.iter().filter(|t| !is_incomplete_todo(t)).count();
    let mut lines = vec![];
    if !errors.is_empty() {
        lines.push(format!("Errors: {}", errors.join("; ")));
    }
    if remaining.is_empty() {
        lines.push("Remaining items: none.".into());
    } else {
        lines.push(format!("Remaining items ({}):", remaining.len()));
        lines.extend(
            remaining.iter().map(|(p, t)| {
                format!("  - {} [{}] ({})", t.content, status_text(t.status), p.name)
            }),
        );
    }
    lines.push(format!(
        "Overall: {}/{} done, {} open.",
        phases.iter().map(closed).sum::<usize>(),
        total,
        remaining.len()
    ));
    let suffix = if phases.iter().skip(current_idx + 1).any(|p| closed(p) > 0) {
        " — earliest phase with open tasks; the in-progress pointer auto-advances to the earliest open task on each completion, so it can sit behind out-of-order work (nothing was un-completed)."
    } else {
        "."
    };
    lines.push(format!(
        "Active phase {}/{} \"{}\" ({}/{}){}",
        current_idx + 1,
        phases.len(),
        current.name,
        closed(current),
        current.tasks.len(),
        suffix
    ));
    for p in phases {
        lines.push(format!("  {}:", p.name));
        for t in &p.tasks {
            let checkbox = if t.status == TodoStatus::Completed {
                "[X]"
            } else {
                "[ ]"
            };
            let tag = match t.status {
                TodoStatus::InProgress => " (in progress)",
                TodoStatus::Abandoned => " (dropped)",
                TodoStatus::Pending | TodoStatus::Completed => "",
            };
            lines.push(format!("    - {checkbox} {}{tag}", t.content));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sanitizes_ansi_controls_and_whitespace() {
        assert_eq!(
            sanitize_todo_text("\x1b[31mDeploy\x1b[0m\r\n  API\x00\u{feff}"),
            "Deploy API"
        );
    }
    #[test]
    fn strips_osc_hyperlink_but_retains_label() {
        assert_eq!(
            sanitize_todo_text("\x1b]8;;https://example.org\x07label\x1b]8;;\x1b\\"),
            "label"
        );
    }
    #[test]
    fn source_whitespace_and_control_codepoint_matrix() {
        for codepoint in 0..=0x10ffff {
            let Some(character) = char::from_u32(codepoint) else {
                continue;
            };
            let source_space = matches!(character,'\u{0000}'..='\u{0020}'|'\u{007f}'..='\u{009f}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}');
            if matches!(character, '\u{001b}' | '\u{009b}') {
                continue;
            }
            let input = format!("a{character}z");
            assert_eq!(
                sanitize_todo_text(&input),
                if source_space { "a z".into() } else { input },
                "U+{codepoint:04X}"
            );
        }
    }
    #[test]
    fn handoff_moments_match_list_creation_phase_close_and_all_close() {
        let task = |content: &str, status| TodoItem {
            content: content.into(),
            status,
        };
        let open = vec![
            TodoPhase {
                name: "A".into(),
                tasks: vec![task("a", TodoStatus::InProgress)],
            },
            TodoPhase {
                name: "B".into(),
                tasks: vec![task("b", TodoStatus::Pending)],
            },
        ];
        assert_eq!(
            handoff_moment_of(&[], &open, true),
            Some(HandoffMoment::ListCreated)
        );
        assert_eq!(handoff_moment_of(&[], &open[..1], true), None);
        let phase_closed = vec![
            TodoPhase {
                name: "A".into(),
                tasks: vec![task("a", TodoStatus::Completed)],
            },
            open[1].clone(),
        ];
        assert_eq!(
            handoff_moment_of(&open, &phase_closed, false),
            Some(HandoffMoment::PhaseClosed)
        );
        assert_eq!(handoff_moment_of(&phase_closed, &phase_closed, false), None);
        let all_closed = vec![
            phase_closed[0].clone(),
            TodoPhase {
                name: "B".into(),
                tasks: vec![task("b", TodoStatus::Abandoned)],
            },
        ];
        assert_eq!(
            handoff_moment_of(&phase_closed, &all_closed, false),
            Some(HandoffMoment::AllClosed)
        );
        assert_eq!(handoff_moment_of(&all_closed, &all_closed, false), None);
        assert_eq!(format_handoff_cue(None), "");
        assert!(
            format_handoff_cue(Some(HandoffMoment::PhaseClosed))
                .starts_with("\n\nHandoff due: a phase closed.")
        );
    }
    #[test]
    fn cue_names_the_handoff_block_the_active_preset_defines() {
        use maho_core::dynamic_prompt::build::BuildDynamicSystemPromptOptions;
        use maho_ext_prompt_preset::{
            contract::{PROMPT_PRESET_MODEL_CASES, preset_handoff_cue_kind},
            presets::{ModelMetadata, resolve_preset},
            settings::PromptPresetName,
        };
        for case in PROMPT_PRESET_MODEL_CASES {
            let prompt = resolve_preset(
                ModelMetadata {
                    id: case.model_id,
                    provider: case.provider_id,
                    name: None,
                    prompt_preset: None,
                },
                PromptPresetName::Auto,
                BuildDynamicSystemPromptOptions::default(),
            )
            .map(|resolved| resolved.prompt)
            .unwrap_or_default();
            let expected = if case.preset == PromptPresetName::Gpt62 {
                HandoffCueKind::None
            } else if prompt.contains("[Outcome so far] toward") {
                HandoffCueKind::GptBlock
            } else {
                HandoffCueKind::Default
            };
            assert_eq!(
                preset_handoff_cue_kind(Some(case.preset)),
                expected,
                "{}",
                case.preset.as_str()
            );
        }
        assert_eq!(preset_handoff_cue_kind(None), HandoffCueKind::Default);
        for id in ["gpt-5.5", "gpt-5.6", "gpt-6-astra"] {
            let prompt = resolve_preset(
                ModelMetadata {
                    id,
                    provider: "openai",
                    name: None,
                    prompt_preset: None,
                },
                PromptPresetName::Auto,
                BuildDynamicSystemPromptOptions::default(),
            )
            .expect("gpt preset")
            .prompt;
            assert!(prompt.contains("[Outcome so far] toward"), "{id}");
            assert!(prompt.contains("You need:"), "{id}");
            assert!(prompt.contains("Now:"), "{id}");
            assert!(prompt.contains("Next:"), "{id}");
        }
        for moment in [HandoffMoment::ListCreated, HandoffMoment::PhaseClosed] {
            assert!(gpt_handoff_cue_text(moment).contains(GPT_HANDOFF_LABELS));
        }
        assert!(gpt_handoff_cue_text(HandoffMoment::AllClosed).contains("goes inside You need"));
    }
    #[test]
    fn result_counts_only_open_tasks() {
        let p = vec![TodoPhase {
            name: "Tasks".into(),
            tasks: vec![
                TodoItem {
                    content: "Done".into(),
                    status: TodoStatus::Completed,
                },
                TodoItem {
                    content: "Work".into(),
                    status: TodoStatus::Pending,
                },
            ],
        }];
        assert_eq!(
            get_todo_result_lines(&p),
            vec!["1 todos", "Tasks:", "[✓] Done", "[ ] Work"]
        );
    }
    #[test]
    fn empty_summary_respects_read_only_and_errors() {
        assert_eq!(format_summary(&[], &[], true), "Todo list is empty.");
        assert_eq!(format_summary(&[], &[], false), "Todo list cleared.");
        assert_eq!(
            format_summary(&[], &["missing".into()], false),
            "Errors: missing"
        );
    }
    #[test]
    fn ask_now_next_header_tracks_active_and_next_tasks() {
        let phases = vec![TodoPhase {
            name: "Build".into(),
            tasks: vec![
                TodoItem {
                    content: "Compile".into(),
                    status: TodoStatus::InProgress,
                },
                TodoItem {
                    content: "Test".into(),
                    status: TodoStatus::Pending,
                },
            ],
        }];
        let ask = TodoAsk {
            entry_id: "m1".into(),
            text: "Ship it".into(),
            captured_at: 1,
        };
        assert_eq!(
            format_ask_now_next_header(&phases, Some(&ask)),
            "Ask: Ship it\nNow: Compile (Build)\nNext: Test (Build)\n\n"
        );
    }
}
