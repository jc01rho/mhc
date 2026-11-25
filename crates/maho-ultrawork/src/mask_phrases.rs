//! Masking of relayed headers and prohibitive phrasing.

use crate::mask::{Span, lines};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    From,
    To,
}

fn header_side(line: &str) -> Option<Side> {
    let lower = line.trim_start().to_ascii_lowercase();
    if lower.starts_with("from:") || lower.starts_with("sender:") {
        Some(Side::From)
    } else if lower.starts_with("to:") || lower.starts_with("recipient:") {
        Some(Side::To)
    } else {
        None
    }
}

fn arrow_header_end(line: &str) -> Option<usize> {
    let colon = line.find(':')?;
    let prefix = &line[..colon];
    let arrowed = prefix.contains("->") || prefix.contains('\u{2192}');
    let plain = prefix
        .chars()
        .all(|c| c.is_alphanumeric() || " .@_-[]>\u{2192}".contains(c));
    (arrowed && plain && prefix.len() <= 100).then_some(colon + 1)
}

/// A relayed message opening with an all-caps sender tag (`[REPORT] ...`) is someone else's text.
fn relay_tag(text: &str) -> bool {
    let Some(rest) = text.trim_start().strip_prefix('[') else {
        return false;
    };
    let name = rest
        .bytes()
        .take_while(|&b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        .count();
    rest.as_bytes().first().is_some_and(u8::is_ascii_uppercase)
        && name >= 3
        && rest.as_bytes().get(name) == Some(&b']')
}

pub(crate) fn header_spans(text: &str) -> Vec<Span> {
    if relay_tag(text) {
        return vec![(0, text.len())];
    }
    let all = lines(text);
    let mut spans = Vec::new();
    for (n, &(start, end)) in all.iter().enumerate() {
        if let Some(offset) = arrow_header_end(&text[start..end]) {
            spans.push((start, start + offset));
        }
        let Some(side) = header_side(&text[start..end]) else {
            continue;
        };
        let partner = all
            .iter()
            .enumerate()
            .skip(n + 1)
            .take(2)
            .find(|(_, s)| header_side(&text[s.0..s.1]).is_some_and(|other| other != side));
        if let Some((_, &(_, partner_end))) = partner {
            spans.push((start, partner_end));
        }
    }
    spans
}

pub(crate) const NEGATIONS: [&str; 12] = [
    "never", "not", "dont", "don't", "doesnt", "doesn't", "wont", "won't", "cant", "can't",
    "cannot", "avoid",
];
pub(crate) const PROHIBITIONS: [&str; 9] = [
    "avoid", "avoiding", "skip", "skipping", "without", "no", "never", "dont", "don't",
];
const INVOCATIONS: [&str; 19] = [
    "load",
    "loading",
    "arm",
    "arming",
    "use",
    "using",
    "enable",
    "enabling",
    "run",
    "running",
    "trigger",
    "triggering",
    "invoke",
    "activate",
    "apply",
    "start",
    "launch",
    "engage",
    "turn",
];
const CONTRASTS: [&str; 6] = ["but", "however", "instead", "though", "although", "yet"];

fn words(text: &str) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    let mut current: Option<usize> = None;
    for (index, ch) in text.char_indices() {
        let in_word = ch.is_alphanumeric() || ch == '\'' || ch == '\u{2019}';
        match (in_word, current) {
            (true, None) => current = Some(index),
            (false, Some(start)) => {
                out.push((start, index, normalise(&text[start..index])));
                current = None;
            }
            _ => {}
        }
    }
    if let Some(start) = current {
        out.push((start, text.len(), normalise(&text[start..])));
    }
    out
}

fn normalise(word: &str) -> String {
    word.to_lowercase().replace('\u{2019}', "'")
}

fn clause_end(text: &str, from: usize, tokens: &[(usize, usize, String)]) -> usize {
    let bytes = text.as_bytes();
    let contrast = tokens
        .iter()
        .find(|(start, _, word)| *start >= from && CONTRASTS.contains(&word.as_str()))
        .map_or(bytes.len(), |(start, _, _)| *start);
    let sentence = (from..bytes.len().min(contrast)).find(|&index| {
        matches!(bytes[index], b';' | b'\n')
            || (matches!(bytes[index], b'.' | b'!' | b'?')
                && bytes.get(index + 1).is_none_or(u8::is_ascii_whitespace))
    });
    sentence.unwrap_or(contrast)
}

pub(crate) fn prohibitive_spans(text: &str) -> Vec<Span> {
    let tokens = words(text);
    let mut spans = Vec::new();
    for (n, (start, negation_end, word)) in tokens.iter().enumerate() {
        if !NEGATIONS.contains(&word.as_str()) {
            continue;
        }
        let verb = tokens
            .iter()
            .skip(n + 1)
            .take(3)
            .find(|(_, _, other)| INVOCATIONS.contains(&other.as_str()));
        if let Some((verb_start, verb_end, _)) = verb
            && !text[*negation_end..*verb_start].contains([',', ';', '.', '!', '?', '\n'])
        {
            spans.push((*start, clause_end(text, *verb_end, &tokens)));
        }
    }
    spans.extend(prohibited_keyword_spans(text, &tokens));
    spans
}

fn only_spaces(slice: &str) -> bool {
    !slice.is_empty() && slice.chars().all(char::is_whitespace)
}

fn comma_between(slice: &str) -> bool {
    slice.trim() == ","
}

fn optional_comma_spaces(slice: &str) -> bool {
    let trimmed = slice.trim();
    (trimmed.is_empty() && !slice.is_empty()) || trimmed == ","
}

fn slash_between(slice: &str) -> bool {
    let trimmed = slice.trim();
    trimmed == "/"
        || trimmed
            .strip_prefix(',')
            .is_some_and(|rest| rest.trim() == "/")
}

fn prohibited_item_at(
    text: &str,
    tokens: &[(usize, usize, String)],
    idx: usize,
) -> Option<(usize, usize)> {
    let (_, mut end, ref first) = *tokens.get(idx)?;
    let mut cur = idx + 1;
    if first == "mass" {
        let &(next_start, next_end, ref second) = tokens.get(cur)?;
        let gap = &text[end..next_start];
        if !matches!(second.as_str(), "ulw" | "ultrawork")
            || gap.is_empty()
            || !gap
                .chars()
                .all(|c| c == '-' || c == '_' || c.is_whitespace())
        {
            return None;
        }
        end = next_end;
        cur += 1;
    } else if !matches!(first.as_str(), "ulw" | "ultrawork" | "mulw" | "meth") {
        return None;
    }
    if let Some((_, next_end, _)) = tokens.get(cur).filter(|&&(next_start, _, ref suffix)| {
        matches!(text.get(end..next_start), Some("-" | "_"))
            && matches!(
                suffix.as_str(),
                "loop" | "plan" | "research" | "execute" | "mass"
            )
    }) {
        end = *next_end;
        cur += 1;
    }
    Some((cur, end))
}

fn prohibited_conjunction_tail(
    text: &str,
    tokens: &[(usize, usize, String)],
    prev_end: usize,
    cur: usize,
) -> Option<usize> {
    let &(next_start, next_end, ref word) = tokens.get(cur)?;
    let gap = &text[prev_end..next_start];
    if matches!(word.as_str(), "or" | "and" | "nor") && optional_comma_spaces(gap) {
        let &(item_start, _, _) = tokens.get(cur + 1)?;
        if !only_spaces(&text[next_end..item_start]) {
            return None;
        }
        return prohibited_item_at(text, tokens, cur + 1).map(|(_, end)| end);
    }
    if slash_between(gap) {
        return prohibited_item_at(text, tokens, cur).map(|(_, end)| end);
    }
    None
}

fn prohibited_keyword_spans(text: &str, tokens: &[(usize, usize, String)]) -> Vec<Span> {
    let mut spans = Vec::new();
    for (i, &(start, end, ref word)) in tokens.iter().enumerate() {
        let (after_prohib, mut next_idx) = if word == "do" {
            let Some(&(_, not_end, _)) = tokens.get(i + 1).filter(|&&(not_start, _, ref next)| {
                next == "not" && only_spaces(&text[end..not_start])
            }) else {
                continue;
            };
            (not_end, i + 2)
        } else if PROHIBITIONS.contains(&word.as_str()) {
            (end, i + 1)
        } else {
            continue;
        };

        let Some(&(first_token_start, first_token_end, ref first_word)) = tokens.get(next_idx)
        else {
            continue;
        };
        if !only_spaces(&text[after_prohib..first_token_start]) {
            continue;
        }

        if matches!(first_word.as_str(), "using" | "the")
            && tokens
                .get(next_idx + 1)
                .is_some_and(|&(after_start, _, _)| {
                    only_spaces(&text[first_token_end..after_start])
                })
        {
            next_idx += 1;
        } else if first_word == "use"
            && tokens
                .get(next_idx + 1)
                .filter(|&&(of_start, _, ref w)| {
                    w == "of" && only_spaces(&text[first_token_end..of_start])
                })
                .is_some_and(|&(_, of_end, _)| {
                    tokens
                        .get(next_idx + 2)
                        .is_some_and(|&(after_start, _, _)| only_spaces(&text[of_end..after_start]))
                })
        {
            next_idx += 2;
        }

        let Some((mut cur, first_end)) = prohibited_item_at(text, tokens, next_idx) else {
            continue;
        };
        let mut prev_end = first_end;
        while let Some((after_item, item_end)) = tokens
            .get(cur)
            .filter(|&&(item_start, _, _)| comma_between(&text[prev_end..item_start]))
            .and_then(|_| prohibited_item_at(text, tokens, cur))
        {
            prev_end = item_end;
            cur = after_item;
        }
        let span_end =
            prohibited_conjunction_tail(text, tokens, prev_end, cur).unwrap_or(first_end);
        spans.push((start, span_end));
    }
    spans
}
