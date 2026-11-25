use serde::{Deserialize, Serialize};

use crate::internal::validate::{Node, ObjectSpec, array, enumeration, optional};

pub const KEYWORD_TYPES: [&str; 4] = ["ultrawork", "team", "hyperplan", "hyperplan-ultrawork"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeywordType {
    Ultrawork,
    Team,
    Hyperplan,
    HyperplanUltrawork,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OmoKeywordDetectorSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled_expansions: Option<Vec<KeywordType>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_keywords: Option<Vec<KeywordType>>,
}

pub fn omo_keyword_detector_settings_schema() -> Node {
    Node::Object(Box::new(ObjectSpec {
        fields: vec![
            optional("enabled_expansions", array(enumeration(&KEYWORD_TYPES))),
            optional("disabled_keywords", array(enumeration(&KEYWORD_TYPES))),
        ],
        ..ObjectSpec::default()
    }))
}
