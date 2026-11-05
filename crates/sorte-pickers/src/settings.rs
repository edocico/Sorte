//! A picker's settings: the schema the Aspetto panel is generated from and the list's
//! `picker_settings` JSON is checked against, and the checked values the picker reads.
//!
//! A picker declares each field once, as a constant, and reads it back with the same constant:
//!
//! ```
//! use sorte_pickers::{IntField, LocalizedStr, PickerSettings, SettingsSchema};
//!
//! const SPEED: IntField =
//!     IntField { key: "speed", label: LocalizedStr::new("Velocità", "Speed"), min: 1, max: 10, default: 5 };
//!
//! let schema = SettingsSchema::new(vec![SPEED.into()]);
//! let settings: PickerSettings = schema.validate(&serde_json::json!({ "speed": 12 }));
//! assert_eq!(settings.int(&SPEED), 10);
//! ```

use std::collections::BTreeMap;

use serde_json::Value;

use crate::localized::LocalizedStr;

/// A yes/no setting, shown as a switch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoolField {
    /// Where the value lives in the list's `picker_settings` object.
    pub key: &'static str,
    pub label: LocalizedStr,
    pub default: bool,
}

/// A whole number within `min..=max`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IntField {
    pub key: &'static str,
    pub label: LocalizedStr,
    pub min: i64,
    pub max: i64,
    pub default: i64,
}

/// A number within `min..=max`, shown as a slider that moves by `step`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FloatField {
    pub key: &'static str,
    pub label: LocalizedStr,
    pub min: f64,
    pub max: f64,
    /// The slider's increment. Values between steps are still valid.
    pub step: f64,
    pub default: f64,
}

/// One option among several, shown as a drop-down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChoiceField {
    pub key: &'static str,
    pub label: LocalizedStr,
    pub options: &'static [Choice],
    /// The key of the option a list starts with.
    pub default: &'static str,
}

/// An option of a [`ChoiceField`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Choice {
    /// What the list's file stores, and what [`PickerSettings::choice`] returns.
    pub key: &'static str,
    pub label: LocalizedStr,
}

/// One field of a [`SettingsSchema`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SettingField {
    Bool(BoolField),
    Int(IntField),
    Float(FloatField),
    Choice(ChoiceField),
}

impl SettingField {
    pub fn key(&self) -> &'static str {
        match self {
            Self::Bool(f) => f.key,
            Self::Int(f) => f.key,
            Self::Float(f) => f.key,
            Self::Choice(f) => f.key,
        }
    }

    pub fn label(&self) -> LocalizedStr {
        match self {
            Self::Bool(f) => f.label,
            Self::Int(f) => f.label,
            Self::Float(f) => f.label,
            Self::Choice(f) => f.label,
        }
    }
}

impl From<BoolField> for SettingField {
    fn from(field: BoolField) -> Self {
        Self::Bool(field)
    }
}

impl From<IntField> for SettingField {
    fn from(field: IntField) -> Self {
        Self::Int(field)
    }
}

impl From<FloatField> for SettingField {
    fn from(field: FloatField) -> Self {
        Self::Float(field)
    }
}

impl From<ChoiceField> for SettingField {
    fn from(field: ChoiceField) -> Self {
        Self::Choice(field)
    }
}

/// The settings a picker offers, in the order the Aspetto panel shows them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SettingsSchema {
    fields: Vec<SettingField>,
}

impl SettingsSchema {
    pub fn new(fields: Vec<SettingField>) -> Self {
        Self { fields }
    }

    pub fn fields(&self) -> &[SettingField] {
        &self.fields
    }

    /// Reads a list's `picker_settings` against this schema. Never fails: a missing field (or
    /// `null`) takes its default, a number outside its range takes the nearest bound, and a
    /// value of the wrong type or an unknown choice takes the default; the last three are
    /// reported in [`PickerSettings::issues`]. Keys the schema does not name are ignored.
    pub fn validate(&self, json: &Value) -> PickerSettings {
        let mut settings = PickerSettings::default();
        let object = match json {
            Value::Object(object) => Some(object),
            Value::Null => None,
            _ => {
                settings.issues.push(SettingIssue::NotAnObject);
                None
            }
        };
        for field in &self.fields {
            let key = field.key();
            let raw = object.and_then(|object| object.get(key)).filter(|value| !value.is_null());
            let (value, issue) = match raw {
                None => (default_value(field), None),
                Some(raw) => read_value(field, raw),
            };
            if let Some(issue) = issue {
                settings.issues.push(issue);
            }
            settings.values.insert(key, value);
        }
        settings
    }
}

/// A value [`SettingsSchema::validate`] had to replace, so the app can tell the user why a
/// setting changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingIssue {
    /// The settings were neither an object nor absent: every field took its default.
    NotAnObject,
    /// The value had the wrong type (a string for a number, say): the default replaced it.
    WrongType { key: &'static str },
    /// The number was outside the field's range: the nearest bound replaced it.
    OutOfRange { key: &'static str },
    /// The value is not one of the field's options: the default replaced it.
    UnknownChoice { key: &'static str },
}

/// A picker's settings, checked against its schema by [`SettingsSchema::validate`].
///
/// Each getter takes the field's declaration and always answers within it: a field these
/// settings do not hold (they came from another schema, or are `PickerSettings::default()`)
/// reads as its default.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PickerSettings {
    values: BTreeMap<&'static str, Value>,
    issues: Vec<SettingIssue>,
}

impl PickerSettings {
    pub fn bool(&self, field: &BoolField) -> bool {
        self.values.get(field.key).and_then(Value::as_bool).unwrap_or(field.default)
    }

    pub fn int(&self, field: &IntField) -> i64 {
        let value = self.values.get(field.key).and_then(Value::as_i64).unwrap_or(field.default);
        clamp_int(field, value)
    }

    pub fn float(&self, field: &FloatField) -> f64 {
        let value = self.values.get(field.key).and_then(Value::as_f64).unwrap_or(field.default);
        clamp_float(field, value)
    }

    /// The key of the chosen option.
    pub fn choice(&self, field: &ChoiceField) -> &'static str {
        let stored = self.values.get(field.key).and_then(Value::as_str);
        stored.and_then(|key| option(field, key)).unwrap_or_else(|| default_choice(field))
    }

    /// The values [`SettingsSchema::validate`] replaced, in schema order.
    pub fn issues(&self) -> &[SettingIssue] {
        &self.issues
    }
}

/// The field's default, kept within its own bounds.
fn default_value(field: &SettingField) -> Value {
    match field {
        SettingField::Bool(f) => Value::Bool(f.default),
        SettingField::Int(f) => Value::from(clamp_int(f, f.default)),
        SettingField::Float(f) => Value::from(clamp_float(f, f.default)),
        SettingField::Choice(f) => Value::from(default_choice(f)),
    }
}

/// `raw` checked against `field`, and the issue when it had to be replaced.
fn read_value(field: &SettingField, raw: &Value) -> (Value, Option<SettingIssue>) {
    let key = field.key();
    let wrong_type = || (default_value(field), Some(SettingIssue::WrongType { key }));
    match field {
        SettingField::Bool(_) => match raw.as_bool() {
            Some(value) => (Value::Bool(value), None),
            None => wrong_type(),
        },
        SettingField::Int(f) => match int_of(raw) {
            Some(value) => {
                let clamped = clamp_int(f, value);
                let issue = (clamped != value).then_some(SettingIssue::OutOfRange { key });
                (Value::from(clamped), issue)
            }
            None => wrong_type(),
        },
        SettingField::Float(f) => match raw.as_f64() {
            Some(value) => {
                let clamped = clamp_float(f, value);
                let issue = (clamped != value).then_some(SettingIssue::OutOfRange { key });
                (Value::from(clamped), issue)
            }
            None => wrong_type(),
        },
        SettingField::Choice(f) => match raw.as_str() {
            Some(name) => match option(f, name) {
                Some(key) => (Value::from(key), None),
                None => (default_value(field), Some(SettingIssue::UnknownChoice { key })),
            },
            None => wrong_type(),
        },
    }
}

/// A JSON number as a whole number, saturating: `12`, `12.0` and `1e30` are whole, `1.5` is not.
fn int_of(raw: &Value) -> Option<i64> {
    if let Some(value) = raw.as_i64() {
        return Some(value);
    }
    if raw.as_u64().is_some() {
        return Some(i64::MAX);
    }
    // `as` saturates, and JSON numbers are finite.
    raw.as_f64().filter(|value| value.fract() == 0.0).map(|value| value as i64)
}

/// `value` within `min..=max`; never panics, even on a field whose bounds are swapped.
fn clamp_int(field: &IntField, value: i64) -> i64 {
    value.max(field.min).min(field.max)
}

/// `value` within `min..=max`, and the default for NaN; never panics.
fn clamp_float(field: &FloatField, value: f64) -> f64 {
    let value = if value.is_nan() { field.default } else { value };
    if value < field.min {
        field.min
    } else if value > field.max {
        field.max
    } else {
        value
    }
}

/// The key of `field`'s option named `name`, as the schema spells it.
fn option(field: &ChoiceField, name: &str) -> Option<&'static str> {
    field.options.iter().find(|choice| choice.key == name).map(|choice| choice.key)
}

/// The default option, or the first one when the default names none of them.
fn default_choice(field: &ChoiceField) -> &'static str {
    option(field, field.default).or_else(|| field.options.first().map(|choice| choice.key)).unwrap_or(field.default)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use serde_json::json;

    use super::*;

    const fn label(text: &'static str) -> LocalizedStr {
        LocalizedStr::new(text, text)
    }

    const MYSTERY: BoolField = BoolField { key: "mystery", label: label("mystery"), default: false };
    const SPEED: IntField = IntField { key: "speed", label: label("speed"), min: 1, max: 10, default: 5 };
    const ZOOM: FloatField =
        FloatField { key: "zoom", label: label("zoom"), min: 0.5, max: 2.0, step: 0.1, default: 1.0 };
    const THEMES: &[Choice] =
        &[Choice { key: "classic", label: label("classic") }, Choice { key: "neon", label: label("neon") }];
    const THEME: ChoiceField = ChoiceField { key: "theme", label: label("theme"), options: THEMES, default: "classic" };

    fn schema() -> SettingsSchema {
        SettingsSchema::new(vec![MYSTERY.into(), SPEED.into(), ZOOM.into(), THEME.into()])
    }

    #[test]
    fn missing_settings_read_as_defaults_without_issues() {
        for json in [json!(null), json!({}), json!({ "speed": null, "other": 3 })] {
            let settings = schema().validate(&json);
            assert_eq!(settings.issues(), [], "{json}");
            assert!(!settings.bool(&MYSTERY));
            assert_eq!(settings.int(&SPEED), 5);
            assert_eq!(settings.float(&ZOOM), 1.0);
            assert_eq!(settings.choice(&THEME), "classic");
        }
    }

    #[test]
    fn valid_values_are_kept() {
        let json = json!({ "mystery": true, "speed": 9, "zoom": 1.35, "theme": "neon" });
        let settings = schema().validate(&json);
        assert_eq!(settings.issues(), []);
        assert!(settings.bool(&MYSTERY));
        assert_eq!(settings.int(&SPEED), 9);
        assert_eq!(settings.float(&ZOOM), 1.35);
        assert_eq!(settings.choice(&THEME), "neon");
    }

    #[test]
    fn numbers_outside_their_range_take_the_nearest_bound() {
        let settings = schema().validate(&json!({ "speed": -3, "zoom": 7.5 }));
        assert_eq!(settings.int(&SPEED), 1);
        assert_eq!(settings.float(&ZOOM), 2.0);
        let expected = [SettingIssue::OutOfRange { key: "speed" }, SettingIssue::OutOfRange { key: "zoom" }];
        assert_eq!(settings.issues(), expected);
        let huge = schema().validate(&json!({ "speed": u64::MAX }));
        assert_eq!((huge.int(&SPEED), huge.issues()), (10, &[SettingIssue::OutOfRange { key: "speed" }][..]));
    }

    #[test]
    fn whole_floats_count_as_ints_and_fractions_do_not() {
        let whole = schema().validate(&json!({ "speed": 7.0 }));
        assert_eq!((whole.int(&SPEED), whole.issues()), (7, &[][..]));
        let fraction = schema().validate(&json!({ "speed": 7.5 }));
        assert_eq!((fraction.int(&SPEED), fraction.issues()), (5, &[SettingIssue::WrongType { key: "speed" }][..]));
        let ints_are_floats = schema().validate(&json!({ "zoom": 2 }));
        assert_eq!((ints_are_floats.float(&ZOOM), ints_are_floats.issues()), (2.0, &[][..]));
    }

    #[test]
    fn wrong_types_and_unknown_choices_are_reported_and_take_defaults() {
        let json = json!({ "mystery": "yes", "speed": "fast", "zoom": [1], "theme": "plaid" });
        let settings = schema().validate(&json);
        assert_eq!(
            settings.issues(),
            [
                SettingIssue::WrongType { key: "mystery" },
                SettingIssue::WrongType { key: "speed" },
                SettingIssue::WrongType { key: "zoom" },
                SettingIssue::UnknownChoice { key: "theme" },
            ]
        );
        assert_eq!((settings.bool(&MYSTERY), settings.int(&SPEED)), (false, 5));
        assert_eq!((settings.float(&ZOOM), settings.choice(&THEME)), (1.0, "classic"));
        let numeric_theme = schema().validate(&json!({ "theme": 1 }));
        assert_eq!(numeric_theme.issues(), [SettingIssue::WrongType { key: "theme" }]);
    }

    #[test]
    fn settings_that_are_not_an_object_take_every_default() {
        for json in [json!([1, 2]), json!("speed=3"), json!(4), json!(true)] {
            let settings = schema().validate(&json);
            assert_eq!(settings.issues(), [SettingIssue::NotAnObject], "{json}");
            assert_eq!(settings, schema().validate(&json!({})).with_issues(vec![SettingIssue::NotAnObject]));
        }
    }

    #[test]
    fn getters_answer_with_defaults_for_fields_they_do_not_hold() {
        let settings = PickerSettings::default();
        assert_eq!((settings.bool(&MYSTERY), settings.int(&SPEED), settings.choice(&THEME)), (false, 5, "classic"));
        // Settings checked against another schema that reuses a key with other bounds.
        const OTHER_SPEED: IntField = IntField { max: 100, ..SPEED };
        let other = SettingsSchema::new(vec![OTHER_SPEED.into()]).validate(&json!({ "speed": 80 }));
        assert_eq!(other.int(&SPEED), 10);
    }

    #[test]
    fn malformed_fields_never_panic_and_still_answer_within_reason() {
        const SWAPPED: IntField = IntField { min: 10, max: 1, ..SPEED };
        const NAN_DEFAULT: FloatField = FloatField { default: f64::NAN, ..ZOOM };
        const NO_OPTIONS: ChoiceField = ChoiceField { options: &[], ..THEME };
        const BAD_DEFAULT: ChoiceField = ChoiceField { default: "missing", ..THEME };
        let schema =
            SettingsSchema::new(vec![SWAPPED.into(), NAN_DEFAULT.into(), NO_OPTIONS.into(), BAD_DEFAULT.into()]);
        let settings = schema.validate(&json!({ "speed": 5, "zoom": "x" }));
        assert_eq!(settings.int(&SWAPPED), 1);
        assert!(settings.float(&NAN_DEFAULT).is_nan(), "a NaN default is the schema's bug, kept visible");
        assert_eq!(settings.choice(&NO_OPTIONS), "classic");
        assert_eq!(settings.choice(&BAD_DEFAULT), "classic");
    }

    /// Any JSON value a hand-edited file could hold.
    fn any_json() -> impl Strategy<Value = Value> {
        let leaf = prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Bool),
            any::<i64>().prop_map(Value::from),
            any::<u64>().prop_map(Value::from),
            (-1e6f64..1e6).prop_map(Value::from),
            prop::sample::select(vec!["classic", "neon", "plaid", ""]).prop_map(Value::from),
        ];
        leaf.prop_recursive(2, 8, 4, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
                prop::collection::btree_map(
                    prop::sample::select(vec!["mystery", "speed", "zoom", "theme", "extra"]).prop_map(String::from),
                    inner,
                    0..5,
                )
                .prop_map(|map| Value::Object(map.into_iter().collect())),
            ]
        })
    }

    proptest! {
        #[test]
        fn every_getter_answers_within_its_field(json in any_json()) {
            let settings = schema().validate(&json);
            prop_assert!((SPEED.min..=SPEED.max).contains(&settings.int(&SPEED)));
            prop_assert!((ZOOM.min..=ZOOM.max).contains(&settings.float(&ZOOM)));
            prop_assert!(THEMES.iter().any(|choice| choice.key == settings.choice(&THEME)));
            prop_assert!(settings.issues().len() <= schema().fields().len());
        }

        #[test]
        fn validating_the_checked_values_again_changes_nothing(json in any_json()) {
            let settings = schema().validate(&json);
            let again = schema().validate(&json!({
                "mystery": settings.bool(&MYSTERY),
                "speed": settings.int(&SPEED),
                "zoom": settings.float(&ZOOM),
                "theme": settings.choice(&THEME),
            }));
            prop_assert_eq!(again.issues(), &[]);
            prop_assert_eq!(again, settings.with_issues(Vec::new()));
        }
    }

    impl PickerSettings {
        fn with_issues(mut self, issues: Vec<SettingIssue>) -> Self {
            self.issues = issues;
            self
        }
    }
}
