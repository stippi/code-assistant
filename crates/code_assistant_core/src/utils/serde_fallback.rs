//! Lenient deserialization for persisted settings.
//!
//! Session files outlive the code that wrote them: a setting may hold a
//! value that a newer (or another branch's) build introduced and this build
//! doesn't know, or one that was removed since. Failing the whole session
//! load over such a field would lock the user out of the conversation, so
//! settings fields fall back to their default instead.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};

/// Deserializes `T`, falling back to `T::default()` when the stored value
/// doesn't parse (e.g. an unknown enum variant). Use as
/// `#[serde(default, deserialize_with = "or_default")]`.
pub fn or_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned + Default,
{
    or_else(deserializer, T::default)
}

/// Like [`or_default`], but with an explicit fallback — for settings whose
/// default isn't a safe stand-in for an unknown value.
pub fn or_else<'de, D, T>(deserializer: D, fallback: impl FnOnce() -> T) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    // Buffer the value first so a parse failure doesn't leave the outer
    // deserializer in the middle of a field.
    let value = serde_json::Value::deserialize(deserializer)?;
    match T::deserialize(&value) {
        Ok(parsed) => Ok(parsed),
        Err(e) => {
            tracing::warn!(
                "Ignoring unsupported {} value {value}: {e}; using a fallback",
                std::any::type_name::<T>()
            );
            Ok(fallback())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default, PartialEq, Deserialize)]
    #[serde(rename_all = "kebab-case")]
    enum Mode {
        #[default]
        Off,
        On,
    }

    #[derive(Debug, Deserialize)]
    struct Settings {
        #[serde(default, deserialize_with = "or_default")]
        mode: Mode,
        #[serde(default)]
        other: u32,
    }

    #[test]
    fn known_values_parse_normally() {
        let s: Settings = serde_json::from_str(r#"{"mode":"on","other":3}"#).unwrap();
        assert_eq!(s.mode, Mode::On);
        assert_eq!(s.other, 3);
    }

    #[test]
    fn unknown_values_fall_back_without_losing_other_fields() {
        let s: Settings = serde_json::from_str(r#"{"mode":"auto","other":3}"#).unwrap();
        assert_eq!(s.mode, Mode::Off);
        assert_eq!(s.other, 3);

        let s: Settings = serde_json::from_str(r#"{"mode":{"nested":true},"other":3}"#).unwrap();
        assert_eq!(s.mode, Mode::Off);
        assert_eq!(s.other, 3);
    }

    #[test]
    fn missing_values_use_the_default() {
        let s: Settings = serde_json::from_str(r#"{"other":3}"#).unwrap();
        assert_eq!(s.mode, Mode::Off);
    }
}
