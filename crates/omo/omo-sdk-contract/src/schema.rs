//! The value shapes the contract's closed answers share: a non-empty string and the literal `true`.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A string that must not be empty (the contract's `Text`, `minLength: 1`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Text(String);

impl Text {
    pub fn new(text: impl Into<String>) -> Option<Self> {
        let text = text.into();
        (!text.is_empty()).then_some(Self(text))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Text {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        Self::new(text)
            .ok_or_else(|| <D::Error as serde::de::Error>::custom("expected a non-empty string"))
    }
}

/// The literal `true`: a field that is either absent or exactly `true`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct True;

impl Serialize for True {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bool(true)
    }
}

impl<'de> Deserialize<'de> for True {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        if bool::deserialize(deserializer)? {
            Ok(Self)
        } else {
            Err(<D::Error as serde::de::Error>::custom("expected true"))
        }
    }
}
