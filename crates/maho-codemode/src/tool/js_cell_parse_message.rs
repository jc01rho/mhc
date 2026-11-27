use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

const BUN_PARSE_LOCATOR: &str = r#"import { readFileSync } from "node:fs";
const code = readFileSync(0, "utf8");
const transpiler = new Bun.Transpiler({ loader: "js" });
const pick = (error) => {
	const list = Array.isArray(error?.errors) ? error.errors : [error];
	for (const item of list) {
		if (item?.message === "Top-level return cannot be used inside an ECMAScript module") continue;
		const pos = item?.position;
		if (pos && typeof pos.line === "number" && typeof pos.column === "number" && pos.line >= 1 && pos.column >= 1) {
			return { line: pos.line, column: pos.column - 1 };
		}
	}
	return null;
};
let loc = null;
try {
	transpiler.transformSync(code);
} catch (error) {
	loc = pick(error);
}
if (!loc) {
	try {
		transpiler.transformSync(`(async () => {\n${code}\n})()`);
	} catch (error) {
		const wrapped = pick(error);
		if (wrapped && wrapped.line >= 2) loc = { line: wrapped.line - 1, column: wrapped.column };
	}
}
if (loc) process.stdout.write(`${loc.line}:${loc.column}`);
"#;

static QUOTED_HEREDOC_RE: OnceLock<regex::Regex> = OnceLock::new();

fn locate_js_parse_error(code: &str) -> Option<(usize, usize)> {
    let mut child = Command::new("bun")
        .args(["-e", BUN_PARSE_LOCATOR])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(code.as_bytes());
    }
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = std::str::from_utf8(&output.stdout).ok()?.trim();
    let (line, column) = text.split_once(':')?;
    Some((line.parse().ok()?, column.parse().ok()?))
}

fn split_js_lines(code: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = code.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        match ch {
            '\r' => {
                lines.push(&code[start..index]);
                if chars.peek().is_some_and(|(_, next)| *next == '\n') {
                    chars.next();
                }
                start = chars.peek().map_or(code.len(), |(next, _)| *next);
            }
            '\n' | '\u{2028}' | '\u{2029}' => {
                lines.push(&code[start..index]);
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }
    lines.push(&code[start..]);
    lines
}

pub fn js_cell_parse_message(code: &str, message: &str) -> String {
    let Some((line_number, column)) = locate_js_parse_error(code) else {
        return message.to_owned();
    };
    let lines = split_js_lines(code);
    let line = line_number
        .checked_sub(1)
        .and_then(|index| lines.get(index).copied())
        .unwrap_or("");
    let units: Vec<u16> = line.encode_utf16().collect();
    let start = column.saturating_sub(60);
    let end = (column + 60).min(units.len());
    let excerpt_units = if start < end { &units[start..end] } else { &[] };
    let caret_end = column.saturating_sub(start).min(excerpt_units.len());
    let excerpt = String::from_utf16_lossy(excerpt_units);
    let caret: String = String::from_utf16_lossy(&excerpt_units[..caret_end])
        .chars()
        .map(|ch| if ch == '\t' { '\t' } else { ' ' })
        .collect();
    let heredoc_re = QUOTED_HEREDOC_RE.get_or_init(|| {
        regex::Regex::new(
            r#"<<-?[ \t]*(?:'[A-Za-z_][A-Za-z0-9_]*'|"[A-Za-z_][A-Za-z0-9_]*")[ \t]*(?:\r?\n|$)"#,
        )
        .expect("quoted heredoc regex")
    });
    let hint = if heredoc_re.is_match(code) && code.contains('`') {
        "\nFor a shell heredoc in a JS template literal, escape literal backticks and ${ with a backslash, or use ordinary quoted JS strings."
    } else {
        ""
    };
    format!(
        "{message}\nJS cell line {line_number}, column {}:\n{excerpt}\n{caret}^{hint}",
        column + 1
    )
}
