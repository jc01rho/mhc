//! Port of `device-service-core/src/release/version.ts`: semantic version parsing and comparison.

use std::cmp::Ordering;

use crate::local_api::LocalApiError;

fn is_ident_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-'
}

fn is_digits(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

struct ParsedSemver<'a> {
    major: u64,
    minor: u64,
    patch: u64,
    pre: Option<&'a str>,
}

fn parse_semver(value: &str) -> Option<ParsedSemver<'_>> {
    let (head, build) = match value.split_once('+') {
        Some((head, build)) => (head, Some(build)),
        None => (value, None),
    };
    if let Some(build) = build
        && (build.is_empty() || !build.bytes().all(is_ident_char))
    {
        return None;
    }
    let (core, pre) = match head.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (head, None),
    };
    if let Some(pre) = pre
        && (pre.is_empty() || !pre.bytes().all(is_ident_char))
    {
        return None;
    }
    let mut parts = core.split('.');
    let major = parts.next().filter(|p| is_digits(p))?.parse::<u64>().ok()?;
    let minor = parts.next().filter(|p| is_digits(p))?.parse::<u64>().ok()?;
    let patch = parts.next().filter(|p| is_digits(p))?.parse::<u64>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(ParsedSemver {
        major,
        minor,
        patch,
        pre,
    })
}

pub fn is_semver(value: &str) -> bool {
    parse_semver(value).is_some()
}

fn compare_identifier(a: &str, b: &str) -> Ordering {
    match (is_digits(a), is_digits(b)) {
        (true, true) => {
            let left = a.trim_start_matches('0');
            let right = b.trim_start_matches('0');
            left.len().cmp(&right.len()).then_with(|| left.cmp(right))
        }
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => a.cmp(b),
    }
}

/// Semantic-version precedence of two valid versions. Build metadata is ignored.
pub fn compare_versions(a: &str, b: &str) -> Result<Ordering, LocalApiError> {
    let left = parse_semver(a)
        .ok_or_else(|| LocalApiError::protocol_violation(format!("not a semantic version: {a}")))?;
    let right = parse_semver(b)
        .ok_or_else(|| LocalApiError::protocol_violation(format!("not a semantic version: {b}")))?;

    let core_cmp =
        (left.major, left.minor, left.patch).cmp(&(right.major, right.minor, right.patch));
    if core_cmp != Ordering::Equal {
        return Ok(core_cmp);
    }

    match (left.pre, right.pre) {
        (None, None) => Ok(Ordering::Equal),
        (None, Some(_)) => Ok(Ordering::Greater),
        (Some(_), None) => Ok(Ordering::Less),
        (Some(pre_left), Some(pre_right)) => {
            let mut left_iter = pre_left.split('.');
            let mut right_iter = pre_right.split('.');
            loop {
                match (left_iter.next(), right_iter.next()) {
                    (None, None) => return Ok(Ordering::Equal),
                    (None, Some(_)) => return Ok(Ordering::Less),
                    (Some(_), None) => return Ok(Ordering::Greater),
                    (Some(x), Some(y)) => {
                        let order = compare_identifier(x, y);
                        if order != Ordering::Equal {
                            return Ok(order);
                        }
                    }
                }
            }
        }
    }
}
