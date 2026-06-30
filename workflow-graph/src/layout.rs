//! A layered layout that draws a graph from top to bottom, as Graphviz `dot` does, in the
//! steps of Sugiyama's method:
//!
//! 1. Edges point from a node to a later one; an edge that points back is laid out turned
//!    round, so the order of the nodes is a topological order.
//! 2. Layers by the longest path from a node without predecessors. Then each node that has
//!    successors moves down to just above the first of them, which shortens long edges.
//! 3. An edge that spans several layers gets a point in each layer between, so that it can
//!    pass between the nodes there.
//! 4. The order in each layer: sweeps down and up that sort each layer by the mean position of
//!    its neighbours (the barycenter), a bounded number of times, keeping the order with the
//!    fewest crossings.
//! 5. The places in each layer: each node moves towards the mean place of its neighbours, as
//!    close as the order and the gaps between nodes allow (an isotonic regression).
//!
//! Everything is deterministic: the same input gives the same layout. The work grows with the
//! number of nodes, edges and layers, so graphs of thousands of tasks stay fast.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

/// The space between two nodes of a layer.
const GAP: f64 = 24.0;
/// The space between two layers.
const LAYER_GAP: f64 = 48.0;
/// The space around the drawing.
const MARGIN: f64 = 16.0;
/// The width that an edge takes where it passes a layer.
const PASSING: f64 = 8.0;
/// How many times the order of the layers is swept down and up at most.
const SWEEPS: usize = 12;
/// How many times the places are moved towards the neighbours.
const PLACING: usize = 8;
/// Beyond this many points in passing, edges that span more layers than [`LONG`] go straight.
const MOST_PASSING: usize = 200_000;
const LONG: usize = 16;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// The size of a node.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

/// Where a graph's nodes and edges go.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    pub width: f64,
    pub height: f64,
    /// The center of each node.
    pub nodes: Vec<Point>,
    /// For each edge, the points it passes, from the border of its first node to the border
    /// of its second; none for an edge from a node to itself.
    pub edges: Vec<Vec<Point>>,
    /// The layer of each node, from 0 at the top.
    pub layers: Vec<usize>,
}

/// Lays out nodes of the given sizes and edges between them, by their positions in `sizes`.
/// Edges that name no node are left without points.
pub fn layout(sizes: &[Size], edges: &[(usize, usize)]) -> Layout {
    let count = sizes.len();
    // Edges from an earlier node to a later one, once each.
    let mut forward: Vec<(usize, usize)> = edges
        .iter()
        .filter(|(from, to)| from != to && *from < count && *to < count)
        .map(|&(from, to)| (from.min(to), from.max(to)))
        .collect();
    forward.sort_unstable();
    forward.dedup();
    let layer = layers(count, &forward);
    let mut graph = Layered::new(sizes, &layer, &forward);
    graph.order();
    graph.place();
    graph.finish(sizes, edges)
}

/// The layer of each node: the longest path from a node without predecessors, then moved down
/// to just above its first successor.
fn layers(count: usize, forward: &[(usize, usize)]) -> Vec<usize> {
    let mut layer = vec![0usize; count];
    for &(from, to) in forward {
        // Sorted by `from`, and `from < to`, so each `from` is final when it is read.
        layer[to] = layer[to].max(layer[from] + 1);
    }
    // Later nodes first, so that a node moves after the nodes below it have moved.
    let mut successors: Vec<Vec<usize>> = vec![Vec::new(); count];
    for &(from, to) in forward {
        successors[from].push(to);
    }
    for node in (0..count).rev() {
        if let Some(lowest) = successors[node].iter().map(|&to| layer[to]).min() {
            layer[node] = layer[node].max(lowest.saturating_sub(1));
        }
    }
    layer
}

/// The graph with points in passing, arranged in layers.
struct Layered {
    /// The width of each node, the real ones first, then the points in passing.
    widths: Vec<f64>,
    /// The layer of each node.
    layer: Vec<usize>,
    /// The nodes of each layer, in their order.
    layers: Vec<Vec<usize>>,
    /// The neighbours of each node in the layer above and in the layer below.
    up: Vec<Vec<usize>>,
    down: Vec<Vec<usize>>,
    /// For each forward edge, the nodes it passes from top to bottom, both ends included.
    chains: Vec<Vec<usize>>,
    /// The forward edges, for finding the chain of an edge.
    forward: Vec<(usize, usize)>,
    /// The position of each node in its layer.
    position: Vec<usize>,
    /// The center of each node.
    x: Vec<f64>,
}

impl Layered {
    fn new(sizes: &[Size], layer: &[usize], forward: &[(usize, usize)]) -> Self {
        let mut widths: Vec<f64> = sizes.iter().map(|size| size.width.max(1.0)).collect();
        let mut layer = layer.to_vec();
        let passing: usize = forward
            .iter()
            .map(|&(from, to)| layer[to] - layer[from] - 1)
            .sum();
        let mut chains = Vec::with_capacity(forward.len());
        for &(from, to) in forward {
            let span = layer[to] - layer[from];
            let mut chain = vec![from];
            if span > 1 && (passing <= MOST_PASSING || span <= LONG) {
                for step in 1..span {
                    widths.push(PASSING);
                    layer.push(layer[from] + step);
                    chain.push(widths.len() - 1);
                }
            }
            chain.push(to);
            chains.push(chain);
        }
        let total = widths.len();
        let mut up = vec![Vec::new(); total];
        let mut down = vec![Vec::new(); total];
        for chain in &chains {
            for pair in chain.windows(2) {
                down[pair[0]].push(pair[1]);
                up[pair[1]].push(pair[0]);
            }
        }
        let depth = layer.iter().copied().max().map_or(0, |deepest| deepest + 1);
        let mut layers = vec![Vec::new(); depth];
        for node in 0..total {
            layers[layer[node]].push(node);
        }
        let mut position = vec![0; total];
        for nodes in &layers {
            for (index, &node) in nodes.iter().enumerate() {
                position[node] = index;
            }
        }
        Layered {
            widths,
            layer,
            layers,
            up,
            down,
            chains,
            forward: forward.to_vec(),
            position,
            x: vec![0.0; total],
        }
    }

    /// Sweeps down and up, sorting each layer by the barycenter of its neighbours, and keeps
    /// the order with the fewest crossings.
    fn order(&mut self) {
        let mut best = self.layers.clone();
        let mut fewest = self.crossings();
        let mut worse = 0;
        for _ in 0..SWEEPS {
            if fewest == 0 {
                break;
            }
            for index in 1..self.layers.len() {
                self.sort_layer(index, true);
            }
            for index in (0..self.layers.len().saturating_sub(1)).rev() {
                self.sort_layer(index, false);
            }
            let crossings = self.crossings();
            if crossings < fewest {
                fewest = crossings;
                best = self.layers.clone();
                worse = 0;
            } else {
                worse += 1;
                if worse >= 2 {
                    break;
                }
            }
        }
        self.layers = best;
        for nodes in &self.layers {
            for (index, &node) in nodes.iter().enumerate() {
                self.position[node] = index;
            }
        }
    }

    /// Sorts a layer by the mean position of each node's neighbours above (or below); a node
    /// without such neighbours keeps its position. Ties keep their order.
    fn sort_layer(&mut self, index: usize, from_above: bool) {
        let mut keyed: Vec<(f64, usize, usize)> = self.layers[index]
            .iter()
            .map(|&node| {
                let neighbours = if from_above {
                    &self.up[node]
                } else {
                    &self.down[node]
                };
                let key = if neighbours.is_empty() {
                    self.position[node] as f64
                } else {
                    neighbours
                        .iter()
                        .map(|&other| self.position[other] as f64)
                        .sum::<f64>()
                        / neighbours.len() as f64
                };
                (key, self.position[node], node)
            })
            .collect();
        keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        for (place, &(_, _, node)) in keyed.iter().enumerate() {
            self.position[node] = place;
        }
        self.layers[index] = keyed.into_iter().map(|(_, _, node)| node).collect();
    }

    /// The number of edge crossings between all neighbouring layers.
    fn crossings(&self) -> usize {
        let mut total = 0;
        for index in 0..self.layers.len().saturating_sub(1) {
            let mut pairs: Vec<(usize, usize)> = Vec::new();
            for &node in &self.layers[index] {
                for &below in &self.down[node] {
                    pairs.push((self.position[node], self.position[below]));
                }
            }
            pairs.sort_unstable();
            total += inversions(
                &pairs.iter().map(|pair| pair.1).collect::<Vec<_>>(),
                self.layers[index + 1].len(),
            );
        }
        total
    }

    /// Places the nodes of each layer: first side by side, then each towards the mean place
    /// of its neighbours, alternating the neighbours above and below.
    fn place(&mut self) {
        for nodes in &self.layers {
            let mut left = 0.0;
            for &node in nodes {
                self.x[node] = left + self.widths[node] / 2.0;
                left += self.widths[node] + GAP;
            }
        }
        for round in 0..PLACING {
            let from_above = round % 2 == 0;
            let order: Vec<usize> = if from_above {
                (1..self.layers.len()).collect()
            } else {
                (0..self.layers.len().saturating_sub(1)).rev().collect()
            };
            for index in order {
                self.place_layer(index, from_above);
            }
        }
        // The drawing starts at the margin.
        let leftmost = (0..self.x.len())
            .map(|node| self.x[node] - self.widths[node] / 2.0)
            .fold(f64::INFINITY, f64::min);
        if leftmost.is_finite() {
            for x in &mut self.x {
                *x += MARGIN - leftmost;
            }
        }
    }

    /// Moves the nodes of a layer as close to the mean place of their neighbours as their
    /// order and the gaps allow: the least squares fit under order constraints, found by
    /// pooling adjacent violators.
    fn place_layer(&mut self, index: usize, from_above: bool) {
        let nodes = &self.layers[index];
        if nodes.is_empty() {
            return;
        }
        // The least distance of each node's center from the first node's center.
        let mut offset = Vec::with_capacity(nodes.len());
        let mut distance = 0.0;
        for (place, &node) in nodes.iter().enumerate() {
            if place > 0 {
                distance += (self.widths[nodes[place - 1]] + self.widths[node]) / 2.0 + GAP;
            }
            offset.push(distance);
        }
        let wanted: Vec<f64> = nodes
            .iter()
            .map(|&node| {
                let neighbours = if from_above {
                    &self.up[node]
                } else {
                    &self.down[node]
                };
                if neighbours.is_empty() {
                    self.x[node]
                } else {
                    neighbours.iter().map(|&other| self.x[other]).sum::<f64>()
                        / neighbours.len() as f64
                }
            })
            .collect();
        // With y = x - offset, the order and gaps say y does not decrease.
        let targets: Vec<f64> = wanted
            .iter()
            .zip(&offset)
            .map(|(want, offset)| want - offset)
            .collect();
        let fitted = isotonic(&targets);
        for (place, &node) in nodes.iter().enumerate() {
            self.x[node] = fitted[place] + offset[place];
        }
    }

    /// The final layout: the centers of the real nodes, and the points of each edge.
    fn finish(&self, sizes: &[Size], edges: &[(usize, usize)]) -> Layout {
        let mut heights = vec![0.0f64; self.layers.len()];
        for (node, size) in sizes.iter().enumerate() {
            heights[self.layer[node]] = heights[self.layer[node]].max(size.height);
        }
        let mut tops = Vec::with_capacity(heights.len());
        let mut top = MARGIN;
        for height in &heights {
            tops.push(top);
            top += height + LAYER_GAP;
        }
        let height = if heights.is_empty() {
            2.0 * MARGIN
        } else {
            top - LAYER_GAP + MARGIN
        };
        let center = |node: usize| Point {
            x: self.x[node],
            y: tops[self.layer[node]] + heights[self.layer[node]] / 2.0,
        };
        let nodes: Vec<Point> = (0..sizes.len()).map(center).collect();
        let width = (0..self.x.len())
            .map(|node| self.x[node] + self.widths[node] / 2.0)
            .fold(0.0, f64::max)
            + MARGIN;
        let routes = edges
            .iter()
            .map(|&(from, to)| {
                if from == to || from >= sizes.len() || to >= sizes.len() {
                    return Vec::new();
                }
                let key = (from.min(to), from.max(to));
                let Ok(found) = self.forward.binary_search(&key) else {
                    return Vec::new();
                };
                let chain = &self.chains[found];
                let mut points: Vec<Point> = chain.iter().map(|&node| center(node)).collect();
                // From the bottom of the upper node to the top of the lower one.
                if let Some(first) = points.first_mut() {
                    first.y += sizes[chain[0]].height / 2.0;
                }
                if let Some(last) = points.last_mut() {
                    last.y -= sizes[chain[chain.len() - 1]].height / 2.0;
                }
                if from > to {
                    points.reverse();
                }
                points
            })
            .collect();
        Layout {
            width: width.max(2.0 * MARGIN),
            height,
            nodes,
            edges: routes,
            layers: self.layer[..sizes.len()].to_vec(),
        }
    }
}

/// The number of pairs out of order in a sequence of values below `limit`, counted with a
/// Fenwick tree.
fn inversions(values: &[usize], limit: usize) -> usize {
    let mut tree = vec![0usize; limit + 1];
    let mut count = 0;
    for (seen, &value) in values.iter().enumerate() {
        // How many of the values seen so far are at most `value`.
        let mut at_most = 0;
        let mut index = value + 1;
        while index > 0 {
            at_most += tree[index];
            index &= index - 1;
        }
        count += seen - at_most;
        let mut index = value + 1;
        while index <= limit {
            tree[index] += 1;
            index += index & index.wrapping_neg();
        }
    }
    count
}

/// The non-decreasing sequence closest to `targets` in least squares, by pooling adjacent
/// violators.
fn isotonic(targets: &[f64]) -> Vec<f64> {
    // Blocks of equal value: their mean and how many targets they hold.
    let mut blocks: Vec<(f64, usize)> = Vec::with_capacity(targets.len());
    for &target in targets {
        blocks.push((target, 1));
        while blocks.len() > 1 {
            let (last, last_count) = blocks[blocks.len() - 1];
            let (before, before_count) = blocks[blocks.len() - 2];
            if before <= last {
                break;
            }
            let count = last_count + before_count;
            let mean = (before * before_count as f64 + last * last_count as f64) / count as f64;
            blocks.pop();
            let end = blocks.len() - 1;
            blocks[end] = (mean, count);
        }
    }
    blocks
        .into_iter()
        .flat_map(|(mean, count)| std::iter::repeat_n(mean, count))
        .collect()
}
