//! senpi's approval modes, and which of them a creator may hand to a thread it creates.
//!
//! Upstream counterpart: `thread/contracts/permission-presets.ts` (omo #13409).

/// senpi's approval modes (`permission-system/types.ts` `PermissionPresetName`).
pub const PERMISSION_PRESETS: &[&str] = &[
    "full-access",
    "workspace",
    "accept-edits",
    "auto",
    "read-only",
    "ask",
];

pub fn is_permission_preset(value: &str) -> bool {
    PERMISSION_PRESETS.contains(&value)
}

/// The modes each mode lets through without asking, by senpi's preset rules (`permission-system/config.ts`):
/// full-access allows everything; workspace allows read, edit and bash; accept-edits drops bash; read-only drops edit;
/// ask allows nothing. auto asks for everything and approves only its own allowlist, so it is below full-access and
/// above ask only: its allowlist includes calls read-only would ask for, and read-only allows reads auto may ask for.
fn at_most(creator: &str) -> &'static [&'static str] {
    match creator {
        "full-access" => PERMISSION_PRESETS,
        "workspace" => &["workspace", "accept-edits", "read-only", "ask"],
        "accept-edits" => &["accept-edits", "read-only", "ask"],
        "read-only" => &["read-only", "ask"],
        "auto" => &["auto", "ask"],
        _ => &["ask"],
    }
}

/// Whether a session in `creator`'s mode may start a thread in `requested`: the same mode or one that lets less
/// through. A creator whose mode is unknown may only ask for `ask`, the one mode below every other.
pub fn preset_within(requested: &str, creator: Option<&str>) -> bool {
    match creator {
        Some(creator) if is_permission_preset(creator) => at_most(creator).contains(&requested),
        _ => requested == "ask",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_creator_keeps_or_narrows_its_own_mode_and_never_widens_it() {
        let cases: [(Option<&str>, &str, bool); 13] = [
            (Some("workspace"), "workspace", true),
            (Some("workspace"), "accept-edits", true),
            (Some("workspace"), "read-only", true),
            (Some("workspace"), "ask", true),
            (Some("full-access"), "auto", true),
            (Some("workspace"), "full-access", false),
            (Some("read-only"), "accept-edits", false),
            (Some("read-only"), "auto", false),
            (Some("auto"), "read-only", false),
            (Some("auto"), "ask", true),
            (Some("ask"), "read-only", false),
            (None, "ask", true),
            (None, "read-only", false),
        ];
        for (creator, asked, allowed) in cases {
            assert_eq!(
                preset_within(asked, creator),
                allowed,
                "creator {creator:?} asking for {asked}"
            );
        }
    }

    #[test]
    fn a_creator_in_a_mode_senpi_does_not_name_may_only_ask() {
        assert!(preset_within("ask", Some("yolo")));
        assert!(!preset_within("workspace", Some("yolo")));
    }

    #[test]
    fn every_mode_is_within_full_access_and_names_itself() {
        for preset in PERMISSION_PRESETS {
            assert!(is_permission_preset(preset));
            assert!(preset_within(preset, Some("full-access")), "{preset}");
            assert!(preset_within(preset, Some(preset)), "{preset}");
        }
        assert!(!is_permission_preset("yolo"));
    }
}
