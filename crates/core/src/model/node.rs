//! Nodes: instances of a node type ([`NodeSpec`]) with their own parameter values.

use std::sync::Arc;

use crate::{Error, NodeSocket, NodeSpec, ParamValue, SocketDirection};

/// One node: the spec it was built from and its current parameter values. Its id
/// is given by the [`Graph`](crate::Graph) that holds it.
///
/// Everything that comes from the type (name, sockets, parameter specs) is read
/// from the shared spec, so building a node copies nothing but the parameter
/// values. The values always fit the spec: they start at the defaults and change
/// only through [`set_param`](Self::set_param).
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    spec: Arc<NodeSpec>,
    /// Current parameter values, in the order of the spec's
    /// [`params`](NodeSpec::params).
    params: Vec<ParamValue>,
}

impl Node {
    /// A node of type `spec`, with every parameter at its default.
    pub fn new(spec: Arc<NodeSpec>) -> Self {
        let params = spec.default_params();
        Self { spec, params }
    }

    /// The node's type.
    pub fn spec(&self) -> &Arc<NodeSpec> {
        &self.spec
    }

    /// Type id of the node's spec.
    pub fn type_id(&self) -> &str {
        &self.spec.type_id
    }

    /// The node's sockets on `direction`. The spec's for now; read them here, not
    /// from the spec, so nodes whose settings change their sockets fit in later.
    pub fn sockets(&self, direction: SocketDirection) -> &[NodeSocket] {
        self.spec.sockets(direction)
    }

    /// Current parameter values, in the order of the spec's
    /// [`params`](NodeSpec::params).
    pub fn params(&self) -> &[ParamValue] {
        &self.params
    }

    /// The value of parameter `id`, if the spec has it.
    pub fn param(&self, id: &str) -> Option<ParamValue> {
        self.spec.param_index(id).map(|index| self.params[index])
    }

    /// Sets parameter `id` to `value`. Fails, leaving the node unchanged, if the
    /// spec has no such parameter or doesn't accept the value
    /// ([`ParamSpec::accepts`](crate::ParamSpec::accepts)).
    pub fn set_param(&mut self, id: &str, value: ParamValue) -> Result<(), Error> {
        let type_id = || self.type_id().to_owned();
        let Some(index) = self.spec.param_index(id) else {
            return Err(Error::UnknownParam {
                type_id: type_id(),
                param: id.to_owned(),
            });
        };
        if !self.spec.params[index].accepts(value) {
            return Err(Error::ParamRejected {
                type_id: type_id(),
                param: id.to_owned(),
            });
        }
        self.params[index] = value;
        Ok(())
    }
}

/// The category of a node type in a signal chain: where the editor lists it and
/// its default color. Only loosely tied to the sockets (see
/// [`NodeSpec::validate`]): a generator may take modulation inputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NodeKind {
    /// A node that generates a signal.
    Generator,
    /// A node that processes a signal.
    Processor,
    /// A node that outputs a signal.
    Output,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ParamSpec;

    fn gain() -> Arc<NodeSpec> {
        Arc::new(
            NodeSpec::new("test.gain", "Gain", NodeKind::Processor)
                .input(NodeSocket::signal_in("in"))
                .output(NodeSocket::signal_out("out"))
                .param(ParamSpec::number("gain", "Gain", 0.0, 2.0, 1.0))
                .param(ParamSpec::toggle("on", "On", true)),
        )
    }

    #[test]
    fn starts_at_the_defaults_and_shares_the_spec() {
        let spec = gain();
        let node = Node::new(Arc::clone(&spec));

        assert_eq!(node.type_id(), "test.gain");
        assert!(Arc::ptr_eq(node.spec(), &spec));
        assert_eq!(
            node.params(),
            [ParamValue::Number(1.0), ParamValue::Toggle(true)]
        );
    }

    #[test]
    fn sets_only_accepted_values() {
        let mut node = Node::new(gain());

        node.set_param("gain", ParamValue::Number(2.0)).unwrap();
        assert_eq!(node.param("gain"), Some(ParamValue::Number(2.0)));

        assert!(matches!(
            node.set_param("gain", ParamValue::Number(3.0)),
            Err(Error::ParamRejected { .. })
        ));
        assert!(matches!(
            node.set_param("gain", ParamValue::Toggle(false)),
            Err(Error::ParamRejected { .. })
        ));
        assert!(matches!(
            node.set_param("missing", ParamValue::Toggle(false)),
            Err(Error::UnknownParam { .. })
        ));
        assert_eq!(node.param("gain"), Some(ParamValue::Number(2.0)));
        assert_eq!(node.param("missing"), None);
    }
}
