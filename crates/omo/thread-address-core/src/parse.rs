//! The one reading of an address text.
//!
//! Upstream counterpart: `parse.ts`. A leaf: it imports nothing.

/// The shortest device id prefix a device part accepts: shorter would match most of an account's ids by chance.
pub const DEVICE_PREFIX_MIN: usize = 6;
/// The device part that names this device.
pub const HERE: &str = "here";

const THREAD_ID_GROUPS: [usize; 5] = [8, 4, 4, 4, 12];

/// A thread id: the session's durable id, `8-4-4-4-12` hex digits in either case (upstream `THREAD_ID_SHAPE`).
pub fn is_thread_id(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == THREAD_ID_GROUPS.len()
        && groups.iter().zip(THREAD_ID_GROUPS).all(|(group, length)| {
            group.len() == length && group.chars().all(|c| c.is_ascii_hexdigit())
        })
}

/// A host routing id: `rpc-` and at least one digit.
fn is_routing_id(text: &str) -> bool {
    text.strip_prefix("rpc-")
        .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
}

fn is_key_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_' || character == '-'
}

/// A device-service listing key: exactly 22 characters of base64url.
fn is_listing_key(text: &str) -> bool {
    text.len() == 22 && text.chars().all(is_key_character)
}

/// A session id: a host routing id (`rpc-N`) or another device's listing key (upstream `SESSION_ID_SHAPE`).
pub fn is_session_id(text: &str) -> bool {
    is_routing_id(text) || (text.len() >= 16 && text.chars().all(is_key_character))
}

/// A thread id, a host routing id or a device-service listing key: a text a device can be asked for without listing it
/// first (#13748). Narrower than [`is_session_id`], so an unlisted hyphenated name ("release-planner-2") is never passed
/// through. Residual: a name of exactly 22 characters with no spaces reads as a listing key.
pub fn id_shaped(text: &str) -> bool {
    is_thread_id(text) || is_routing_id(text) || is_listing_key(text)
}

/// The shortest text that names a thread by a prefix of its id: 8 hex characters, hyphens allowed after (upstream
/// `ID_PREFIX_SHAPE`).
pub fn is_id_prefix(text: &str) -> bool {
    text.len() >= 8
        && text.char_indices().all(|(index, character)| {
            character.is_ascii_hexdigit() || (index >= 8 && character == '-')
        })
}

/// How an address text reads: the caller itself, a bare address, or a thread on a named device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedAddress {
    /// The literal `self`.
    SelfThread,
    Bare {
        text: String,
    },
    Device {
        device: String,
        thread: String,
        text: String,
    },
}

fn is_drive_letter(part: &str) -> bool {
    let mut characters = part.chars();
    matches!(
        (characters.next(), characters.next(), characters.next()),
        (Some(letter), Some(':'), None) if letter.is_ascii_alphabetic()
    )
}

/// The one reading of an address text (design section 2). `<device>/<thread>` splits on its single `/` unless the text
/// is a path: a first part starting with `.` or `~`, a drive letter, or a `.jsonl` file. Any other text is a bare
/// address. A thread name may contain `/` itself, so the resolver still tries the whole text when the device reading
/// misses.
pub fn parse_address(input: &str) -> ParsedAddress {
    let text = input.trim();
    if text == "self" {
        return ParsedAddress::SelfThread;
    }
    let bare = || ParsedAddress::Bare {
        text: text.to_owned(),
    };
    let Some((device, thread)) = text.split_once('/') else {
        return bare();
    };
    if device.is_empty()
        || thread.is_empty()
        || thread.contains('/')
        || text.to_ascii_lowercase().ends_with(".jsonl")
        || is_drive_letter(device)
        || device.starts_with(['.', '~'])
    {
        return bare();
    }
    ParsedAddress::Device {
        device: device.to_owned(),
        thread: thread.to_owned(),
        text: text.to_owned(),
    }
}

/// `<device id>/<thread>`: the address every tool accepts for a thread on a known device.
pub fn format_address(device_id: &str, thread: &str) -> String {
    format!("{device_id}/{thread}")
}
