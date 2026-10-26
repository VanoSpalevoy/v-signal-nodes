//! Saving and loading documents: the file format of a node graph and its
//! (de)serialization.
//!
//! A [`Document`] is plain data, independent of the editor and egui: node types
//! are referenced by plugin type id, parameters by id, sockets by 0-based index,
//! positions are canvas coordinates. Nothing derived (routes) or transient
//! (selection, drag, zoom) is stored. The editor converts its state to and from a
//! document (`VSignalNodesApp::to_document` / `open_document`) and checks the
//! plugins and every graph rule there; this module only checks the format.
//!
//! On disk a document is pretty-printed JSON:
//!
//! ```json
//! {
//!   "version": 1,
//!   "used_plugins": ["std"],
//!   "nodes": [
//!     { "id": 0, "type_id": "std.gain", "pos": [40.0, 60.0], "params": { "gain": { "number": 0.5 } } }
//!   ],
//!   "connections": [
//!     { "from_node": 0, "from_output": 0, "to_node": 1, "to_input": 0 }
//!   ]
//! }
//! ```

#![allow(dead_code, reason = "no Save/Open commands in the UI yet")]

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use v_signal_nodes_core::{AppError, ParamValue};

/// Version of the file format written by this build. Bump it on any incompatible
/// change and migrate older versions in [`Document::from_json`].
pub(crate) const FORMAT_VERSION: u32 = 1;

/// Largest node coordinate a document may hold (either sign). Far beyond any
/// real layout, but small enough for the canvas math in `f32` (grid, routing) to
/// stay exact to well under a canvas unit.
const MAX_COORDINATE: f32 = 1_000_000.0;

/// A saved node graph.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Document {
    /// File format version, [`FORMAT_VERSION`] when written by this build.
    pub version: u32,
    /// Names of the plugins whose node types the document uses, sorted. Checked
    /// on load before any node is built, so a missing plugin is reported by name.
    /// (Plugins have no versions yet; when they do, they go here too.)
    #[serde(default)]
    pub used_plugins: Vec<String>,
    /// Nodes in paint order: later nodes are drawn on top.
    pub nodes: Vec<DocNode>,
    /// Connections, sorted (see [`Document::new`]) so saving an unchanged graph
    /// gives the same file.
    pub connections: Vec<DocConnection>,
}

/// One node of a [`Document`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocNode {
    /// Node ID, unique within the document.
    pub id: usize,
    /// Type id of the plugin node spec, e.g. `"std.gain"`.
    pub type_id: String,
    /// Top-left corner, canvas coordinates.
    pub pos: [f32; 2],
    /// Parameter values by parameter id. Parameters left out get their default.
    #[serde(default)]
    pub params: BTreeMap<String, ParamValue>,
}

/// A connection from output `from_output` of node `from_node` to input
/// `to_input` of node `to_node` (0-based socket indices).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocConnection {
    pub from_node: usize,
    pub from_output: usize,
    pub to_node: usize,
    pub to_input: usize,
}

impl Document {
    /// A document of the current [`FORMAT_VERSION`]. `nodes` keep their order
    /// (paint order); `used_plugins` and `connections` are sorted here, plugins
    /// also deduplicated.
    pub(crate) fn new(
        mut used_plugins: Vec<String>,
        nodes: Vec<DocNode>,
        mut connections: Vec<DocConnection>,
    ) -> Self {
        used_plugins.sort_unstable();
        used_plugins.dedup();
        connections.sort_unstable();
        Self {
            version: FORMAT_VERSION,
            used_plugins,
            nodes,
            connections,
        }
    }

    /// The document as pretty-printed JSON.
    pub(crate) fn to_json(&self) -> String {
        // Plain data and string-keyed maps only, so this can't fail. (A NaN would
        // become `null` and be refused on load.)
        serde_json::to_string_pretty(self).expect("a document serializes to JSON")
    }

    /// Parses a document from JSON. Refuses malformed JSON, unknown fields,
    /// unsupported format versions, non-finite positions or numbers and positions
    /// beyond [`MAX_COORDINATE`]. Plugins and graph rules (known types, sockets,
    /// cycles) are checked by the editor.
    pub(crate) fn from_json(json: &str) -> Result<Self, AppError> {
        let document: Self = serde_json::from_str(json)
            .map_err(|err| AppError::InvalidInput(format!("not a valid document: {err}")))?;
        if document.version != FORMAT_VERSION {
            return Err(AppError::InvalidInput(format!(
                "unsupported document version {} (expected {FORMAT_VERSION})",
                document.version
            )));
        }
        for node in &document.nodes {
            let numbers = node.params.values().filter_map(|value| match value {
                ParamValue::Number(value) => Some(*value),
                _ => None,
            });
            if !node.pos.into_iter().chain(numbers).all(f32::is_finite) {
                return Err(AppError::InvalidInput(format!(
                    "node {} has a non-finite number",
                    node.id
                )));
            }
            if node
                .pos
                .iter()
                .any(|coordinate| coordinate.abs() > MAX_COORDINATE)
            {
                return Err(AppError::InvalidInput(format!(
                    "node {} is placed beyond {MAX_COORDINATE} canvas units",
                    node.id
                )));
            }
        }
        Ok(document)
    }
}

/// Writes `document` to `path`, replacing the file there (asking before
/// overwriting is up to the UI). Writes a temporary file next to it first
/// (`path` + `.tmp`, so `song.json` → `song.json.tmp`, never touching an
/// unrelated `song.tmp`) and renames it over `path`, so a failed save leaves the
/// old file intact.
pub(crate) fn save_document(document: &Document, path: impl AsRef<Path>) -> Result<(), AppError> {
    let path = path.as_ref();
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    let temp = std::path::PathBuf::from(temp);
    fs::write(&temp, document.to_json())?;
    fs::rename(&temp, path).inspect_err(|_| {
        let _ = fs::remove_file(&temp);
    })?;
    Ok(())
}

/// Reads a document from `path` (see [`Document::from_json`]).
pub(crate) fn load_document(path: impl AsRef<Path>) -> Result<Document, AppError> {
    Document::from_json(&fs::read_to_string(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Document {
        Document::new(
            vec!["std".into(), "stub".into(), "std".into()],
            vec![
                DocNode {
                    id: 3,
                    type_id: "std.source".into(),
                    pos: [40.0, 60.0],
                    params: BTreeMap::new(),
                },
                DocNode {
                    id: 0,
                    type_id: "std.gain".into(),
                    pos: [300.0, 60.0],
                    params: BTreeMap::from([
                        ("gain".into(), ParamValue::Number(0.5)),
                        ("mode".into(), ParamValue::Choice(1)),
                        ("on".into(), ParamValue::Toggle(false)),
                    ]),
                },
            ],
            vec![
                DocConnection {
                    from_node: 3,
                    from_output: 0,
                    to_node: 0,
                    to_input: 1,
                },
                DocConnection {
                    from_node: 3,
                    from_output: 0,
                    to_node: 0,
                    to_input: 0,
                },
            ],
        )
    }

    #[test]
    fn new_sorts_plugins_and_connections() {
        let document = sample();
        assert_eq!(document.version, FORMAT_VERSION);
        assert_eq!(document.used_plugins, ["std", "stub"]);
        assert_eq!(document.connections[0].to_input, 0);
        assert_eq!(document.connections[1].to_input, 1);
    }

    #[test]
    fn json_round_trip() {
        let document = sample();
        let json = document.to_json();
        assert_eq!(Document::from_json(&json).unwrap(), document);
        // Stable output: the same document always gives the same text.
        assert_eq!(Document::from_json(&json).unwrap().to_json(), json);
    }

    #[test]
    fn params_and_plugins_may_be_omitted() {
        let json = r#"{
            "version": 1,
            "nodes": [
                { "id": 0, "type_id": "std.gain", "pos": [0, 0], "params": { "gain": { "number": 2.0 } } },
                { "id": 1, "type_id": "std.sink", "pos": [200, 0] }
            ],
            "connections": []
        }"#;
        let document = Document::from_json(json).unwrap();
        assert!(document.used_plugins.is_empty());
        assert_eq!(document.nodes[0].params["gain"], ParamValue::Number(2.0));
        assert!(document.nodes[1].params.is_empty());
    }

    #[test]
    fn rejects_malformed_documents() {
        let refused = |json: &str| {
            assert!(
                matches!(Document::from_json(json), Err(AppError::InvalidInput(_))),
                "accepted {json}"
            );
        };
        refused("");
        refused("[]");
        refused(r#"{ "version": 1, "nodes": [] }"#);
        refused(r#"{ "version": 2, "nodes": [], "connections": [] }"#);
        refused(r#"{ "version": 1, "nodes": [], "connections": [], "extra": 0 }"#);
        refused(
            r#"{ "version": 1, "connections": [],
                 "nodes": [{ "id": 0, "type_id": "std.gain", "pos": [null, 0] }] }"#,
        );
        refused(
            r#"{ "version": 1, "connections": [],
                 "nodes": [{ "id": 0, "type_id": "std.gain", "pos": [0, 0],
                             "params": { "gain": { "percent": 5 } } }] }"#,
        );
        refused(
            r#"{ "version": 1, "connections": [],
                 "nodes": [{ "id": 0, "type_id": "std.gain", "pos": [0, -1e9] }] }"#,
        );
    }

    #[test]
    fn saves_and_loads_a_file() {
        let dir = std::env::temp_dir().join(format!("v-signal-nodes-doc-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("graph.json");
        // Someone else's file that merely shares the stem: must survive the save.
        let unrelated = path.with_extension("tmp");
        fs::write(&unrelated, "keep me").unwrap();
        let document = sample();

        save_document(&document, &path).unwrap();
        assert_eq!(load_document(&path).unwrap(), document);
        assert!(!dir.join("graph.json.tmp").exists());
        assert_eq!(fs::read_to_string(&unrelated).unwrap(), "keep me");
        assert!(matches!(
            load_document(dir.join("missing.json")),
            Err(AppError::Io(_))
        ));

        fs::remove_dir_all(&dir).unwrap();
    }
}
