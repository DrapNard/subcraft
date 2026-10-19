//! Bounded procedural source graphs for paint layers.
//!
//! Adapted from Kalium src/procedural.rs, AGPL-3.0-only.
//! Copyright DrapNard and the Kalium contributors; see LICENSE-AGPL-3.0.
//!
//! The first supported graph is UV-driven seeded value noise, optionally multiplied by the
//! uncomposited Occlusion raster of another paint layer in the same texture set, producing a
//! Roughness source. Graph references use stable layer UUIDs, never stack positions.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use subcraft_core::LayerId;

pub const MAX_GRAPH_NODES: usize = 64;
pub const MAX_GRAPH_EDGES: usize = 64;
pub const MIN_NOISE_SCALE: f32 = 0.01;
pub const MAX_NOISE_SCALE: f32 = 2048.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProceduralGraph {
    pub nodes: Vec<ProceduralNode>,
    pub edges: Vec<GraphEdge>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProceduralNode {
    pub id: u32,
    pub kind: NodeKind,
}

/// Supported value types in the constrained roughness graph.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum NodeKind {
    Uv,
    ValueNoise { seed: u64, scale: f32 },
    OcclusionInput { source_layer_id: LayerId },
    Multiply,
    RoughnessOutput,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphEdge {
    pub from: u32,
    pub to: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphError {
    NodeCount,
    EdgeCount,
    DuplicateNode,
    InvalidEdge,
    InvalidNoiseScale,
    InvalidSourceLayer,
    UnsupportedTopology,
    Cycle,
    InvalidUv,
    InvalidOcclusion,
    CoordinateOverflow,
}

impl std::fmt::Display for GraphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NodeCount => "procedural graph node count is outside the supported limit",
            Self::EdgeCount => "procedural graph edge count exceeds the supported limit",
            Self::DuplicateNode => "procedural graph contains duplicate node IDs",
            Self::InvalidEdge => "procedural graph contains an invalid or duplicate edge",
            Self::InvalidNoiseScale => "value noise scale is outside the finite supported range",
            Self::InvalidSourceLayer => "Occlusion graph input requires a nonzero source layer ID",
            Self::UnsupportedTopology => "graph is not a supported UV/value-noise/roughness graph",
            Self::Cycle => "procedural graph contains a cycle",
            Self::InvalidUv => "procedural graph UV input must be finite",
            Self::InvalidOcclusion => {
                "procedural graph Occlusion input must be finite and normalized"
            }
            Self::CoordinateOverflow => "scaled procedural graph coordinates must remain finite",
        })
    }
}

impl std::error::Error for GraphError {}

/// A validated, allocation-free evaluator for the graph's fixed execution contract.
#[derive(Clone, Copy, Debug)]
pub struct ProceduralEvaluator {
    seed: u64,
    scale: f32,
    reads_occlusion: bool,
}

impl ProceduralGraph {
    /// UV → seeded noise → Roughness.
    pub fn value_noise(seed: u64, scale: f32) -> Self {
        Self {
            nodes: vec![
                ProceduralNode {
                    id: 1,
                    kind: NodeKind::Uv,
                },
                ProceduralNode {
                    id: 2,
                    kind: NodeKind::ValueNoise { seed, scale },
                },
                ProceduralNode {
                    id: 3,
                    kind: NodeKind::RoughnessOutput,
                },
            ],
            edges: vec![GraphEdge { from: 1, to: 2 }, GraphEdge { from: 2, to: 3 }],
        }
    }

    /// UV → seeded noise × raw same-set Occlusion paint → Roughness.
    pub fn value_noise_times_occlusion(seed: u64, scale: f32, source_layer_id: LayerId) -> Self {
        Self {
            nodes: vec![
                ProceduralNode {
                    id: 1,
                    kind: NodeKind::Uv,
                },
                ProceduralNode {
                    id: 2,
                    kind: NodeKind::ValueNoise { seed, scale },
                },
                ProceduralNode {
                    id: 3,
                    kind: NodeKind::OcclusionInput { source_layer_id },
                },
                ProceduralNode {
                    id: 4,
                    kind: NodeKind::Multiply,
                },
                ProceduralNode {
                    id: 5,
                    kind: NodeKind::RoughnessOutput,
                },
            ],
            edges: vec![
                GraphEdge { from: 1, to: 2 },
                GraphEdge { from: 2, to: 4 },
                GraphEdge { from: 3, to: 4 },
                GraphEdge { from: 4, to: 5 },
            ],
        }
    }

    pub fn occlusion_source_layer_id(&self) -> Option<LayerId> {
        self.nodes.iter().find_map(|node| match node.kind {
            NodeKind::OcclusionInput { source_layer_id } => Some(source_layer_id),
            _ => None,
        })
    }

    /// Attach or remove the optional Occlusion input while retaining noise parameters.
    pub fn set_occlusion_source_layer_id(&mut self, source_layer_id: Option<LayerId>) -> bool {
        let source_layer_id = source_layer_id.filter(|id| *id != LayerId::nil());
        let Some((seed, scale)) = self.nodes.iter().find_map(|node| match node.kind {
            NodeKind::ValueNoise { seed, scale } => Some((seed, scale)),
            _ => None,
        }) else {
            return false;
        };
        if self.occlusion_source_layer_id() == source_layer_id {
            return false;
        }
        *self = match source_layer_id {
            Some(id) => Self::value_noise_times_occlusion(seed, scale, id),
            None => Self::value_noise(seed, scale),
        };
        true
    }

    /// Validate bounded structure and the deliberately small supported graph vocabulary.
    pub fn validate(&self) -> Result<(), GraphError> {
        if self.nodes.is_empty() || self.nodes.len() > MAX_GRAPH_NODES {
            return Err(GraphError::NodeCount);
        }
        if self.edges.len() > MAX_GRAPH_EDGES {
            return Err(GraphError::EdgeCount);
        }

        let mut nodes = HashMap::with_capacity(self.nodes.len());
        let mut indegree = HashMap::with_capacity(self.nodes.len());
        for node in &self.nodes {
            if nodes.insert(node.id, &node.kind).is_some() {
                return Err(GraphError::DuplicateNode);
            }
            indegree.insert(node.id, 0usize);
            match node.kind {
                NodeKind::ValueNoise { scale, .. }
                    if !scale.is_finite()
                        || !(MIN_NOISE_SCALE..=MAX_NOISE_SCALE).contains(&scale) =>
                {
                    return Err(GraphError::InvalidNoiseScale);
                }
                NodeKind::OcclusionInput { source_layer_id }
                    if source_layer_id == LayerId::nil() =>
                {
                    return Err(GraphError::InvalidSourceLayer);
                }
                _ => {}
            }
        }

        let mut edges = HashSet::with_capacity(self.edges.len());
        let mut outgoing: HashMap<u32, Vec<u32>> = HashMap::with_capacity(self.nodes.len());
        for edge in &self.edges {
            if edge.from == edge.to
                || !nodes.contains_key(&edge.from)
                || !nodes.contains_key(&edge.to)
                || !edges.insert(*edge)
            {
                return Err(GraphError::InvalidEdge);
            }
            *indegree.get_mut(&edge.to).expect("edge target was checked") += 1;
            outgoing.entry(edge.from).or_default().push(edge.to);
        }

        let mut ready: Vec<u32> = indegree
            .iter()
            .filter_map(|(id, d)| (*d == 0).then_some(*id))
            .collect();
        let mut visited = 0;
        while let Some(id) = ready.pop() {
            visited += 1;
            if let Some(targets) = outgoing.get(&id) {
                for target in targets {
                    let degree = indegree.get_mut(target).expect("target was validated");
                    *degree -= 1;
                    if *degree == 0 {
                        ready.push(*target);
                    }
                }
            }
        }
        if visited != self.nodes.len() {
            return Err(GraphError::Cycle);
        }

        let count =
            |kind: fn(&NodeKind) -> bool| self.nodes.iter().filter(|n| kind(&n.kind)).count();
        let has_uv = count(|k| matches!(k, NodeKind::Uv)) == 1;
        let has_noise = count(|k| matches!(k, NodeKind::ValueNoise { .. })) == 1;
        let has_output = count(|k| matches!(k, NodeKind::RoughnessOutput)) == 1;
        let has_ao = count(|k| matches!(k, NodeKind::OcclusionInput { .. })) == 1;
        let has_multiply = count(|k| matches!(k, NodeKind::Multiply)) == 1;
        if !has_uv || !has_noise || !has_output || has_ao != has_multiply {
            return Err(GraphError::UnsupportedTopology);
        }
        let expected_edges = if has_ao { 4 } else { 2 };
        if self.edges.len() != expected_edges {
            return Err(GraphError::UnsupportedTopology);
        }

        let by_id: HashMap<u32, &NodeKind> = self.nodes.iter().map(|n| (n.id, &n.kind)).collect();
        for node in &self.nodes {
            let inputs = self.edges.iter().filter(|e| e.to == node.id).count();
            let valid = match node.kind {
                NodeKind::Uv | NodeKind::OcclusionInput { .. } => inputs == 0,
                NodeKind::ValueNoise { .. } | NodeKind::RoughnessOutput => inputs == 1,
                NodeKind::Multiply => inputs == 2,
            };
            if !valid {
                return Err(GraphError::UnsupportedTopology);
            }
        }
        let compatible = self.edges.iter().all(|e| {
            matches!(
                (by_id.get(&e.from), by_id.get(&e.to)),
                (Some(NodeKind::Uv), Some(NodeKind::ValueNoise { .. }))
                    | (
                        Some(NodeKind::ValueNoise { .. }),
                        Some(NodeKind::RoughnessOutput)
                    )
            ) || (has_ao
                && matches!(
                    (by_id.get(&e.from), by_id.get(&e.to)),
                    (Some(NodeKind::ValueNoise { .. }), Some(NodeKind::Multiply))
                        | (
                            Some(NodeKind::OcclusionInput { .. }),
                            Some(NodeKind::Multiply)
                        )
                        | (Some(NodeKind::Multiply), Some(NodeKind::RoughnessOutput))
                ))
        });
        if !compatible {
            return Err(GraphError::UnsupportedTopology);
        }
        let noise_id = self
            .nodes
            .iter()
            .find(|n| matches!(n.kind, NodeKind::ValueNoise { .. }))
            .unwrap()
            .id;
        let output_id = self
            .nodes
            .iter()
            .find(|n| matches!(n.kind, NodeKind::RoughnessOutput))
            .unwrap()
            .id;
        if has_ao {
            let multiply_id = self
                .nodes
                .iter()
                .find(|n| matches!(n.kind, NodeKind::Multiply))
                .unwrap()
                .id;
            let ao_id = self
                .nodes
                .iter()
                .find(|n| matches!(n.kind, NodeKind::OcclusionInput { .. }))
                .unwrap()
                .id;
            if !self.edges.contains(&GraphEdge {
                from: noise_id,
                to: multiply_id,
            }) || !self.edges.contains(&GraphEdge {
                from: ao_id,
                to: multiply_id,
            }) || !self.edges.contains(&GraphEdge {
                from: multiply_id,
                to: output_id,
            }) {
                return Err(GraphError::UnsupportedTopology);
            }
        } else if !self.edges.contains(&GraphEdge {
            from: noise_id,
            to: output_id,
        }) {
            return Err(GraphError::UnsupportedTopology);
        }
        Ok(())
    }

    pub fn compile(&self) -> Result<ProceduralEvaluator, GraphError> {
        self.validate()?;
        let (seed, scale) = self
            .nodes
            .iter()
            .find_map(|n| match n.kind {
                NodeKind::ValueNoise { seed, scale } => Some((seed, scale)),
                _ => None,
            })
            .expect("validated graph has one noise node");
        Ok(ProceduralEvaluator {
            seed,
            scale,
            reads_occlusion: self.occlusion_source_layer_id().is_some(),
        })
    }

    pub fn evaluate_roughness(&self, uv: [f32; 2], occlusion: f32) -> Result<f32, GraphError> {
        self.compile()?.evaluate(uv, occlusion)
    }
}

impl ProceduralEvaluator {
    /// Missing source pixels are represented by neutral Occlusion (1.0), as in Kalium.
    pub fn evaluate(self, uv: [f32; 2], occlusion: f32) -> Result<f32, GraphError> {
        if !uv[0].is_finite() || !uv[1].is_finite() {
            return Err(GraphError::InvalidUv);
        }
        if !occlusion.is_finite() || !(0.0..=1.0).contains(&occlusion) {
            return Err(GraphError::InvalidOcclusion);
        }
        let noise = value_noise_2d(uv, self.seed, self.scale)?;
        Ok(if self.reads_occlusion {
            noise * occlusion
        } else {
            noise
        })
    }
}

fn value_noise_2d(uv: [f32; 2], seed: u64, scale: f32) -> Result<f32, GraphError> {
    let x = uv[0] * scale;
    let y = uv[1] * scale;
    let min_coord = (i64::MIN + 1) as f64;
    let max_coord = (i64::MAX - 2) as f64;
    if !x.is_finite()
        || !y.is_finite()
        || (x as f64) < min_coord
        || (x as f64) > max_coord
        || (y as f64) < min_coord
        || (y as f64) > max_coord
    {
        return Err(GraphError::CoordinateOverflow);
    }
    let x0 = x.floor() as i64;
    let y0 = y.floor() as i64;
    let tx = smoothstep(x - x.floor());
    let ty = smoothstep(y - y.floor());
    let top = lerp(
        lattice_value(x0, y0, seed),
        lattice_value(x0 + 1, y0, seed),
        tx,
    );
    let bottom = lerp(
        lattice_value(x0, y0 + 1, seed),
        lattice_value(x0 + 1, y0 + 1, seed),
        tx,
    );
    Ok(lerp(top, bottom, ty))
}

fn lattice_value(x: i64, y: i64, seed: u64) -> f32 {
    let mut value = seed
        ^ (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (y as u64).wrapping_mul(0xD1B5_4A32_D192_ED03);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^= value >> 31;
    (value >> 40) as f32 / ((1u32 << 24) - 1) as f32
}

fn smoothstep(value: f32) -> f32 {
    value * value * (3.0 - 2.0 * value)
}
fn lerp(a: f32, b: f32, weight: f32) -> f32 {
    a + (b - a) * weight
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_is_seeded_repeatable_and_bounded() {
        let a = ProceduralGraph::value_noise(7, 8.0);
        let b = ProceduralGraph::value_noise(8, 8.0);
        let uv = [0.237, 0.619];
        let value = a.evaluate_roughness(uv, 1.0).unwrap();
        assert_eq!(value, a.evaluate_roughness(uv, 1.0).unwrap());
        assert!((0.0..=1.0).contains(&value));
        assert_ne!(value, b.evaluate_roughness(uv, 1.0).unwrap());
    }

    #[test]
    fn ao_graph_multiplies_noise_and_uses_typed_stable_source_id() {
        let source = LayerId::new();
        let graph = ProceduralGraph::value_noise_times_occlusion(71, 4.0, source);
        assert_eq!(graph.occlusion_source_layer_id(), Some(source));
        let noise = ProceduralGraph::value_noise(71, 4.0)
            .evaluate_roughness([0.375, 0.625], 1.0)
            .unwrap();
        assert_eq!(
            graph.evaluate_roughness([0.375, 0.625], 0.5).unwrap(),
            noise * 0.5
        );
        assert_eq!(
            graph.evaluate_roughness([0.375, 0.625], 1.0).unwrap(),
            noise
        );
    }

    #[test]
    fn graph_rejects_bad_ids_scale_topology_cycles_and_limits() {
        let mut g = ProceduralGraph::value_noise(1, f32::NAN);
        assert_eq!(g.validate(), Err(GraphError::InvalidNoiseScale));
        g = ProceduralGraph::value_noise_times_occlusion(1, 2.0, LayerId::nil());
        assert_eq!(g.validate(), Err(GraphError::InvalidSourceLayer));
        g = ProceduralGraph::value_noise(1, 2.0);
        g.edges.push(GraphEdge { from: 3, to: 1 });
        assert_eq!(g.validate(), Err(GraphError::Cycle));
        g = ProceduralGraph::value_noise(1, 2.0);
        g.edges.pop();
        assert!(g.validate().is_err());
        g.nodes.resize(
            MAX_GRAPH_NODES + 1,
            ProceduralNode {
                id: 0,
                kind: NodeKind::Uv,
            },
        );
        assert_eq!(g.validate(), Err(GraphError::NodeCount));
    }

    #[test]
    fn evaluation_rejects_non_finite_inputs_and_scale_overflow() {
        let graph = ProceduralGraph::value_noise(0, 1.0);
        assert_eq!(
            graph.evaluate_roughness([f32::INFINITY, 0.0], 1.0),
            Err(GraphError::InvalidUv)
        );
        assert_eq!(
            graph.evaluate_roughness([f32::MAX, f32::MAX], 1.0),
            Err(GraphError::CoordinateOverflow)
        );
        assert_eq!(
            graph.evaluate_roughness([0.5, 0.5], f32::NAN),
            Err(GraphError::InvalidOcclusion)
        );
    }
}
