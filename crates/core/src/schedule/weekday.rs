use std::collections::HashSet;

use chrono::Weekday;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Set of weekdays a trigger is eligible to fire on. Empty means "every
/// day". `HashSet` provides O(1) membership; canonical ordering on serialize
/// is enforced by sorting on `num_days_from_monday` before writing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WeekdaySet(pub HashSet<Weekday>);

impl WeekdaySet {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn contains(&self, day: Weekday) -> bool {
        self.0.contains(&day)
    }

    /// True if `day` is eligible: empty set means every day; otherwise
    /// require explicit membership.
    pub fn allows(&self, day: Weekday) -> bool {
        self.is_empty() || self.contains(day)
    }
}

impl Serialize for WeekdaySet {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Sort by canonical Mon–Sun order before writing so the output is
        // deterministic regardless of HashSet iteration order.
        let mut days: Vec<Weekday> = self.0.iter().copied().collect();
        days.sort_by_key(Weekday::num_days_from_monday);
        days.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for WeekdaySet {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let days = Vec::<Weekday>::deserialize(deserializer)?;
        Ok(WeekdaySet(days.into_iter().collect()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_allows_every_day() {
        let ws = WeekdaySet::default();
        assert!(ws.allows(Weekday::Mon));
        assert!(ws.allows(Weekday::Sun));
    }

    #[test]
    fn specific_days_only() {
        let mut ws = WeekdaySet::default();
        ws.0.insert(Weekday::Tue);
        ws.0.insert(Weekday::Thu);
        assert!(ws.allows(Weekday::Tue));
        assert!(!ws.allows(Weekday::Wed));
    }

    #[test]
    fn ron_round_trip() {
        let mut ws = WeekdaySet::default();
        ws.0.insert(Weekday::Mon);
        ws.0.insert(Weekday::Fri);
        let s = ron::to_string(&ws).expect("ser");
        let back: WeekdaySet = ron::from_str(&s).expect("de");
        assert_eq!(ws, back);
    }
}
