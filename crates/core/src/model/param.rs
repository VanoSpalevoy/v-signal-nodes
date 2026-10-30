//! Node parameters: declared by a [`NodeSpec`](super::NodeSpec), stored per node.
//!
//! A plugin declares each parameter once as a typed key constant
//! ([`NumberParam`], [`ChoiceParam`], [`ToggleParam`]) and builds both the
//! parameter's [`ParamSpec`] and its reads in `process` from it, so the two can't
//! disagree on the id or the kind:
//!
//! ```
//! use v_signal_nodes_core::{NumberParam, ParamKey, ParamValue};
//!
//! const GAIN: NumberParam = NumberParam("gain");
//!
//! let spec = GAIN.spec("Gain", 0.0, 2.0, 1.0);
//! assert_eq!(spec.id, "gain");
//! assert_eq!(GAIN.read(spec.default_value()), Some(1.0));
//! assert_eq!(GAIN.read(ParamValue::Toggle(true)), None);
//! ```

use crate::ParamProblem;
use crate::model::TextKey;

/// What values a parameter takes, with its default.
#[derive(Clone, Debug, PartialEq)]
pub enum ParamKind {
    /// A number in `min..=max`.
    Number { min: f32, max: f32, default: f32 },
    /// One of `options` (labels), chosen by index.
    Choice {
        options: Vec<String>,
        default: usize,
    },
    /// On or off.
    Toggle { default: bool },
}

/// One parameter of a node type.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamSpec {
    /// Id unique within the node type, e.g. `"gain"`.
    pub id: String,
    /// Display name.
    pub name: String,
    pub kind: ParamKind,
}

/// Current value of a parameter.
///
/// Not part of the C ABI: dynamically loaded plugins get values as
/// `plugin-api`'s `ffi::FfiParamValue`. With the `serde` feature it serializes as
/// `{"number": 1.0}`, `{"choice": 0}` or `{"toggle": true}`.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum ParamValue {
    Number(f32),
    /// Index into [`ParamKind::Choice::options`].
    Choice(usize),
    Toggle(bool),
}

impl ParamValue {
    /// The number, if this is a [`Number`](Self::Number).
    pub fn as_number(self) -> Option<f32> {
        match self {
            Self::Number(value) => Some(value),
            _ => None,
        }
    }

    /// The option index, if this is a [`Choice`](Self::Choice).
    pub fn as_choice(self) -> Option<usize> {
        match self {
            Self::Choice(index) => Some(index),
            _ => None,
        }
    }

    /// The state, if this is a [`Toggle`](Self::Toggle).
    pub fn as_toggle(self) -> Option<bool> {
        match self {
            Self::Toggle(on) => Some(on),
            _ => None,
        }
    }
}

/// A typed parameter key: the parameter's id plus the type of its value.
pub trait ParamKey: Copy {
    /// What reading the parameter gives.
    type Value;

    /// The parameter's [`id`](ParamSpec::id).
    fn id(self) -> &'static str;

    /// `value` as this key's type, or `None` if it's another kind of value.
    fn read(self, value: ParamValue) -> Option<Self::Value>;

    /// The [`TextKey`] of the parameter's name, for translations.
    fn name_key(self) -> TextKey {
        TextKey::param(self.id())
    }
}

/// Key of a [`Number`](ParamKind::Number) parameter; reads as `f32`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NumberParam(pub &'static str);

/// Key of a [`Choice`](ParamKind::Choice) parameter; reads as the option index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ChoiceParam(pub &'static str);

/// Key of a [`Toggle`](ParamKind::Toggle) parameter; reads as `bool`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ToggleParam(pub &'static str);

impl NumberParam {
    /// The spec of this parameter: a number in `min..=max`.
    pub fn spec(self, name: impl Into<String>, min: f32, max: f32, default: f32) -> ParamSpec {
        ParamSpec::number(self.0, name, min, max, default)
    }
}

impl ChoiceParam {
    /// The spec of this parameter: one of `options`.
    pub fn spec<S: Into<String>>(
        self,
        name: impl Into<String>,
        options: impl IntoIterator<Item = S>,
        default: usize,
    ) -> ParamSpec {
        ParamSpec::choice(self.0, name, options, default)
    }

    /// The [`TextKey`] of option `index`, for translations.
    pub fn option_key(self, index: usize) -> TextKey {
        TextKey::option(self.0, index)
    }
}

impl ToggleParam {
    /// The spec of this parameter: on or off.
    pub fn spec(self, name: impl Into<String>, default: bool) -> ParamSpec {
        ParamSpec::toggle(self.0, name, default)
    }
}

impl ParamKey for NumberParam {
    type Value = f32;

    fn id(self) -> &'static str {
        self.0
    }

    fn read(self, value: ParamValue) -> Option<f32> {
        value.as_number()
    }
}

impl ParamKey for ChoiceParam {
    type Value = usize;

    fn id(self) -> &'static str {
        self.0
    }

    fn read(self, value: ParamValue) -> Option<usize> {
        value.as_choice()
    }
}

impl ParamKey for ToggleParam {
    type Value = bool;

    fn id(self) -> &'static str {
        self.0
    }

    fn read(self, value: ParamValue) -> Option<bool> {
        value.as_toggle()
    }
}

impl ParamSpec {
    pub fn number(
        id: impl Into<String>,
        name: impl Into<String>,
        min: f32,
        max: f32,
        default: f32,
    ) -> Self {
        Self::new(id, name, ParamKind::Number { min, max, default })
    }

    pub fn choice<S: Into<String>>(
        id: impl Into<String>,
        name: impl Into<String>,
        options: impl IntoIterator<Item = S>,
        default: usize,
    ) -> Self {
        let options = options.into_iter().map(Into::into).collect();
        Self::new(id, name, ParamKind::Choice { options, default })
    }

    pub fn toggle(id: impl Into<String>, name: impl Into<String>, default: bool) -> Self {
        Self::new(id, name, ParamKind::Toggle { default })
    }

    fn new(id: impl Into<String>, name: impl Into<String>, kind: ParamKind) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            kind,
        }
    }

    /// The value a new node starts with.
    pub fn default_value(&self) -> ParamValue {
        match &self.kind {
            ParamKind::Number { default, .. } => ParamValue::Number(*default),
            ParamKind::Choice { default, .. } => ParamValue::Choice(*default),
            ParamKind::Toggle { default } => ParamValue::Toggle(*default),
        }
    }

    /// Whether `value` has the right variant and lies in range.
    pub fn accepts(&self, value: ParamValue) -> bool {
        match (&self.kind, value) {
            (ParamKind::Number { min, max, .. }, ParamValue::Number(value)) => {
                (*min..=*max).contains(&value)
            }
            (ParamKind::Choice { options, .. }, ParamValue::Choice(index)) => index < options.len(),
            (ParamKind::Toggle { .. }, ParamValue::Toggle(_)) => true,
            _ => false,
        }
    }

    /// Checks the id and name aren't empty, a number range is finite and
    /// ordered, a choice has options, and the default is accepted.
    pub fn validate(&self) -> Result<(), ParamProblem> {
        if self.id.trim().is_empty() {
            return Err(ParamProblem::EmptyId);
        }
        if self.name.trim().is_empty() {
            return Err(ParamProblem::EmptyName);
        }
        if let ParamKind::Number { min, max, .. } = self.kind
            && !(min.is_finite() && max.is_finite() && min <= max)
        {
            return Err(ParamProblem::BadRange);
        }
        if let ParamKind::Choice { options, .. } = &self.kind
            && options.is_empty()
        {
            return Err(ParamProblem::NoOptions);
        }
        if !self.accepts(self.default_value()) {
            return Err(ParamProblem::DefaultOutOfRange);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_ranges() {
        let gain = ParamSpec::number("gain", "Gain", 0.0, 2.0, 1.0);
        assert_eq!(gain.default_value(), ParamValue::Number(1.0));
        assert!(gain.accepts(ParamValue::Number(2.0)));
        assert!(!gain.accepts(ParamValue::Number(2.5)));
        assert!(!gain.accepts(ParamValue::Toggle(true)));
        assert!(gain.validate().is_ok());

        let mode = ParamSpec::choice("mode", "Mode", ["a", "b"], 1);
        assert_eq!(mode.default_value(), ParamValue::Choice(1));
        assert!(!mode.accepts(ParamValue::Choice(2)));
    }

    #[test]
    fn keys_build_specs_and_read_their_kind() {
        const MODE: ChoiceParam = ChoiceParam("mode");
        const ON: ToggleParam = ToggleParam("on");

        assert_eq!(
            MODE.spec("Mode", ["a", "b"], 1),
            ParamSpec::choice("mode", "Mode", ["a", "b"], 1)
        );
        assert_eq!(MODE.read(ParamValue::Choice(1)), Some(1));
        assert_eq!(MODE.read(ParamValue::Number(1.0)), None);
        assert_eq!(MODE.option_key(1), TextKey::option("mode", 1));
        assert_eq!(
            ON.spec("On", true).default_value(),
            ParamValue::Toggle(true)
        );
        assert_eq!(ON.read(ParamValue::Toggle(false)), Some(false));
        assert_eq!(ON.name_key(), TextKey::param("on"));
    }

    #[test]
    fn rejects_invalid_params() {
        for (param, problem) in [
            (
                ParamSpec::number("x", "X", 1.0, 0.0, 0.5),
                ParamProblem::BadRange,
            ),
            (
                ParamSpec::number("x", "X", 0.0, f32::INFINITY, 0.5),
                ParamProblem::BadRange,
            ),
            (
                ParamSpec::number("x", "X", 0.0, 1.0, 2.0),
                ParamProblem::DefaultOutOfRange,
            ),
            (
                ParamSpec::choice("x", "X", Vec::<String>::new(), 0),
                ParamProblem::NoOptions,
            ),
            (
                ParamSpec::choice("x", "X", ["a"], 1),
                ParamProblem::DefaultOutOfRange,
            ),
            (ParamSpec::toggle("", "X", false), ParamProblem::EmptyId),
        ] {
            assert_eq!(param.validate(), Err(problem), "{param:?}");
        }
    }
}
