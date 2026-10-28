//! Building nodes from plugin-provided specs.
//!
//! A plugin describes each node type it offers with a [`NodeSpec`]. Specs are
//! registered in a [`NodeFactory`], validated once, and then used to stamp out
//! [`Node`] instances, which share the spec instead of copying it. The spec lives
//! in `core` (not in `plugin-api`) so both the plugin API and the host can use it
//! without a dependency cycle.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use crate::AppError;
use crate::model::{
    MAX_CHANNELS, Node, NodeKind, NodeSocket, ParamKind, ParamSpec, ParamValue, SocketDirection,
};

/// The description of one node type, as provided by a plugin.
///
/// All labels (`name`, socket names, parameter names and choice options) are the
/// English defaults; [`translations`](Self::translations) can provide them in
/// other languages, addressed by [`TextKey`].
#[derive(Clone, Debug, PartialEq)]
pub struct NodeSpec {
    /// Globally unique type id, namespaced by the plugin, e.g. `"std.gain"`.
    pub type_id: String,
    /// Display name.
    pub name: String,
    /// Category: where the editor lists the node and its default color.
    pub kind: NodeKind,
    /// Color for the node in the editor instead of its kind's; should be bright
    /// enough for dark text on it.
    pub color: Option<Rgb>,
    /// Input sockets; each must have [`SocketDirection::Input`].
    pub inputs: Vec<NodeSocket>,
    /// Output sockets; each must have [`SocketDirection::Output`].
    pub outputs: Vec<NodeSocket>,
    /// Parameters; ids must be unique.
    pub params: Vec<ParamSpec>,
    /// Translations of the labels above.
    pub translations: Vec<Translation>,
}

/// Which label of a [`NodeSpec`] a [`Translation`] is for.
///
/// Labels are addressed by their role and position or id, not by their English
/// text, so editing the English label keeps its translations, and the same English
/// word in two roles (a node and a parameter named "Gain") can be translated
/// differently. Sockets and choice options are addressed by index, which is
/// already their identity: connections and parameter values refer to them that way.
///
/// `S` is the parameter id: owned (`String`, the default) when stored in a
/// [`Translation`], borrowed (`&str`, see [`borrowed`](Self::borrowed)) for
/// lookups such as [`NodeSpec::label`], which then allocate nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextKey<S = String> {
    /// The node type's [`name`](NodeSpec::name).
    Name,
    /// Name of input socket `index`.
    Input(usize),
    /// Name of output socket `index`.
    Output(usize),
    /// Name of the parameter with this [`id`](ParamSpec::id).
    Param(S),
    /// Option `index` of the choice parameter with this id.
    Option(S, usize),
}

impl TextKey {
    /// [`Param`](Self::Param) `id`.
    pub fn param(id: impl Into<String>) -> Self {
        Self::Param(id.into())
    }

    /// [`Option`](Self::Option) `index` of parameter `id`.
    pub fn option(id: impl Into<String>, index: usize) -> Self {
        Self::Option(id.into(), index)
    }
}

impl<S> TextKey<S> {
    /// Name of socket `index` on the `direction` side.
    pub fn socket(direction: SocketDirection, index: usize) -> Self {
        match direction {
            SocketDirection::Input => Self::Input(index),
            SocketDirection::Output => Self::Output(index),
        }
    }
}

impl<S: AsRef<str>> TextKey<S> {
    /// The same key with a borrowed parameter id.
    pub fn borrowed(&self) -> TextKey<&str> {
        match self {
            Self::Name => TextKey::Name,
            Self::Input(index) => TextKey::Input(*index),
            Self::Output(index) => TextKey::Output(*index),
            Self::Param(id) => TextKey::Param(id.as_ref()),
            Self::Option(id, index) => TextKey::Option(id.as_ref(), *index),
        }
    }
}

/// An sRGB color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// A label of a [`NodeSpec`] in another language.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Translation {
    /// Language code, e.g. `"ru"`.
    pub language: String,
    /// The label being translated.
    pub key: TextKey,
    /// The label in `language`.
    pub text: String,
}

impl NodeSpec {
    /// A spec with no sockets, parameters or translations; add them with the
    /// builder methods ([`input`](Self::input), [`param`](Self::param), ...).
    pub fn new(type_id: impl Into<String>, name: impl Into<String>, kind: NodeKind) -> Self {
        Self {
            type_id: type_id.into(),
            name: name.into(),
            kind,
            color: None,
            inputs: Vec::new(),
            outputs: Vec::new(),
            params: Vec::new(),
            translations: Vec::new(),
        }
    }

    /// Sets the node's own color.
    pub fn color(mut self, color: Rgb) -> Self {
        self.color = Some(color);
        self
    }

    /// Adds an input socket.
    pub fn input(mut self, socket: NodeSocket) -> Self {
        self.inputs.push(socket);
        self
    }

    /// Adds an output socket.
    pub fn output(mut self, socket: NodeSocket) -> Self {
        self.outputs.push(socket);
        self
    }

    /// Adds a parameter.
    pub fn param(mut self, param: ParamSpec) -> Self {
        self.params.push(param);
        self
    }

    /// Adds translations of labels into `language`, as `(key, text)` pairs.
    pub fn translate<'a>(
        mut self,
        language: &str,
        pairs: impl IntoIterator<Item = (TextKey, &'a str)>,
    ) -> Self {
        self.translations
            .extend(pairs.into_iter().map(|(key, text)| Translation {
                language: language.to_owned(),
                key,
                text: text.to_owned(),
            }));
        self
    }

    /// The sockets on the `direction` side: [`inputs`](Self::inputs) or
    /// [`outputs`](Self::outputs).
    pub fn sockets(&self, direction: SocketDirection) -> &[NodeSocket] {
        match direction {
            SocketDirection::Input => &self.inputs,
            SocketDirection::Output => &self.outputs,
        }
    }

    /// Position of parameter `id` in [`params`](Self::params).
    pub fn param_index(&self, id: &str) -> Option<usize> {
        self.params.iter().position(|param| param.id == id)
    }

    /// Parameter `id`.
    pub fn find_param(&self, id: &str) -> Option<&ParamSpec> {
        self.params.iter().find(|param| param.id == id)
    }

    /// The values a new node starts with, in [`params`](Self::params) order.
    pub fn default_params(&self) -> Vec<ParamValue> {
        self.params.iter().map(ParamSpec::default_value).collect()
    }

    /// Every label of the spec: the name, the inputs and outputs, then each
    /// parameter followed by its choice options.
    pub fn text_keys(&self) -> impl Iterator<Item = TextKey<&str>> {
        let sockets = (0..self.inputs.len())
            .map(TextKey::Input)
            .chain((0..self.outputs.len()).map(TextKey::Output));
        let params = self.params.iter().flat_map(|param| {
            let id = param.id.as_str();
            let options = match &param.kind {
                ParamKind::Choice { options, .. } => options.len(),
                _ => 0,
            };
            std::iter::once(TextKey::Param(id))
                .chain((0..options).map(move |index| TextKey::Option(id, index)))
        });
        std::iter::once(TextKey::Name).chain(sockets).chain(params)
    }

    /// The English label `key` refers to, or `None` if there's no such label.
    pub fn english(&self, key: TextKey<&str>) -> Option<&str> {
        match key {
            TextKey::Name => Some(&self.name),
            TextKey::Input(index) => self.inputs.get(index).map(|socket| socket.name.as_str()),
            TextKey::Output(index) => self.outputs.get(index).map(|socket| socket.name.as_str()),
            TextKey::Param(id) => self.find_param(id).map(|param| param.name.as_str()),
            TextKey::Option(id, index) => match &self.find_param(id)?.kind {
                ParamKind::Choice { options, .. } => options.get(index).map(String::as_str),
                _ => None,
            },
        }
    }

    /// Label `key` in `language`, or in English when there's no translation; empty
    /// if the spec has no such label. Allocates nothing, so it's fine to call for
    /// every label on every frame.
    pub fn label(&self, language: &str, key: TextKey<&str>) -> &str {
        self.translations
            .iter()
            .find(|translation| {
                translation.key.borrowed() == key && translation.language == language
            })
            .map(|translation| translation.text.as_str())
            .or_else(|| self.english(key))
            .unwrap_or_default()
    }

    /// Checks the spec is usable: non-empty type id and name, sockets in the right
    /// list with `1..=MAX_CHANNELS` channels, socket names unique per direction,
    /// sockets that fit the kind (generators need an output, outputs an input,
    /// processors both; a generator may still have inputs, e.g. modulation),
    /// valid parameters with unique ids, and translations of existing labels, at
    /// most one per language and label.
    pub fn validate(&self) -> Result<(), AppError> {
        let invalid = |message: String| {
            Err(AppError::InvalidInput(format!(
                "node spec '{}': {message}",
                self.type_id
            )))
        };

        if self.type_id.trim().is_empty() {
            return invalid("empty type id".into());
        }
        if self.name.trim().is_empty() {
            return invalid("empty name".into());
        }

        for direction in [SocketDirection::Input, SocketDirection::Output] {
            let mut names = HashSet::new();
            for socket in self.sockets(direction) {
                if socket.direction != direction {
                    return invalid(format!(
                        "socket '{}' listed as {direction:?} but is {:?}",
                        socket.name, socket.direction
                    ));
                }
                if !names.insert(socket.name.as_str()) {
                    return invalid(format!("duplicate {direction:?} socket '{}'", socket.name));
                }
                if !(1..=MAX_CHANNELS).contains(&socket.channels) {
                    return invalid(format!(
                        "socket '{}' has {} channels, not 1..={MAX_CHANNELS}",
                        socket.name, socket.channels
                    ));
                }
            }
        }

        let (has_inputs, has_outputs) = (!self.inputs.is_empty(), !self.outputs.is_empty());
        let fits_kind = match self.kind {
            NodeKind::Generator => has_outputs,
            NodeKind::Processor => has_inputs && has_outputs,
            NodeKind::Output => has_inputs,
        };
        if !fits_kind {
            return invalid(format!(
                "{:?} node with {} input(s) and {} output(s)",
                self.kind,
                self.inputs.len(),
                self.outputs.len()
            ));
        }

        let mut ids = HashSet::new();
        for param in &self.params {
            if let Err(err) = param.validate() {
                return invalid(err.to_string());
            }
            if !ids.insert(param.id.as_str()) {
                return invalid(format!("duplicate parameter '{}'", param.id));
            }
        }

        let mut translated = HashSet::new();
        for translation in &self.translations {
            let key = translation.key.borrowed();
            if self.english(key).is_none() {
                return invalid(format!("translation of a missing label {key:?}"));
            }
            if !translated.insert((translation.language.as_str(), key)) {
                return invalid(format!(
                    "duplicate '{}' translation of {key:?}",
                    translation.language
                ));
            }
        }

        Ok(())
    }
}

/// Registry of node specs that builds [`Node`] instances.
///
/// Node ids are chosen by the caller (the graph owns id allocation), the factory
/// only attaches the spec. Specs are kept behind an [`Arc`], shared with every
/// node built from them and with whoever lists them.
#[derive(Debug, Default)]
pub struct NodeFactory {
    /// Registered specs by type id (sorted, so listings are stable).
    specs: BTreeMap<String, Arc<NodeSpec>>,
}

impl NodeFactory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Validates and registers `spec`. Fails if the spec is invalid or its type id
    /// is already taken.
    pub fn register(&mut self, spec: NodeSpec) -> Result<(), AppError> {
        spec.validate()?;
        match self.specs.entry(spec.type_id.clone()) {
            Entry::Occupied(_) => Err(already_registered(&spec.type_id)),
            Entry::Vacant(entry) => {
                entry.insert(Arc::new(spec));
                Ok(())
            }
        }
    }

    /// Moves every spec of `other` into this factory, all or nothing: fails
    /// without changing anything if one of its type ids is already taken. The
    /// specs were validated when `other` registered them.
    pub fn append(&mut self, other: NodeFactory) -> Result<(), AppError> {
        if let Some(type_id) = other.specs.keys().find(|id| self.specs.contains_key(*id)) {
            return Err(already_registered(type_id));
        }
        self.specs.extend(other.specs);
        Ok(())
    }

    /// The spec registered under `type_id`.
    pub fn spec(&self, type_id: &str) -> Option<&Arc<NodeSpec>> {
        self.specs.get(type_id)
    }

    /// All registered specs, sorted by type id.
    pub fn specs(&self) -> impl Iterator<Item = &Arc<NodeSpec>> {
        self.specs.values()
    }

    /// Builds a node of type `type_id`, with every parameter at its default.
    pub fn build_node(&self, type_id: &str) -> Result<Node, AppError> {
        let spec = self
            .spec(type_id)
            .ok_or_else(|| AppError::InvalidInput(format!("unknown node type '{type_id}'")))?;
        Ok(Node::new(Arc::clone(spec)))
    }
}

fn already_registered(type_id: &str) -> AppError {
    AppError::InvalidInput(format!("node type '{type_id}' is already registered"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gain() -> NodeSpec {
        NodeSpec::new("test.gain", "Gain", NodeKind::Processor)
            .input(NodeSocket::signal_in("in"))
            .output(NodeSocket::signal_out("out"))
            .param(ParamSpec::number("gain", "Gain", 0.0, 2.0, 1.0))
            .param(ParamSpec::choice("mode", "Mode", ["soft", "hard"], 0))
            .translate(
                "ru",
                [
                    (TextKey::Name, "Усилитель"),
                    (TextKey::param("gain"), "Усиление"),
                    (TextKey::option("mode", 1), "жёсткий"),
                ],
            )
    }

    #[test]
    fn builds_nodes_from_specs() {
        let mut factory = NodeFactory::new();
        factory.register(gain()).unwrap();

        let node = factory.build_node("test.gain").unwrap();

        assert_eq!(node.type_id(), "test.gain");
        assert_eq!(**node.spec(), gain());
        assert!(Arc::ptr_eq(node.spec(), factory.spec("test.gain").unwrap()));
        assert_eq!(
            node.params(),
            [ParamValue::Number(1.0), ParamValue::Choice(0)]
        );
    }

    #[test]
    fn labels_are_keyed_by_role_and_fall_back_to_english() {
        let spec = gain();

        // Same English text, different roles, different translations.
        assert_eq!(spec.label("ru", TextKey::Name), "Усилитель");
        assert_eq!(spec.label("ru", TextKey::Param("gain")), "Усиление");
        assert_eq!(spec.label("ru", TextKey::Option("mode", 1)), "жёсткий");
        assert_eq!(spec.label("ru", TextKey::Option("mode", 0)), "soft");
        assert_eq!(spec.label("ru", TextKey::Input(0)), "in");
        assert_eq!(spec.label("en", TextKey::Name), "Gain");
        assert_eq!(spec.label("ru", TextKey::Output(5)), "");

        // Renaming the English label keeps the translation.
        let renamed = NodeSpec {
            name: "Amplifier".into(),
            ..gain()
        };
        assert_eq!(renamed.label("ru", TextKey::Name), "Усилитель");
    }

    #[test]
    fn lists_every_label_and_finds_params() {
        let spec = gain();
        let keys: Vec<_> = spec.text_keys().collect();

        assert_eq!(
            keys,
            [
                TextKey::Name,
                TextKey::Input(0),
                TextKey::Output(0),
                TextKey::Param("gain"),
                TextKey::Param("mode"),
                TextKey::Option("mode", 0),
                TextKey::Option("mode", 1),
            ]
        );
        assert!(keys.iter().all(|&key| spec.english(key).is_some()));
        assert_eq!(spec.param_index("mode"), Some(1));
        assert_eq!(
            spec.find_param("gain").map(|param| param.id.as_str()),
            Some("gain")
        );
        assert_eq!(spec.param_index("missing"), None);
        assert_eq!(spec.sockets(SocketDirection::Output), spec.outputs);
        assert_eq!(
            TextKey::<&str>::socket(SocketDirection::Output, 2),
            TextKey::Output(2)
        );
    }

    #[test]
    fn rejects_duplicate_and_unknown_types() {
        let mut factory = NodeFactory::new();
        factory.register(gain()).unwrap();

        assert!(factory.register(gain()).is_err());
        assert!(factory.build_node("test.missing").is_err());
    }

    #[test]
    fn appends_all_or_nothing() {
        let mut factory = NodeFactory::new();
        factory.register(gain()).unwrap();

        let mut clashing = NodeFactory::new();
        clashing
            .register(NodeSpec {
                type_id: "test.amp".into(),
                ..gain()
            })
            .unwrap();
        clashing.register(gain()).unwrap();
        assert!(factory.append(clashing).is_err());
        assert!(factory.spec("test.amp").is_none());

        let mut other = NodeFactory::new();
        other
            .register(NodeSpec {
                type_id: "test.amp".into(),
                ..gain()
            })
            .unwrap();
        factory.append(other).unwrap();
        assert_eq!(factory.specs().count(), 2);
    }

    #[test]
    fn rejects_invalid_specs() {
        let wrong_direction = NodeSpec::new("t.a", "A", NodeKind::Processor)
            .input(NodeSocket::signal_out("in"))
            .output(NodeSocket::signal_out("out"));
        let duplicate_socket = gain().input(NodeSocket::signal_in("in"));
        let generator_without_output =
            NodeSpec::new("t.b", "B", NodeKind::Generator).input(NodeSocket::signal_in("in"));
        let no_channels = gain().output(NodeSocket::signal_out("silent").with_channels(0));
        let too_many_channels =
            gain().output(NodeSocket::signal_out("wide").with_channels(MAX_CHANNELS + 1));
        let empty_id = NodeSpec {
            type_id: " ".into(),
            ..gain()
        };
        let duplicate_param = gain().param(ParamSpec::toggle("gain", "On", true));
        let invalid_param = gain().param(ParamSpec::choice("pan", "Pan", ["a"], 3));
        let missing_label = gain().translate("ru", [(TextKey::option("gain", 0), "x")]);
        let duplicate_translation = gain().translate("ru", [(TextKey::Name, "Другое")]);

        for spec in [
            wrong_direction,
            duplicate_socket,
            generator_without_output,
            no_channels,
            too_many_channels,
            empty_id,
            duplicate_param,
            invalid_param,
            missing_label,
            duplicate_translation,
        ] {
            assert!(spec.validate().is_err(), "{spec:?}");
        }
        assert!(gain().validate().is_ok());
        // A generator with a modulation input, a colored stereo output.
        let lfo = NodeSpec::new("t.lfo", "LFO", NodeKind::Generator)
            .color(Rgb::new(200, 200, 90))
            .input(NodeSocket::control_in("rate"))
            .output(NodeSocket::signal_out("out").with_channels(2));
        assert!(lfo.validate().is_ok());
        // The same key in another language is fine.
        assert!(
            gain()
                .translate("de", [(TextKey::Name, "Verstärker")])
                .validate()
                .is_ok()
        );
    }
}
