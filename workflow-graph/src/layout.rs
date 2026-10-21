//! Lays out directed graph nodes and edges in ordered layers.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

/// The space between two nodes of a layer.
const GAP: f64 = 24.0;
/// The space between a node and a node beside it.
const BESIDE: f64 = 12.0;
/// The space between two layers.
const LAYER_GAP: f64 = 48.0;
/// The space around the drawing.
const MARGIN: f64 = 16.0;
/// The width that an edge takes where it passes a layer.
const PASSING: f64 = 8.0;
/// How much more a point in passing weighs than a node when the places are fitted.
const PASSING_WEIGHT: f64 = 4.0;
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

/// Which way a layout goes, from the first nodes to the last.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Direction {
    /// From top to bottom: the layers are rows.
    #[default]
    Down,
    /// From left to right: the layers are columns.
    Right,
}

impl Direction {
    /// The direction that `-direction` names: `down` or `right`.
    pub fn parse(text: &str) -> Option<Direction> {
        match text {
            "down" => Some(Direction::Down),
            "right" => Some(Direction::Right),
            _ => None,
        }
    }

    /// The name of the direction: `down` or `right`.
    pub fn name(self) -> &'static str {
        match self {
            Direction::Down => "down",
            Direction::Right => "right",
        }
    }
}

/// A graph to lay out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Graph {
    /// The size of each node.
    pub sizes: Vec<Size>,
    /// The edges, by the positions of their nodes in `sizes`.
    pub edges: Vec<(usize, usize)>,
    /// Nodes that sit beside another node instead of in a layer of their own, as pairs of the
    /// node and the one it sits beside.
    pub beside: Vec<(usize, usize)>,
    pub direction: Direction,
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
    /// The layer of each node, from 0 at the top, or at the left in a layout to the right.
    pub layers: Vec<usize>,
}

impl Layout {
    /// Swaps x and y, which turns a layout down into one to the right.
    fn turn(&mut self) {
        std::mem::swap(&mut self.width, &mut self.height);
        let points = self.nodes.iter_mut().chain(self.edges.iter_mut().flatten());
        for point in points {
            std::mem::swap(&mut point.x, &mut point.y);
        }
    }
}

/// The room that a node takes in its layer: from its center to its left and right ends, its
/// own height, and the height it needs with the nodes beside it.
#[derive(Clone, Copy, Debug)]
struct Room {
    left: f64,
    right: f64,
    height: f64,
    band: f64,
}

impl Room {
    fn of(size: Size) -> Room {
        let half = size.width.max(1.0) / 2.0;
        Room {
            left: half,
            right: half,
            height: size.height,
            band: size.height,
        }
    }
}

/// Lays out a graph. Edges that name no node, and edges from a node to itself, are left
/// without points.
pub fn layout(graph: &Graph) -> Layout {
    let count = graph.sizes.len();
    let turned = graph.direction == Direction::Right;
    let sizes: Vec<Size> = (graph.sizes.iter())
        .map(|size| match turned {
            true => Size {
                width: size.height,
                height: size.width,
            },
            false => *size,
        })
        .collect();
    let beside = Beside::new(count, &graph.beside);
    // The nodes in layers, and the room that each takes there with the nodes beside it.
    let mut index: Vec<Option<usize>> = vec![None; count];
    let mut layered = Vec::new();
    let mut rooms = Vec::new();
    for node in (0..count).filter(|&node| beside.host[node].is_none()) {
        index[node] = Some(rooms.len());
        layered.push(node);
        rooms.push(Room::of(sizes[node]));
    }
    for &(node, host) in &beside.pairs {
        if let Some(at) = index[host] {
            rooms[at].right += BESIDE + sizes[node].width;
            rooms[at].band = rooms[at].band.max(sizes[node].height);
        }
    }
    let known = |node: usize| index.get(node).copied().flatten();
    let edges: Vec<(usize, usize)> = (graph.edges.iter())
        .filter_map(|&(from, to)| Some((known(from)?, known(to)?)))
        .collect();
    let arranged = arrange(&rooms, &edges);
    let mut nodes = vec![Point::default(); count];
    let mut layers = vec![0; count];
    let mut ends = Vec::with_capacity(layered.len());
    for (at, &node) in layered.iter().enumerate() {
        nodes[node] = arranged.nodes[at];
        layers[node] = arranged.layers[at];
        ends.push(nodes[node].x + sizes[node].width / 2.0);
    }
    // Each node beside another follows it, or the node that came beside it before.
    for &(node, host) in &beside.pairs {
        let Some(at) = index[host] else { continue };
        let left = ends[at] + BESIDE;
        nodes[node] = Point {
            x: left + sizes[node].width / 2.0,
            y: nodes[host].y,
        };
        layers[node] = layers[host];
        ends[at] = left + sizes[node].width;
    }
    let mut routes = arranged.edges.into_iter();
    let edges = (graph.edges.iter())
        .map(|&(from, to)| {
            if from >= count || to >= count || from == to {
                return Vec::new();
            }
            if known(from).is_some() && known(to).is_some() {
                return routes.next().unwrap_or_default();
            }
            side_by_side((nodes[from], sizes[from]), (nodes[to], sizes[to]))
        })
        .collect();
    let mut layout = Layout {
        width: arranged.width,
        height: arranged.height,
        nodes,
        edges,
        layers,
    };
    if turned {
        layout.turn();
    }
    layout
}

/// The line between two nodes of a layer, like a node and a node beside it: from the side of
/// the first that faces the second to the side of the second that faces the first.
fn side_by_side(from: (Point, Size), to: (Point, Size)) -> Vec<Point> {
    let toward = if to.0.x >= from.0.x { 1.0 } else { -1.0 };
    vec![
        Point {
            x: from.0.x + toward * from.1.width / 2.0,
            y: from.0.y,
        },
        Point {
            x: to.0.x - toward * to.1.width / 2.0,
            y: to.0.y,
        },
    ]
}

/// The nodes that sit beside another node.
struct Beside {
    /// For each node, the node in a layer that it sits beside, following chains of nodes
    /// beside nodes beside nodes.
    host: Vec<Option<usize>>,
    /// Each node beside another, and that node in a layer, in the order given.
    pairs: Vec<(usize, usize)>,
}

impl Beside {
    /// Pairs with an unknown node, a node beside itself, a node given twice, or a chain that
    /// comes back to its start are left out.
    fn new(count: usize, pairs: &[(usize, usize)]) -> Beside {
        let mut next: Vec<Option<usize>> = vec![None; count];
        let mut given = Vec::new();
        for &(node, of) in pairs {
            if node >= count || of >= count || node == of || next[node].is_some() {
                continue;
            }
            let mut at = of;
            let mut round = false;
            while let Some(following) = next[at] {
                round |= following == node;
                at = following;
            }
            if !round {
                next[node] = Some(of);
                given.push(node);
            }
        }
        let root = |mut node: usize| {
            while let Some(of) = next[node] {
                node = of;
            }
            node
        };
        Beside {
            host: (0..count)
                .map(|node| next[node].map(|_| root(node)))
                .collect(),
            pairs: given.into_iter().map(|node| (node, root(node))).collect(),
        }
    }
}

/// Lays out the nodes in layers, which take the given room, and the edges between them.
fn arrange(rooms: &[Room], edges: &[(usize, usize)]) -> Layout {
    let count = rooms.len();
    // The rank of each node in the depth-first order, and the graph with nodes by rank.
    let order = depth_first(count, edges);
    let mut rank = vec![0; count];
    for (position, &node) in order.iter().enumerate() {
        rank[node] = position;
    }
    let ranked_rooms: Vec<Room> = order.iter().map(|&node| rooms[node]).collect();
    let ranked_edges: Vec<(usize, usize)> = (edges.iter())
        .map(|&(from, to)| (rank[from], rank[to]))
        .collect();
    let ranked = ordered(&ranked_rooms, &ranked_edges);
    Layout {
        width: ranked.width,
        height: ranked.height,
        nodes: (0..count).map(|node| ranked.nodes[rank[node]]).collect(),
        edges: ranked.edges,
        layers: (0..count).map(|node| ranked.layers[rank[node]]).collect(),
    }
}

/// The nodes in reverse postorder of a depth-first search that starts at the nodes in their
/// order and follows the edges in theirs: a topological order when the graph has no cycle.
fn depth_first(count: usize, edges: &[(usize, usize)]) -> Vec<usize> {
    let mut successors: Vec<Vec<usize>> = vec![Vec::new(); count];
    for &(from, to) in edges {
        if from != to {
            successors[from].push(to);
        }
    }
    let mut visited = vec![false; count];
    let mut postorder = Vec::with_capacity(count);
    for start in 0..count {
        if visited[start] {
            continue;
        }
        visited[start] = true;
        // Each entry is a node and how many of its successors were looked at.
        let mut stack = vec![(start, 0)];
        while let Some((node, next)) = stack.last_mut() {
            let node = *node;
            match successors[node].get(*next) {
                Some(&successor) => {
                    *next += 1;
                    if !visited[successor] {
                        visited[successor] = true;
                        stack.push((successor, 0));
                    }
                }
                None => {
                    postorder.push(node);
                    stack.pop();
                }
            }
        }
    }
    postorder.reverse();
    postorder
}

/// The layout of a graph whose node order is already a depth-first order.
fn ordered(rooms: &[Room], edges: &[(usize, usize)]) -> Layout {
    let count = rooms.len();
    // Edges from an earlier node to a later one, once each.
    let mut forward: Vec<(usize, usize)> = edges
        .iter()
        .filter(|(from, to)| from != to && *from < count && *to < count)
        .map(|&(from, to)| (from.min(to), from.max(to)))
        .collect();
    forward.sort_unstable();
    forward.dedup();
    let layer = layers(count, &forward);
    let mut graph = Layered::new(rooms, &layer, &forward);
    graph.order();
    graph.place();
    graph.finish(rooms, edges)
}

/// The layer of each node: the longest path from a node without predecessors; then a node
/// with more successors than predecessors moves down to just above its first successor.
fn layers(count: usize, forward: &[(usize, usize)]) -> Vec<usize> {
    let mut layer = vec![0usize; count];
    let mut predecessors = vec![0usize; count];
    for &(from, to) in forward {
        // Sorted by `from`, and `from < to`, so each `from` is final when it is read.
        layer[to] = layer[to].max(layer[from] + 1);
        predecessors[to] += 1;
    }
    // Later nodes first, so that a node moves after the nodes below it have moved.
    let mut successors: Vec<Vec<usize>> = vec![Vec::new(); count];
    for &(from, to) in forward {
        successors[from].push(to);
    }
    for node in (0..count).rev() {
        if predecessors[node] >= successors[node].len() {
            continue;
        }
        if let Some(lowest) = successors[node].iter().map(|&to| layer[to]).min() {
            layer[node] = layer[node].max(lowest.saturating_sub(1));
        }
    }
    layer
}

/// The graph with points in passing, arranged in layers.
struct Layered {
    /// How far each node reaches to the left and to the right of its center, the real ones
    /// first, then the points in passing.
    left: Vec<f64>,
    right: Vec<f64>,
    /// How much each node counts when the places are fitted.
    weight: Vec<f64>,
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
    fn new(rooms: &[Room], layer: &[usize], forward: &[(usize, usize)]) -> Self {
        let mut left: Vec<f64> = rooms.iter().map(|room| room.left).collect();
        let mut right: Vec<f64> = rooms.iter().map(|room| room.right).collect();
        let mut weight = vec![1.0; rooms.len()];
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
                    left.push(PASSING / 2.0);
                    right.push(PASSING / 2.0);
                    weight.push(PASSING_WEIGHT);
                    layer.push(layer[from] + step);
                    chain.push(left.len() - 1);
                }
            }
            chain.push(to);
            chains.push(chain);
        }
        let total = left.len();
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
            left,
            right,
            weight,
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
            let mut end = 0.0;
            for &node in nodes {
                self.x[node] = end + self.left[node];
                end = self.x[node] + self.right[node] + GAP;
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
            .map(|node| self.x[node] - self.left[node])
            .fold(f64::INFINITY, f64::min);
        if leftmost.is_finite() {
            for x in &mut self.x {
                *x += MARGIN - leftmost;
            }
        }
    }

    /// Moves the nodes of a layer as close to the mean place of their neighbours as their
    /// order and the gaps allow: the weighted least squares fit under order constraints, found
    /// by pooling adjacent violators.
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
                distance += self.right[nodes[place - 1]] + GAP + self.left[node];
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
        let weights: Vec<f64> = nodes.iter().map(|&node| self.weight[node]).collect();
        let fitted = isotonic(&targets, &weights);
        for (place, &node) in nodes.iter().enumerate() {
            self.x[node] = fitted[place] + offset[place];
        }
    }

    /// The final layout: the centers of the real nodes, and the points of each edge.
    fn finish(&self, rooms: &[Room], edges: &[(usize, usize)]) -> Layout {
        let mut heights = vec![0.0f64; self.layers.len()];
        for (node, room) in rooms.iter().enumerate() {
            heights[self.layer[node]] = heights[self.layer[node]].max(room.band);
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
        let nodes: Vec<Point> = (0..rooms.len()).map(center).collect();
        let width = (0..self.x.len())
            .map(|node| self.x[node] + self.right[node])
            .fold(0.0, f64::max)
            + MARGIN;
        let routes = edges
            .iter()
            .map(|&(from, to)| {
                if from == to || from >= rooms.len() || to >= rooms.len() {
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
                    first.y += rooms[chain[0]].height / 2.0;
                }
                if let Some(last) = points.last_mut() {
                    last.y -= rooms[chain[chain.len() - 1]].height / 2.0;
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
            layers: self.layer[..rooms.len()].to_vec(),
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
            index += index.isolate_lowest_one();
        }
    }
    count
}

/// The non-decreasing sequence closest to `targets` in least squares, each target counting
/// with its weight, by pooling adjacent violators.
fn isotonic(targets: &[f64], weights: &[f64]) -> Vec<f64> {
    // Blocks of equal value: their weighted mean, their weight and how many targets they hold.
    let mut blocks: Vec<(f64, f64, usize)> = Vec::with_capacity(targets.len());
    for (&target, &weight) in targets.iter().zip(weights) {
        blocks.push((target, weight, 1));
        while blocks.len() > 1 {
            let (last, last_weight, last_count) = blocks[blocks.len() - 1];
            let (before, before_weight, before_count) = blocks[blocks.len() - 2];
            if before <= last {
                break;
            }
            let weight = before_weight + last_weight;
            let mean = (before * before_weight + last * last_weight) / weight;
            blocks.pop();
            let end = blocks.len() - 1;
            blocks[end] = (mean, weight, before_count + last_count);
        }
    }
    blocks
        .into_iter()
        .flat_map(|(mean, _, count)| std::iter::repeat_n(mean, count))
        .collect()
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
