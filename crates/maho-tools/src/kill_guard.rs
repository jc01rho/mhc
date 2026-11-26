//! Kill guard for the shell tool (omo v5.1.73): an agent-run `kill`, `pkill`, `killall` or
//! `pgrep | xargs kill` must not signal the running mhc engine, its ancestors, or another mhc/agentd
//! engine process on the same host.
//!
//! The judge reads the command text, so it stops the shapes agents actually type: a pid or a
//! process-group id, `$PPID`, a `pkill`/`killall` name or pattern, a `$(pgrep ...)` or
//! `pgrep ... | xargs kill` pipeline that names an engine, and any of those inside `sudo`, `env`,
//! `bash -c` or `eval`. It does not follow a pid read from a file or built by arithmetic.

use regex::RegexBuilder;

use crate::kernel_process_identity::read_kernel_process_identity;

/// Process names that stand for an engine. `pkill mhc` and `killall agentd` are refused even when no
/// such process is running right now, so the answer never depends on a race with the process table.
const ENGINE_NAMES: [&str; 2] = ["mhc", "agentd"];
const KILLERS: [&str; 3] = ["kill", "pkill", "killall"];
/// Commands that run their arguments as another command: the judge looks past them.
const WRAPPERS: [&str; 11] = [
    "sudo", "env", "command", "builtin", "exec", "nohup", "time", "timeout", "nice", "ionice",
    "setsid",
];
const SHELLS: [&str; 5] = ["bash", "sh", "zsh", "dash", "ksh"];
/// Deeper than any real `bash -c 'bash -c ...'`; bounds a hostile nesting.
const MAX_NESTING: usize = 3;
/// Bounds the ppid walk over a corrupted process table.
const MAX_ANCESTRY_DEPTH: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessEntry {
    pub pid: u32,
    pub name: String,
    pub args: String,
}

/// What the judge protects: this process, its ancestors, and every engine-looking process.
#[derive(Clone, Debug, Default)]
pub struct ProcessView {
    pub own_pid: u32,
    pub ancestors: Vec<u32>,
    pub processes: Vec<ProcessEntry>,
}

impl ProcessView {
    fn capture() -> Self {
        let own_pid = std::process::id();
        let mut ancestors = Vec::new();
        let mut pid = own_pid;
        for _ in 0..MAX_ANCESTRY_DEPTH {
            let Some(identity) = read_kernel_process_identity(pid) else {
                break;
            };
            if identity.ppid == 0 || ancestors.contains(&identity.ppid) {
                break;
            }
            ancestors.push(identity.ppid);
            pid = identity.ppid;
        }
        Self {
            own_pid,
            ancestors,
            processes: list_processes(),
        }
    }

    fn is_protected(&self, entry: &ProcessEntry) -> bool {
        entry.pid == self.own_pid || self.ancestors.contains(&entry.pid) || is_engine_process(entry)
    }

    fn protects_pid(&self, pid: u32) -> bool {
        pid == self.own_pid
            || self.ancestors.contains(&pid)
            || self
                .processes
                .iter()
                .any(|entry| entry.pid == pid && is_engine_process(entry))
    }

    /// Whether `pkill`/`killall` selection by `pattern` would reach a protected process.
    fn name_selects_protected(&self, pattern: &str, selection: &NameSelection) -> bool {
        let mut candidates: Vec<&str> = ENGINE_NAMES.to_vec();
        for entry in self
            .processes
            .iter()
            .filter(|entry| self.is_protected(entry))
        {
            candidates.push(&entry.name);
            if selection.full {
                candidates.push(&entry.args);
            }
        }
        if selection.exact {
            return candidates.iter().any(|candidate| {
                if selection.insensitive {
                    candidate.eq_ignore_ascii_case(pattern)
                } else {
                    *candidate == pattern
                }
            });
        }
        match RegexBuilder::new(pattern)
            .case_insensitive(selection.insensitive)
            .build()
        {
            Ok(regex) => candidates.iter().any(|candidate| regex.is_match(candidate)),
            Err(_) => candidates
                .iter()
                .any(|candidate| candidate.contains(pattern)),
        }
    }
}

struct NameSelection {
    /// `pkill -f`: the pattern is matched against the whole command line.
    full: bool,
    /// `killall` without `-r`: the name must equal the process name.
    exact: bool,
    insensitive: bool,
}

fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn is_engine_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name);
    ENGINE_NAMES
        .iter()
        .any(|engine| stem.eq_ignore_ascii_case(engine))
}

/// An engine runs as `mhc ...` or as a runtime launched on an engine script (`bun .../agentd.ts`).
fn is_engine_process(entry: &ProcessEntry) -> bool {
    is_engine_name(&entry.name)
        || entry
            .args
            .split_whitespace()
            .take(2)
            .any(|token| is_engine_name(basename(token)))
}

fn mentions_engine_name(word: &str) -> bool {
    let lowered = word.to_ascii_lowercase();
    ENGINE_NAMES.iter().any(|engine| lowered.contains(engine))
}

#[cfg(target_os = "linux")]
fn list_processes() -> Vec<ProcessEntry> {
    let Ok(directory) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    directory
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
            let name = std::fs::read_to_string(entry.path().join("comm"))
                .ok()?
                .trim()
                .to_owned();
            let args = std::fs::read(entry.path().join("cmdline"))
                .map(|raw| {
                    String::from_utf8_lossy(&raw)
                        .replace('\0', " ")
                        .trim()
                        .to_owned()
                })
                .unwrap_or_default();
            Some(ProcessEntry { pid, name, args })
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn list_processes() -> Vec<ProcessEntry> {
    let Ok(output) = std::process::Command::new("ps")
        .args(["-axo", "pid=,args="])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let (pid, args) = line.trim_start().split_once(char::is_whitespace)?;
            let args = args.trim();
            let first = args.split_whitespace().next()?;
            Some(ProcessEntry {
                pid: pid.parse().ok()?,
                name: basename(first).to_owned(),
                args: args.to_owned(),
            })
        })
        .collect()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn list_processes() -> Vec<ProcessEntry> {
    Vec::new()
}

/// The refusal text for a shell command that would signal the engine, or `None` to let it run.
pub fn refuse_engine_kill(command: &str) -> Option<String> {
    // Every shape the judge knows contains the word; the process table is read only past this gate.
    if !command.contains("kill") {
        return None;
    }
    judge_command(command, &ProcessView::capture(), 0)
}

pub fn judge_command(command: &str, view: &ProcessView, depth: usize) -> Option<String> {
    if depth > MAX_NESTING {
        return None;
    }
    statements(command)
        .iter()
        .find_map(|segments| judge_statement(segments, view, depth))
}

fn refusal(detail: &str) -> String {
    format!(
        "Refused: {detail}. The shell tool never signals the running mhc engine, its ancestors, or \
         other mhc/agentd processes on this host. Signal only the exact pids of processes you \
         started yourself (for example `$!`), or ask the owner."
    )
}

fn judge_statement(segments: &[Vec<String>], view: &ProcessView, depth: usize) -> Option<String> {
    // A kill whose targets come from a pipe or a substitution, and a pipeline stage that names an engine.
    let mut targets_from_pipeline = false;
    let mut names_engine = false;
    for words in segments {
        let Some((name, args)) = command_of(words) else {
            continue;
        };
        match name.as_str() {
            "kill" => {
                if let Some(reason) = judge_kill(args, view) {
                    return Some(reason);
                }
                if args
                    .iter()
                    .any(|arg| arg.contains('$') || arg.contains('`'))
                {
                    targets_from_pipeline = true;
                }
            }
            "pkill" | "killall" => {
                if let Some(reason) = judge_name_kill(&name, args, view) {
                    return Some(reason);
                }
            }
            "xargs" => {
                if args.iter().any(|arg| KILLERS.contains(&basename(arg))) {
                    targets_from_pipeline = true;
                }
            }
            "eval" => {
                if let Some(reason) = judge_command(&args.join(" "), view, depth + 1) {
                    return Some(reason);
                }
            }
            shell if SHELLS.contains(&shell) => {
                if let Some(script) = shell_script(args)
                    && let Some(reason) = judge_command(script, view, depth + 1)
                {
                    return Some(reason);
                }
            }
            _ => {}
        }
        if !KILLERS.contains(&name.as_str()) && args.iter().any(|arg| mentions_engine_name(arg)) {
            names_engine = true;
        }
    }
    (targets_from_pipeline && names_engine).then(|| {
        refusal("this pipeline selects an mhc/agentd process by name and feeds it to kill")
    })
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(key, _)| {
        key.chars()
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
            && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

fn is_duration(word: &str) -> bool {
    let digits = word.trim_end_matches(['s', 'm', 'h', 'd']);
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// The command a simple command runs, past assignments and wrappers like `sudo` or `timeout 5`.
fn command_of(words: &[String]) -> Option<(String, &[String])> {
    let mut index = 0;
    loop {
        let word = words.get(index)?;
        if is_assignment(word) {
            index += 1;
            continue;
        }
        let name = basename(word);
        if WRAPPERS.contains(&name) {
            index += 1;
            while words
                .get(index)
                .is_some_and(|next| next.starts_with('-') || is_duration(next))
            {
                index += 1;
            }
            continue;
        }
        return Some((name.to_owned(), &words[index + 1..]));
    }
}

/// The script of `bash -c SCRIPT`, including combined flags such as `-lc`.
fn shell_script(args: &[String]) -> Option<&str> {
    let flag = args
        .iter()
        .position(|arg| arg.starts_with('-') && !arg.starts_with("--") && arg.ends_with('c'))?;
    args.get(flag + 1).map(String::as_str)
}

fn judge_kill(args: &[String], view: &ProcessView) -> Option<String> {
    let mut targets: Vec<&str> = Vec::new();
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        index += 1;
        if arg == "--" {
            targets.extend(args[index..].iter().map(String::as_str));
            break;
        }
        if matches!(arg.as_str(), "-l" | "-L" | "-t") {
            return None;
        }
        if matches!(arg.as_str(), "-s" | "-n") {
            index += 1;
            continue;
        }
        let Some(rest) = arg.strip_prefix('-').filter(|rest| !rest.is_empty()) else {
            targets.push(arg);
            continue;
        };
        // The first dash word is the signal; a later `-N` is a process group id.
        if index == 1 || rest.chars().any(|c| !c.is_ascii_digit()) {
            continue;
        }
        targets.push(arg);
    }
    targets
        .into_iter()
        .find_map(|target| judge_kill_target(target, view))
}

fn judge_kill_target(target: &str, view: &ProcessView) -> Option<String> {
    if target == "$PPID" || target == "${PPID}" {
        return Some(refusal(
            "`$PPID` is the mhc engine that runs this shell, and the command would signal it",
        ));
    }
    let number: i64 = target.parse().ok()?;
    if number == -1 {
        return Some(refusal(
            "`kill -1` signals every process the user can reach, the mhc engine included",
        ));
    }
    let pid = u32::try_from(number.unsigned_abs()).ok()?;
    (pid != 0 && view.protects_pid(pid)).then(|| {
        let which = if pid == view.own_pid {
            "the running mhc engine"
        } else if view.ancestors.contains(&pid) {
            "an ancestor of the running mhc engine"
        } else {
            "an mhc/agentd engine process"
        };
        refusal(&format!("pid {pid} is {which}"))
    })
}

fn judge_name_kill(name: &str, args: &[String], view: &ProcessView) -> Option<String> {
    let pkill = name == "pkill";
    let mut selection = NameSelection {
        full: false,
        exact: !pkill,
        insensitive: false,
    };
    let mut patterns: Vec<&str> = Vec::new();
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        index += 1;
        if let Some(long) = arg.strip_prefix("--") {
            match long {
                "full" => selection.full = true,
                "ignore-case" => selection.insensitive = true,
                "regexp" => selection.exact = false,
                _ => {}
            }
            continue;
        }
        let Some(flags) = arg.strip_prefix('-').filter(|flags| !flags.is_empty()) else {
            patterns.push(arg);
            continue;
        };
        // `-9` and `-KILL` are signals; anything else is a cluster of option letters.
        if flags.chars().all(|c| c.is_ascii_digit())
            || (flags.len() > 1 && flags.chars().all(|c| c.is_ascii_uppercase()))
        {
            continue;
        }
        for flag in flags.chars() {
            match flag {
                'f' => selection.full = true,
                'i' => selection.insensitive = true,
                'I' if !pkill => selection.insensitive = true,
                'r' if !pkill => selection.exact = false,
                _ => {}
            }
        }
        if flags.ends_with(['u', 'U', 'g', 'G', 'P', 's', 't', 'F']) {
            index += 1;
        }
    }
    let selected = if pkill {
        &patterns[..patterns.len().min(1)]
    } else {
        &patterns[..]
    };
    selected
        .iter()
        .find(|pattern| view.name_selects_protected(pattern, &selection))
        .map(|pattern| {
            refusal(&format!(
                "`{name} {pattern}` selects an mhc/agentd process by name"
            ))
        })
}

#[derive(Default)]
struct Parser {
    statements: Vec<Vec<Vec<String>>>,
    segments: Vec<Vec<String>>,
    words: Vec<String>,
    word: String,
    in_word: bool,
}

impl Parser {
    fn end_word(&mut self) {
        if self.in_word {
            self.words.push(std::mem::take(&mut self.word));
            self.in_word = false;
        }
    }

    fn end_segment(&mut self) {
        self.end_word();
        if !self.words.is_empty() {
            self.segments.push(std::mem::take(&mut self.words));
        }
    }

    fn end_statement(&mut self) {
        self.end_segment();
        if !self.segments.is_empty() {
            self.statements.push(std::mem::take(&mut self.segments));
        }
    }
}

/// Splits shell text into statements (`;`, `&&`, `||`, `&`, newline) of pipeline segments (`|`, `$(`,
/// backtick, parentheses) of words. A `$(` or backtick leaves a marker word so the command that
/// receives the substitution is known to take its targets from elsewhere.
fn statements(command: &str) -> Vec<Vec<Vec<String>>> {
    let mut parser = Parser::default();
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(open) = quote {
            if c == open {
                quote = None;
            } else if c == '\\' && open == '"' {
                if let Some(next) = chars.next() {
                    parser.word.push(next);
                }
            } else {
                parser.word.push(c);
            }
            continue;
        }
        match c {
            '\'' | '"' => {
                quote = Some(c);
                parser.in_word = true;
            }
            '\\' => {
                if let Some(next) = chars.next()
                    && next != '\n'
                {
                    parser.word.push(next);
                    parser.in_word = true;
                }
            }
            '#' if !parser.in_word => {
                while chars.peek().is_some_and(|next| *next != '\n') {
                    chars.next();
                }
            }
            ' ' | '\t' | '\r' => parser.end_word(),
            '\n' | ';' => parser.end_statement(),
            '&' if parser.word.ends_with(['<', '>']) || chars.peek() == Some(&'>') => {
                parser.word.push(c);
                parser.in_word = true;
            }
            '&' | '|' => {
                let doubled = chars.peek() == Some(&c);
                if doubled {
                    chars.next();
                }
                if c == '|' && !doubled {
                    parser.end_segment();
                } else {
                    parser.end_statement();
                }
            }
            '(' => {
                if parser.in_word && parser.word.ends_with('$') {
                    parser.word.push('(');
                }
                parser.end_segment();
            }
            ')' => parser.end_segment(),
            '`' => {
                parser.end_word();
                parser.words.push("`".to_owned());
                parser.end_segment();
            }
            _ => {
                parser.word.push(c);
                parser.in_word = true;
            }
        }
    }
    parser.end_statement();
    parser.statements
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(pid: u32, name: &str, args: &str) -> ProcessEntry {
        ProcessEntry {
            pid,
            name: name.to_owned(),
            args: args.to_owned(),
        }
    }

    fn view() -> ProcessView {
        ProcessView {
            own_pid: 100,
            ancestors: vec![50, 1],
            processes: vec![
                entry(100, "mhc", "/home/u/.local/share/mhc/mhc --mode rpc"),
                entry(200, "bun", "bun /srv/omomeow/agentd.ts --serve"),
                entry(300, "node", "node server.js"),
            ],
        }
    }

    fn refused(command: &str) -> bool {
        judge_command(command, &view(), 0).is_some()
    }

    #[test]
    fn refuses_signals_to_the_engine_its_ancestors_and_other_engines_by_pid() {
        for command in [
            "kill 100",
            "kill -9 100",
            "kill -KILL 50",
            "kill -s TERM 1",
            "/bin/kill 200",
            "sudo kill -9 200",
            "timeout 5 kill 100",
            "FOO=1 kill 100",
            "kill -9 -100",
            "kill -9 -1",
            "kill $PPID",
            "kill -9 ${PPID}",
            "kill 300 100",
            "kill -- 100",
        ] {
            assert!(refused(command), "{command}");
        }
    }

    #[test]
    fn refuses_name_selection_that_reaches_an_engine() {
        for command in [
            "pkill mhc",
            "pkill -9 mhc",
            "pkill -f agentd",
            "pkill -f 'share/mhc/mhc'",
            "pkill -i MHC",
            "pkill m",
            "killall mhc",
            "killall -9 agentd",
            "bash -c 'pkill agentd'",
            "sh -lc \"kill 100\"",
            "eval 'kill 100'",
        ] {
            assert!(refused(command), "{command}");
        }
    }

    #[test]
    fn refuses_a_pipeline_that_feeds_an_engine_name_to_kill() {
        for command in [
            "pgrep -f mhc | xargs kill",
            "pgrep agentd | xargs -r kill -9",
            "kill -9 $(pgrep mhc)",
            "kill $(pidof agentd)",
            "kill -9 `pgrep -f mhc`",
            "ps aux 2>&1 | grep mhc | awk '{print $2}' | xargs kill -9",
        ] {
            assert!(refused(command), "{command}");
        }
    }

    #[test]
    fn lets_ordinary_process_management_through() {
        for command in [
            "kill 300",
            "kill -9 300 301",
            "kill -l",
            "kill -9 $!",
            "kill $(lsof -ti :3000)",
            "pkill node",
            "pkill -f 'vite dev'",
            "killall python",
            "pgrep -f mhc",
            "echo mhc; kill 300",
            "pgrep mhc && kill 300",
            "ls | xargs echo kill",
            "echo kill 100",
            "git log --oneline",
            "kill 0",
        ] {
            assert!(!refused(command), "{command}");
        }
    }

    #[test]
    fn a_command_without_the_word_kill_never_reads_the_process_table() {
        assert_eq!(refuse_engine_kill("echo hello"), None);
    }
}
