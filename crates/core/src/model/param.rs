//! Node parameters: declared by a [`NodeSpec`](super::NodeSpec), stored per node.

use crate::AppError;

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
    pub fn validate(&self) -> Result<(), AppError> {
        let invalid = |message: &str| {
            Err(AppError::InvalidInput(format!(
                "parameter '{}': {message}",
                self.id
            )))
        };
        if self.id.trim().is_empty() {
            return invalid("empty id");
        }
        if self.name.trim().is_empty() {
            return invalid("empty name");
        }
        if let ParamKind::Number { min, max, .. } = self.kind
            && !(min.is_finite() && max.is_finite() && min <= max)
        {
            return invalid("range must be finite with min <= max");
        }
        if let ParamKind::Choice { options, .. } = &self.kind
            && options.is_empty()
        {
            return invalid("no options");
        }
        if !self.accepts(self.default_value()) {
            return invalid("default out of range");
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
    fn rejects_invalid_params() {
        for param in [
            ParamSpec::number("x", "X", 1.0, 0.0, 0.5),
            ParamSpec::number("x", "X", 0.0, f32::INFINITY, 0.5),
            ParamSpec::number("x", "X", 0.0, 1.0, 2.0),
            ParamSpec::choice("x", "X", Vec::<String>::new(), 0),
            ParamSpec::choice("x", "X", ["a"], 1),
            ParamSpec::toggle("", "X", false),
        ] {
            assert!(param.validate().is_err(), "{param:?}");
        }
    }
}
