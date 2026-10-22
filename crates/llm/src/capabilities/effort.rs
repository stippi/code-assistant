use serde::{Deserialize, Serialize};
use std::fmt;

/// How hard a model thinks before answering, independent of the provider.
///
/// The variants are ordered from least to most effort; each model supports a
/// subset (see [`super::ReasoningSpec::levels`]) and its reasoning style
/// translates a level into the provider's request fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    /// Reasoning switched off.
    Off,
    Minimal,
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
    Max,
}

impl Effort {
    pub const ALL: [Effort; 7] = [
        Effort::Off,
        Effort::Minimal,
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::XHigh,
        Effort::Max,
    ];

    /// The value providers use for this level (`"xhigh"`, `"medium"`, ...).
    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Off => "off",
            Effort::Minimal => "minimal",
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::XHigh => "xhigh",
            Effort::Max => "max",
        }
    }

    /// Human-readable label for selectors.
    pub fn label(self) -> &'static str {
        match self {
            Effort::Off => "Off",
            Effort::Minimal => "Minimal",
            Effort::Low => "Low",
            Effort::Medium => "Medium",
            Effort::High => "High",
            Effort::XHigh => "Extra high",
            Effort::Max => "Max",
        }
    }

    pub fn parse(value: &str) -> Option<Effort> {
        Effort::ALL
            .into_iter()
            .find(|effort| effort.as_str().eq_ignore_ascii_case(value.trim()))
    }

    /// The supported level closest to `self`. `Off` only maps to `Off`; when
    /// two levels are equally close the higher one wins. Returns `None` only
    /// for an empty `supported` list.
    pub fn closest_in(self, supported: &[Effort]) -> Option<Effort> {
        if supported.contains(&self) {
            return Some(self);
        }
        let candidates = supported.iter().copied().filter(|e| *e != Effort::Off);
        let distance = |e: Effort| (e as i32 - self as i32).abs();
        candidates
            .min_by_key(|e| (distance(*e), std::cmp::Reverse(*e)))
            .or_else(|| supported.first().copied())
    }
}

impl fmt::Display for Effort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_as_provider_strings() {
        assert_eq!(serde_json::to_value(Effort::XHigh).unwrap(), "xhigh");
        assert_eq!(
            serde_json::from_value::<Effort>("medium".into()).unwrap(),
            Effort::Medium
        );
        assert_eq!(Effort::parse("XHigh"), Some(Effort::XHigh));
        assert_eq!(Effort::parse("extreme"), None);
    }

    #[test]
    fn closest_level_prefers_the_higher_neighbour() {
        let anthropic = [
            Effort::Low,
            Effort::Medium,
            Effort::High,
            Effort::XHigh,
            Effort::Max,
        ];
        assert_eq!(Effort::Minimal.closest_in(&anthropic), Some(Effort::Low));
        assert_eq!(Effort::High.closest_in(&anthropic), Some(Effort::High));

        let gemini_pro = [Effort::Low, Effort::High];
        assert_eq!(Effort::Medium.closest_in(&gemini_pro), Some(Effort::High));
        assert_eq!(Effort::Max.closest_in(&gemini_pro), Some(Effort::High));
    }

    #[test]
    fn off_never_maps_to_a_reasoning_level_and_back() {
        let with_off = [Effort::Off, Effort::Low, Effort::High];
        assert_eq!(Effort::Off.closest_in(&with_off), Some(Effort::Off));
        // A model that cannot switch reasoning off runs at its lowest level.
        let without_off = [Effort::Low, Effort::High];
        assert_eq!(Effort::Off.closest_in(&without_off), Some(Effort::Low));
        assert_eq!(Effort::Low.closest_in(&[Effort::Off]), Some(Effort::Off));
        assert_eq!(Effort::Low.closest_in(&[]), None);
    }
}
