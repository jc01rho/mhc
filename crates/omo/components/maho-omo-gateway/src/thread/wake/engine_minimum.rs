pub const MIN_WAKE_ENGINE_VERSION: Option<&str> = Some("2026.11.15-2");

const MIN_WAKE_OMO_VERSION: &str = "5.1.72";
const OMOB_PREFIX: &str = "0.0.0-omob.";

fn parse_calver_fields(version: &str) -> Option<[i64; 4]> {
    let mut dots = version.splitn(3, '.');
    let year: i64 = dots.next()?.parse().ok()?;
    let month: i64 = dots.next()?.parse().ok()?;
    let rest = dots.next()?;
    let day_digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if day_digits.is_empty() {
        return None;
    }
    let day: i64 = day_digits.parse().ok()?;
    let tail = &rest[day_digits.len()..];
    if tail.is_empty() {
        return Some([year, month, day, 0]);
    }
    let first = tail.as_bytes()[0];
    if first != b'-' && first != b'+' && first != b'.' {
        return None;
    }
    if first == b'-' {
        let after_dash = &tail[1..];
        let rev_digits: String = after_dash
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if !rev_digits.is_empty() {
            let after_rev = &after_dash[rev_digits.len()..];
            if after_rev.is_empty() || matches!(after_rev.as_bytes()[0], b'-' | b'+' | b'.') {
                let rev: i64 = rev_digits.parse().ok()?;
                return Some([year, month, day, rev]);
            }
        }
    }
    Some([year, month, day, 0])
}

fn parse_omo_release_fields(version: &str) -> Option<[i64; 4]> {
    let mut dots = version.splitn(3, '.');
    let major: i64 = dots.next()?.parse().ok()?;
    if major >= 1000 {
        return None;
    }
    let minor: i64 = dots.next()?.parse().ok()?;
    let rest = dots.next()?;
    let patch_digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if patch_digits.is_empty() {
        return None;
    }
    let patch: i64 = patch_digits.parse().ok()?;
    let tail = &rest[patch_digits.len()..];
    if tail.is_empty() || tail.starts_with('+') {
        return Some([major, minor, patch, 0]);
    }
    if tail.starts_with('-') {
        return Some([major, minor, patch, -1]);
    }
    None
}

fn at_least(fields: &[i64; 4], floor: &[i64; 4]) -> bool {
    for (field, bound) in fields.iter().zip(floor.iter()) {
        if field != bound {
            return field > bound;
        }
    }
    true
}

pub fn engine_drops_retained_record(version: Option<&str>, minimum: Option<&str>) -> bool {
    let (Some(version), Some(minimum)) = (version, minimum) else {
        return false;
    };
    let omob = version.starts_with(OMOB_PREFIX);
    let omo = if omob {
        None
    } else {
        parse_omo_release_fields(version)
    };
    if omob || omo.is_some() {
        if Some(minimum) != MIN_WAKE_ENGINE_VERSION {
            return false;
        }
        if omob {
            return true;
        }
        let (Some(omo_fields), Some(floor_fields)) =
            (omo, parse_omo_release_fields(MIN_WAKE_OMO_VERSION))
        else {
            return false;
        };
        return at_least(&omo_fields, &floor_fields);
    }
    let (Some(fields), Some(floor)) = (parse_calver_fields(version), parse_calver_fields(minimum))
    else {
        return false;
    };
    at_least(&fields, &floor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_the_shipped_minimum_then_it_is_the_first_senpi_release_carrying_senpi_6449() {
        assert_eq!(MIN_WAKE_ENGINE_VERSION, Some("2026.11.15-2"));
    }

    #[test]
    fn given_an_engine_version_when_the_shipped_minimum_is_read_then_the_wake_gate_matches() {
        let cases = [
            ("2026.11.13", false),
            ("2026.11.15", false),
            ("2026.11.15-1", false),
            ("2026.11.15-2", true),
            ("2026.11.15-3", true),
            ("2026.11.16", true),
            ("2026.11.15-2+build.7", true),
            ("not-a-version", false),
            ("5.1.70", false),
            ("5.1.71", false),
            ("5.1.72", true),
            ("5.1.73-beta.1", true),
            ("5.1.72-beta.1", false),
            ("5.1.72+build.3", true),
            ("5.2.0", true),
            ("0.0.0-omob.c072ae7.43bff8d+1794717661.43bff8d", true),
            (
                "2026.11.19-2-1-omob.6d05a3b.1975499+1795063675.1975499",
                true,
            ),
            ("2026.11.19-2-1-omob.6d05a3b.1975499", true),
            ("2026.11.15-2-dev.3", true),
            ("2026.11.15-1-1-omob.6d05a3b.1975499", false),
            ("2026.11.15-omob.6d05a3b", false),
        ];
        for (version, expected) in cases {
            assert_eq!(
                engine_drops_retained_record(Some(version), MIN_WAKE_ENGINE_VERSION),
                expected,
                "version: {version}"
            );
        }
        assert!(!engine_drops_retained_record(None, MIN_WAKE_ENGINE_VERSION));
        assert!(!engine_drops_retained_record(Some("2026.11.15-2"), None));
    }

    #[test]
    fn given_an_omo_stamped_engine_when_the_caller_minimum_differs_from_shipped_then_wake_does_not_run()
     {
        for version in ["5.2.0", "0.0.0-omob.c072ae7.43bff8d+1794717661.43bff8d"] {
            assert!(!engine_drops_retained_record(
                Some(version),
                Some("2026.12.1")
            ));
            assert!(engine_drops_retained_record(
                Some(version),
                MIN_WAKE_ENGINE_VERSION
            ));
        }
    }
}
