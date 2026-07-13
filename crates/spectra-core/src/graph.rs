//! Domain-neutral, packed graph primitives.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::{Error, Result};

#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
pub struct NodeId(pub u32);

#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
pub struct EdgeId(pub u32);

#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
pub struct AtomId(pub u32);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Scalar {
    Bool(bool),
    I64(i64),
    F64(f64),
    Atom(AtomId),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Attribute {
    pub key: AtomId,
    pub value: Scalar,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub kind: AtomId,
    pub label: AtomId,
    #[serde(default)]
    pub attributes: Vec<Attribute>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub id: EdgeId,
    pub source: NodeId,
    pub target: NodeId,
    pub kind: AtomId,
    #[serde(default)]
    pub attributes: Vec<Attribute>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AtomTable {
    values: Vec<String>,
    #[serde(skip)]
    lookup: HashMap<String, AtomId>,
}

impl AtomTable {
    pub fn intern(&mut self, value: impl AsRef<str>) -> AtomId {
        let value = value.as_ref();
        if let Some(id) = self.lookup.get(value) {
            return *id;
        }
        if self.lookup.is_empty() && !self.values.is_empty() {
            self.rebuild_lookup();
        }
        if let Some(id) = self.lookup.get(value) {
            return *id;
        }
        let id = AtomId(self.values.len() as u32);
        self.values.push(value.to_owned());
        self.lookup.insert(value.to_owned(), id);
        id
    }

    pub fn resolve(&self, id: AtomId) -> Option<&str> {
        self.values.get(id.0 as usize).map(String::as_str)
    }

    pub fn rebuild_lookup(&mut self) {
        self.lookup = self
            .values
            .iter()
            .enumerate()
            .map(|(index, value)| (value.clone(), AtomId(index as u32)))
            .collect();
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PackedGraph {
    pub atoms: AtomTable,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    #[serde(skip)]
    outgoing: Vec<Vec<EdgeId>>,
    #[serde(skip)]
    incoming: Vec<Vec<EdgeId>>,
}

impl PackedGraph {
    pub fn add_node(&mut self, kind: &str, label: &str) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        let kind = self.atoms.intern(kind);
        let label = self.atoms.intern(label);
        self.nodes.push(Node {
            id,
            kind,
            label,
            attributes: Vec::new(),
        });
        self.outgoing.push(Vec::new());
        self.incoming.push(Vec::new());
        id
    }

    pub fn add_edge(&mut self, source: NodeId, target: NodeId, kind: &str) -> Result<EdgeId> {
        if self.node(source).is_none() || self.node(target).is_none() {
            return Err(Error::Graph(format!(
                "edge endpoint out of range: {} -> {}",
                source.0, target.0
            )));
        }
        let id = EdgeId(self.edges.len() as u32);
        let kind = self.atoms.intern(kind);
        self.edges.push(Edge {
            id,
            source,
            target,
            kind,
            attributes: Vec::new(),
        });
        self.outgoing[source.0 as usize].push(id);
        self.incoming[target.0 as usize].push(id);
        Ok(id)
    }

    pub fn add_node_attribute(&mut self, node: NodeId, key: &str, value: Scalar) -> Result<()> {
        let key = self.atoms.intern(key);
        let Some(node) = self.nodes.get_mut(node.0 as usize) else {
            return Err(Error::Graph(format!("node {} is out of range", node.0)));
        };
        node.attributes.push(Attribute { key, value });
        Ok(())
    }

    pub fn add_edge_attribute(&mut self, edge: EdgeId, key: &str, value: Scalar) -> Result<()> {
        let key = self.atoms.intern(key);
        let Some(edge) = self.edges.get_mut(edge.0 as usize) else {
            return Err(Error::Graph(format!("edge {} is out of range", edge.0)));
        };
        edge.attributes.push(Attribute { key, value });
        Ok(())
    }

    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id.0 as usize)
    }
    pub fn edge(&self, id: EdgeId) -> Option<&Edge> {
        self.edges.get(id.0 as usize)
    }
    pub fn atom(&self, id: AtomId) -> &str {
        self.atoms.resolve(id).unwrap_or("<invalid>")
    }
    pub fn label(&self, id: NodeId) -> &str {
        self.node(id)
            .map(|n| self.atom(n.label))
            .unwrap_or("<invalid>")
    }
    pub fn kind(&self, id: NodeId) -> &str {
        self.node(id)
            .map(|n| self.atom(n.kind))
            .unwrap_or("<invalid>")
    }
    pub fn edge_kind(&self, id: EdgeId) -> &str {
        self.edge(id)
            .map(|e| self.atom(e.kind))
            .unwrap_or("<invalid>")
    }
    pub fn outgoing(&self, id: NodeId) -> &[EdgeId] {
        self.outgoing
            .get(id.0 as usize)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
    pub fn incoming(&self, id: NodeId) -> &[EdgeId] {
        self.incoming
            .get(id.0 as usize)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn rebuild_indexes(&mut self) -> Result<()> {
        self.atoms.rebuild_lookup();
        self.outgoing = vec![Vec::new(); self.nodes.len()];
        self.incoming = vec![Vec::new(); self.nodes.len()];
        self.validate()?;
        for edge in &self.edges {
            self.outgoing[edge.source.0 as usize].push(edge.id);
            self.incoming[edge.target.0 as usize].push(edge.id);
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        for (index, node) in self.nodes.iter().enumerate() {
            if node.id.0 as usize != index {
                return Err(Error::Graph("node IDs must be contiguous".into()));
            }
            if self.atoms.resolve(node.kind).is_none() || self.atoms.resolve(node.label).is_none() {
                return Err(Error::Graph(format!("node {index} has an invalid atom")));
            }
        }
        for (index, edge) in self.edges.iter().enumerate() {
            if edge.id.0 as usize != index {
                return Err(Error::Graph("edge IDs must be contiguous".into()));
            }
            if self.node(edge.source).is_none() || self.node(edge.target).is_none() {
                return Err(Error::Graph(format!(
                    "edge {index} has an invalid endpoint"
                )));
            }
            if self.atoms.resolve(edge.kind).is_none() {
                return Err(Error::Graph(format!("edge {index} has an invalid kind")));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_graph_round_trips_and_rebuilds_adjacency() {
        let mut graph = PackedGraph::default();
        let a = graph.add_node("thing", "a");
        let b = graph.add_node("thing", "b");
        graph.add_edge(a, b, "links").unwrap();
        let json = serde_json::to_string(&graph).unwrap();
        let mut decoded: PackedGraph = serde_json::from_str(&json).unwrap();
        decoded.rebuild_indexes().unwrap();
        assert_eq!(decoded.outgoing(a), &[EdgeId(0)]);
        assert_eq!(decoded.incoming(b), &[EdgeId(0)]);
    }

    #[test]
    fn rejects_invalid_endpoints() {
        let mut graph = PackedGraph::default();
        let a = graph.add_node("thing", "a");
        assert!(graph.add_edge(a, NodeId(9), "links").is_err());
    }
}
