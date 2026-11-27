#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileMutationMode {
    ApplyPatch,
    EditWrite,
    None,
}

pub struct FileMutationRouting {
    pub mode: FileMutationMode,
    pub tools: Vec<&'static str>,
}

pub fn resolve_file_mutation_routing(tool_names: &[String]) -> FileMutationRouting {
    if tool_names.iter().any(|name| name == "apply_patch") {
        return FileMutationRouting {
            mode: FileMutationMode::ApplyPatch,
            tools: vec!["apply_patch"],
        };
    }
    let tools: Vec<_> = ["edit", "write"]
        .into_iter()
        .filter(|name| tool_names.iter().any(|tool| tool == name))
        .collect();
    FileMutationRouting {
        mode: if tools.is_empty() {
            FileMutationMode::None
        } else {
            FileMutationMode::EditWrite
        },
        tools,
    }
}

/// senpi `formatToolList`: `a`, `a` and `b`, `a`, `b` and `c`.
pub fn format_tool_list(tools: &[&str]) -> String {
    let quoted: Vec<String> = tools.iter().map(|name| format!("`{name}`")).collect();
    match quoted.split_last() {
        None => String::new(),
        Some((last, [])) => last.clone(),
        Some((last, init)) => format!("{} and {last}", init.join(", ")),
    }
}

pub fn build_file_operations_tuning(tool_names: &[String]) -> String {
    let routing = resolve_file_mutation_routing(tool_names);
    let mut paragraphs = Vec::new();
    if routing.mode != FileMutationMode::None {
        let quoted = format_tool_list(&routing.tools);
        paragraphs.push(format!("Use {quoted} for ALL file edits and creations. Do NOT write or modify files via bash heredoc (`cat >`, `echo > `), `sed -i`, `awk -i`, or inline `python`/`python3 -c` scripts."));
    }
    if tool_names.iter().any(|name| name == "read") {
        paragraphs.push("Use `read` for ALL file inspection. Do NOT substitute `cat`, `sed`, `head`, `tail`, or inline `python` invoked through bash.".into());
    }
    if tool_names.iter().any(|name| name == "grep") {
        paragraphs.push("For text or filename search, use the `grep` tool (ripgrep-backed, respects .gitignore). Do NOT shell out to `grep` or `rg` through bash for the same purpose.".into());
    }
    if routing.mode == FileMutationMode::ApplyPatch {
        paragraphs.push("Do not re-read a file immediately after a successful `apply_patch`; the call returns failure directly if the patch did not apply.".into());
    }
    if paragraphs.is_empty() {
        String::new()
    } else {
        format!("## File operations\n\n{}", paragraphs.join("\n\n"))
    }
}
