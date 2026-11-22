use fancy_regex::Regex;
use std::sync::OnceLock;

pub const IMAGE_MIME_TYPES: [(&str, &str); 9] = [
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("avif", "image/avif"),
    ("svg", "image/svg+xml"),
    ("bmp", "image/bmp"),
    ("ico", "image/x-icon"),
];

const ABSOLUTE_PATH: &str = r"(?:/(?!/)|[a-z]:[\\/])";
const IMAGE_EXTENSIONS: &str = "png|jpg|jpeg|gif|webp|avif|svg|bmp|ico";
const RAW_TEXT_ELEMENTS: [&str; 10] = [
    "script",
    "style",
    "textarea",
    "title",
    "xmp",
    "noscript",
    "iframe",
    "noembed",
    "noframes",
    "plaintext",
];

struct Patterns {
    image_path: Regex,
    css_url: Regex,
    script_path: Regex,
}

fn patterns() -> &'static Patterns {
    static PATTERNS: OnceLock<Patterns> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        let image_path =
            format!(r"(?i)^{ABSOLUTE_PATH}[^\r\n]{{0,2048}}\.(?:{IMAGE_EXTENSIONS})$");
        let unquoted_url = format!(
            r#"url\(\s*({ABSOLUTE_PATH}[^\s"'\x60()]{{0,2048}}?\.(?:{IMAGE_EXTENSIONS}))\s*\)"#
        );
        let css_url = format!(
            r#"(?i)url\(\s*(["'])({ABSOLUTE_PATH}(?:(?!\1)[^\r\n]){{0,2048}}?\.(?:{IMAGE_EXTENSIONS}))\1\s*\)|{unquoted_url}"#
        );
        let script_path = format!(
            r#"(?i)(["'\x60])({ABSOLUTE_PATH}(?:(?!\1)[^\r\n]){{0,2048}}?\.(?:{IMAGE_EXTENSIONS}))\1|{unquoted_url}"#
        );
        let build = |source: &str| {
            Regex::new(source)
                .unwrap_or_else(|error| panic!("invalid image reference pattern: {error}"))
        };
        Patterns {
            image_path: build(&image_path),
            css_url: build(&css_url),
            script_path: build(&script_path),
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageReference {
    pub start: usize,
    pub end: usize,
    pub path: String,
}

struct Attribute {
    name: String,
    start: usize,
    end: usize,
}

fn is_space(byte: u8) -> bool {
    matches!(byte, b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

fn attribute_name_end(byte: u8) -> bool {
    is_space(byte) || matches!(byte, b'/' | b'>' | b'=')
}

fn unquoted_value_end(byte: u8) -> bool {
    is_space(byte) || byte == b'>'
}

fn skip_space(html: &[u8], from: usize) -> usize {
    let mut at = from;
    while at < html.len() && is_space(html[at]) {
        at += 1;
    }
    at
}

fn end_after(html: &[u8], token: &[u8], from: usize) -> usize {
    html.get(from..)
        .and_then(|rest| {
            rest.windows(token.len())
                .position(|window| window == token)
                .map(|found| from + found + token.len())
        })
        .unwrap_or(html.len())
}

fn comment_end(html: &[u8], from: usize) -> usize {
    if html.get(from) == Some(&b'>') {
        return from + 1;
    }
    if html.get(from..).is_some_and(|rest| rest.starts_with(b"->")) {
        return from + 2;
    }
    end_after(html, b"-->", from)
}

fn read_tag(html: &[u8], from: usize) -> Option<(Vec<Attribute>, usize)> {
    let mut attributes = Vec::new();
    let mut names = Vec::<String>::new();
    let mut add = |attribute: Attribute| {
        if !names.contains(&attribute.name) {
            names.push(attribute.name.clone());
            attributes.push(attribute);
        }
    };
    let mut at = from;
    while at < html.len() {
        let byte = html[at];
        if byte == b'>' {
            return Some((attributes, at + 1));
        }
        if byte == b'/' || is_space(byte) {
            at += 1;
            continue;
        }
        let name_start = at;
        at += 1;
        while at < html.len() && !attribute_name_end(html[at]) {
            at += 1;
        }
        let name = String::from_utf8_lossy(&html[name_start..at]).to_ascii_lowercase();
        let equals = skip_space(html, at);
        if html.get(equals) != Some(&b'=') {
            add(Attribute {
                name,
                start: at,
                end: at,
            });
            at = equals;
            continue;
        }
        let value = skip_space(html, equals + 1);
        let quote = html.get(value).copied();
        if quote == Some(b'"') || quote == Some(b'\'') {
            let quote = quote.unwrap_or(b'"');
            let close = html
                .get(value + 1..)
                .and_then(|rest| rest.iter().position(|candidate| *candidate == quote))
                .map(|found| value + 1 + found)?;
            add(Attribute {
                name,
                start: value + 1,
                end: close,
            });
            at = close + 1;
            continue;
        }
        at = value;
        while at < html.len() && !unquoted_value_end(html[at]) {
            at += 1;
        }
        add(Attribute {
            name,
            start: value,
            end: at,
        });
    }
    None
}

fn path_at(html: &str, start: usize, end: usize) -> Vec<ImageReference> {
    let bytes = html.as_bytes();
    let mut from = start;
    let mut to = end;
    while from < to && is_space(bytes[from]) {
        from += 1;
    }
    while to > from && is_space(bytes[to - 1]) {
        to -= 1;
    }
    let path = &html[from..to];
    if patterns().image_path.is_match(path).unwrap_or(false) {
        vec![ImageReference {
            start: from,
            end: to,
            path: path.to_owned(),
        }]
    } else {
        Vec::new()
    }
}

fn srcset_references(html: &str, start: usize, end: usize) -> Vec<ImageReference> {
    let bytes = html.as_bytes();
    let mut references = Vec::new();
    let mut at = start;
    while at < end {
        while at < end && (is_space(bytes[at]) || bytes[at] == b',') {
            at += 1;
        }
        let mut url_end = at;
        while url_end < end && !is_space(bytes[url_end]) {
            url_end += 1;
        }
        let mut trimmed = url_end;
        while trimmed > at && bytes[trimmed - 1] == b',' {
            trimmed -= 1;
        }
        references.extend(path_at(html, at, trimmed));
        let comma = if trimmed < url_end {
            Some(url_end - 1)
        } else {
            bytes[url_end..end]
                .iter()
                .position(|byte| *byte == b',')
                .map(|found| url_end + found)
        };
        at = comma.map_or(end, |comma| comma + 1);
    }
    references
}

fn matches(pattern: &Regex, html: &str, start: usize, end: usize) -> Vec<ImageReference> {
    let body = &html[start..end];
    pattern
        .captures_iter(body)
        .flatten()
        .filter_map(|captures| {
            let span = captures.get(2).or_else(|| captures.get(3))?;
            Some(ImageReference {
                start: start + span.start(),
                end: start + span.end(),
                path: span.as_str().to_owned(),
            })
        })
        .collect()
}

fn attribute_references(html: &str, attribute: &Attribute) -> Vec<ImageReference> {
    match attribute.name.as_str() {
        "src" | "poster" | "href" | "xlink:href" | "data" | "background" => {
            path_at(html, attribute.start, attribute.end)
        }
        "srcset" => srcset_references(html, attribute.start, attribute.end),
        "style" => matches(&patterns().css_url, html, attribute.start, attribute.end),
        name if name.starts_with("on") => matches(
            &patterns().script_path,
            html,
            attribute.start,
            attribute.end,
        ),
        _ => Vec::new(),
    }
}

fn raw_text_end(lower: &[u8], element: &str, from: usize) -> usize {
    let token = format!("</{element}");
    let token = token.as_bytes();
    let mut search = from;
    while let Some(found) = lower
        .get(search..)
        .and_then(|rest| rest.windows(token.len()).position(|window| window == token))
        .map(|found| search + found)
    {
        if lower
            .get(found + token.len())
            .copied()
            .is_some_and(|byte| is_space(byte) || matches!(byte, b'/' | b'>'))
        {
            return found;
        }
        search = found + 1;
    }
    lower.len()
}

pub fn find_local_images(html: &str) -> Vec<ImageReference> {
    let bytes = html.as_bytes();
    let lower = html.to_ascii_lowercase();
    let lower_bytes = lower.as_bytes();
    let mut references = Vec::new();
    let mut foreign_depth = 0usize;
    let mut at = 0usize;
    while at < bytes.len() {
        let Some(open_rel) = bytes[at..].iter().position(|byte| *byte == b'<') else {
            break;
        };
        let open = at + open_rel;
        let next = bytes.get(open + 1).copied();
        if bytes[open..].starts_with(b"<!--") {
            at = comment_end(bytes, open + 4);
            continue;
        }
        if foreign_depth > 0 && bytes[open..].starts_with(b"<![CDATA[") {
            at = end_after(bytes, b"]]>", open + 9);
            continue;
        }
        if matches!(next, Some(b'!') | Some(b'?'))
            || (next == Some(b'/') && !bytes.get(open + 2).is_some_and(u8::is_ascii_alphabetic))
        {
            at = end_after(bytes, b">", open + 2);
            continue;
        }
        let closing = next == Some(b'/');
        let element_start = open + 1 + usize::from(closing);
        if !bytes
            .get(element_start)
            .is_some_and(u8::is_ascii_alphabetic)
        {
            at = open + 1;
            continue;
        }
        let element_end = bytes[element_start..]
            .iter()
            .position(|byte| is_space(*byte) || matches!(*byte, b'/' | b'>'))
            .map_or(bytes.len(), |found| element_start + found);
        let Some((attributes, end)) = read_tag(bytes, element_end) else {
            break;
        };
        at = end;
        let element = &lower[element_start..element_end];
        if element == "svg" || element == "math" {
            if closing {
                foreign_depth = foreign_depth.saturating_sub(1);
            } else if bytes.get(end.wrapping_sub(2)) != Some(&b'/') {
                foreign_depth += 1;
            }
        }
        if closing {
            continue;
        }
        for attribute in &attributes {
            references.extend(attribute_references(html, attribute));
        }
        if !RAW_TEXT_ELEMENTS.contains(&element) {
            continue;
        }
        let body_end = if element == "plaintext" {
            bytes.len()
        } else {
            raw_text_end(lower_bytes, element, at)
        };
        match element {
            "script" => references.extend(matches(&patterns().script_path, html, at, body_end)),
            "style" => references.extend(matches(&patterns().css_url, html, at, body_end)),
            _ => {}
        }
        at = body_end;
    }
    references
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(html: &str) -> Vec<String> {
        find_local_images(html)
            .into_iter()
            .map(|reference| reference.path)
            .collect()
    }

    #[test]
    fn a_loading_context_loads_and_text_only_mentions_stay_written() {
        assert_eq!(
            paths(
                r#"<img src="/a/b.png"><div style="background:url('/c/d.svg')"></div><a title="/e/f.png">x</a><p>/g/h.png</p>"#
            ),
            vec!["/a/b.png", "/c/d.svg"]
        );
        assert_eq!(
            paths(r#"<script>const src = "/i/j.png";</script>"#),
            vec!["/i/j.png"]
        );
        assert_eq!(
            paths("<style>p{background:url(/k/l.png)}</style>"),
            vec!["/k/l.png"]
        );
        assert!(paths("<textarea>/m/n.png</textarea>").is_empty());
    }

    #[test]
    fn a_comment_and_a_cdata_section_do_not_load_paths() {
        assert!(paths("<!-- /a/b.png -->").is_empty());
        assert!(paths(r#"<![CDATA[<img src="/a/b.png">]]>"#).is_empty());
        assert_eq!(paths(r#"<!--><img src="/a/b.png">"#), vec!["/a/b.png"]);
    }

    #[test]
    fn duplicated_attributes_and_srcset_candidates_are_read_like_a_browser() {
        let first = find_local_images(r#"<img src="/a/b.png" src="/c/d.png">"#);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].path, "/a/b.png");
        let srcset = find_local_images(r#"<img srcset="/a/b.png 1x, /c/d.png 2x">"#);
        assert_eq!(
            srcset
                .iter()
                .map(|reference| reference.path.as_str())
                .collect::<Vec<_>>(),
            vec!["/a/b.png", "/c/d.png"]
        );
    }
}
