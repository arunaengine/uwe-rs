// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::*;

/// The layout down of nodes of these sizes and these edges.
fn lay(sizes: &[Size], edges: &[(usize, usize)]) -> Layout {
    layout(&Graph {
        sizes: sizes.to_vec(),
        edges: edges.to_vec(),
        ..Graph::default()
    })
}

fn sizes(count: usize) -> Vec<Size> {
    (0..count)
        .map(|index| Size {
            width: 40.0 + (index % 7) as f64 * 15.0,
            height: 28.0 + (index % 2) as f64 * 16.0,
        })
        .collect()
}

/// A graph like a pipeline: a few samples, each through a chain of steps, and steps that
/// collect all samples, with a seeded choice of extra edges.
fn pipeline(samples: usize, steps: usize) -> (Vec<Size>, Vec<(usize, usize)>) {
    let mut edges = Vec::new();
    let mut seed: u64 = 7;
    let mut next = |limit: usize| {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (seed >> 33) as usize % limit.max(1)
    };
    let node = |sample: usize, step: usize| sample * steps + step;
    for sample in 0..samples {
        for step in 1..steps {
            edges.push((node(sample, step - 1), node(sample, step)));
            if next(4) == 0 && step >= 2 {
                edges.push((node(sample, next(step - 1)), node(sample, step)));
            }
        }
    }
    let collect = samples * steps;
    for sample in 0..samples {
        edges.push((node(sample, next(steps)), collect));
    }
    (sizes(collect + 1), edges)
}

/// Checks that no two nodes overlap: in each layer the nodes keep a gap, and layers do not
/// share their vertical band.
fn assert_no_overlap(sizes: &[Size], layout: &Layout) {
    let mut by_layer: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
    for (node, &layer) in layout.layers.iter().enumerate() {
        by_layer.entry(layer).or_default().push(node);
    }
    for nodes in by_layer.values() {
        let mut spans: Vec<(f64, f64)> = nodes
            .iter()
            .map(|&node| {
                let half = sizes[node].width / 2.0;
                (layout.nodes[node].x - half, layout.nodes[node].x + half)
            })
            .collect();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        for pair in spans.windows(2) {
            assert!(pair[0].1 + GAP - 1e-6 <= pair[1].0, "overlap: {pair:?}");
        }
    }
    for (a, &layer_a) in layout.layers.iter().enumerate() {
        for (b, &layer_b) in layout.layers.iter().enumerate() {
            if layer_a < layer_b {
                let bottom = layout.nodes[a].y + sizes[a].height / 2.0;
                let top = layout.nodes[b].y - sizes[b].height / 2.0;
                assert!(bottom < top, "layers {layer_a} and {layer_b} overlap");
            }
        }
    }
    for (node, point) in layout.nodes.iter().enumerate() {
        assert!(point.x - sizes[node].width / 2.0 >= MARGIN - 1e-6);
        assert!(point.x + sizes[node].width / 2.0 <= layout.width + 1e-6);
        assert!(point.y + sizes[node].height / 2.0 <= layout.height + 1e-6);
    }
}

#[test]
fn nodes_do_not_overlap() {
    let (sizes, edges) = pipeline(6, 8);
    let layout = lay(&sizes, &edges);
    assert_no_overlap(&sizes, &layout);
}

#[test]
fn the_same_graph_gives_the_same_layout() {
    let (sizes, edges) = pipeline(9, 6);
    assert_eq!(lay(&sizes, &edges), lay(&sizes, &edges));
}

#[test]
fn edges_go_down_through_their_layers() {
    // 0 -> 1 -> 2 -> 3, and 0 -> 3 passes layers 1 and 2.
    let sizes = sizes(4);
    let edges = [(0, 1), (1, 2), (2, 3), (0, 3)];
    let layout = lay(&sizes, &edges);
    assert_eq!(layout.layers, [0, 1, 2, 3]);
    let long = &layout.edges[3];
    assert_eq!(long.len(), 4, "a point in each layer between: {long:?}");
    assert!(long.windows(2).all(|pair| pair[0].y < pair[1].y));
    // It starts at the bottom of node 0 and ends at the top of node 3.
    assert_eq!(long[0].y, layout.nodes[0].y + sizes[0].height / 2.0);
    assert_eq!(long[3].y, layout.nodes[3].y - sizes[3].height / 2.0);
}

#[test]
fn nodes_with_more_successors_than_predecessors_sit_above_their_first_successor() {
    // 0 -> 1 -> 2 -> 3 -> 4; 1 -> 5 -> 4: node 5, one in and one out, stays below node 1, while
    // node 6, which feeds 3 and 4, moves down to just above 3.
    let edges = [
        (0, 1),
        (1, 2),
        (2, 3),
        (3, 4),
        (1, 5),
        (5, 4),
        (6, 3),
        (6, 4),
    ];
    let layout = lay(&sizes(7), &edges);
    assert_eq!(layout.layers, [0, 1, 2, 3, 4, 2, 2]);
}

#[test]
fn nodes_without_predecessors_sit_above_their_first_successor() {
    // 0 -> 2 -> 3 -> 4, and 1 -> 4: node 1 moves down to layer 2.
    let layout = lay(&sizes(5), &[(0, 2), (2, 3), (3, 4), (1, 4)]);
    assert_eq!(layout.layers, [0, 2, 1, 2, 3]);
}

#[test]
fn a_chain_given_backwards_is_drawn_from_the_top() {
    // 2 -> 1 -> 0: node 2 comes first in the depth-first order, so every edge points down.
    let layout = lay(&sizes(3), &[(2, 1), (1, 0)]);
    assert_eq!(layout.layers, [2, 1, 0]);
    for route in &layout.edges {
        assert!(
            route.windows(2).all(|pair| pair[0].y < pair[1].y),
            "{route:?}"
        );
    }
}

#[test]
fn only_the_edge_that_closes_a_cycle_points_up() {
    // 0 -> 1 -> 2 -> 0, and an edge from a node to itself.
    let sizes = sizes(3);
    let layout = lay(&sizes, &[(0, 1), (1, 2), (2, 0), (1, 1)]);
    assert_eq!(layout.layers, [0, 1, 2]);
    let back = &layout.edges[2];
    assert!(back.first().expect("points").y > back.last().expect("points").y);
    assert!(layout.edges[3].is_empty(), "no route for an edge to itself");
}

#[test]
fn depth_first_order_is_topological_without_cycles() {
    assert_eq!(depth_first(4, &[(3, 1), (1, 0), (2, 0)]), [3, 2, 1, 0]);
    assert_eq!(depth_first(3, &[(0, 1), (0, 2)]), [0, 2, 1]);
}

#[test]
fn barycenter_sweeps_remove_crossings() {
    // Two chains whose second layers start out crossed: 0 -> 3, 1 -> 2.
    let layout = lay(&sizes(4), &[(0, 3), (1, 2)]);
    let (x0, x1) = (layout.nodes[0].x, layout.nodes[1].x);
    let (x2, x3) = (layout.nodes[2].x, layout.nodes[3].x);
    assert_eq!(x0 < x1, x3 < x2, "the edges do not cross");
}

#[test]
fn large_graphs_stay_fast() {
    let (sizes, edges) = pipeline(200, 25);
    let start = std::time::Instant::now();
    let layout = lay(&sizes, &edges);
    assert_eq!(layout.nodes.len(), 5001);
    assert!(start.elapsed().as_secs() < 20, "{:?}", start.elapsed());
    assert_no_overlap(&sizes, &layout);
}

#[test]
fn long_edges_run_straight() {
    // A chain of five nodes, and an edge from the first to the last beside it.
    let sizes = sizes(5);
    let layout = lay(&sizes, &[(0, 1), (1, 2), (2, 3), (3, 4), (0, 4)]);
    let long = &layout.edges[4];
    assert_eq!(long.len(), 5);
    let inner: Vec<f64> = long[1..4].iter().map(|point| point.x).collect();
    assert!(
        inner.windows(2).all(|pair| (pair[0] - pair[1]).abs() < 0.5),
        "{inner:?}"
    );
}

#[test]
fn a_layout_to_the_right_is_the_layout_down_turned() {
    let (sizes, edges) = pipeline(4, 5);
    let down = lay(&sizes, &edges);
    let right = layout(&Graph {
        sizes: sizes.clone(),
        edges: edges.clone(),
        direction: Direction::Right,
        ..Graph::default()
    });
    assert_eq!(right.layers, down.layers);
    for (node, point) in right.nodes.iter().enumerate() {
        let layer = right.layers[node];
        // Later layers are further right, and the edges leave and enter at the sides.
        for (other, other_point) in right.nodes.iter().enumerate() {
            if right.layers[other] > layer {
                assert!(other_point.x > point.x, "{node} and {other}");
            }
        }
    }
    let (from, to) = edges[0];
    let route = &right.edges[0];
    let start = route.first().expect("points");
    let end = route.last().expect("points");
    assert!((start.x - (right.nodes[from].x + sizes[from].width / 2.0)).abs() < 1e-6);
    assert!((end.x - (right.nodes[to].x - sizes[to].width / 2.0)).abs() < 1e-6);
}

#[test]
fn nodes_beside_others_follow_them_in_their_layer() {
    // 0 -> 1 and 3 -> 1 with 2 beside 0, joined by an edge.
    let sizes = sizes(4);
    let graph = Graph {
        sizes: sizes.clone(),
        edges: vec![(0, 1), (3, 1), (0, 2)],
        beside: vec![(2, 0)],
        direction: Direction::Down,
    };
    let down = layout(&graph);
    assert_eq!(down.layers, [0, 1, 0, 0]);
    let (host, next) = (down.nodes[0], down.nodes[2]);
    assert_eq!(host.y, next.y);
    let left = host.x + sizes[0].width / 2.0 + BESIDE;
    assert!(
        (next.x - sizes[2].width / 2.0 - left).abs() < 1e-6,
        "{down:?}"
    );
    // Node 3 in the same layer keeps clear of both.
    let spans = |node: usize| {
        let half = sizes[node].width / 2.0;
        (down.nodes[node].x - half, down.nodes[node].x + half)
    };
    let (first, second, third) = (spans(0), spans(2), spans(3));
    assert!(third.0 >= second.1 + GAP - 1e-6 || third.1 + GAP <= first.0 + 1e-6);
    // The edge between them is a short line from side to side.
    assert_eq!(down.edges[2].len(), 2);
    assert!((down.edges[2][0].x - (host.x + sizes[0].width / 2.0)).abs() < 1e-6);
    // In a layout to the right, the node beside sits below.
    let right = layout(&Graph {
        direction: Direction::Right,
        ..graph
    });
    assert_eq!(right.nodes[0].x, right.nodes[2].x);
    let top = right.nodes[0].y + sizes[0].height / 2.0 + BESIDE;
    assert!((right.nodes[2].y - sizes[2].height / 2.0 - top).abs() < 1e-6);
}

#[test]
fn nodes_beside_each_other_in_a_circle_are_left_in_layers() {
    let layout = layout(&Graph {
        sizes: sizes(3),
        beside: vec![(0, 1), (1, 0), (2, 2), (2, 9)],
        ..Graph::default()
    });
    assert_eq!(layout.nodes.len(), 3);
    assert_eq!(layout.layers, [0, 0, 0]);
}

#[test]
fn inversions_count_pairs_out_of_order() {
    assert_eq!(inversions(&[0, 1, 2], 3), 0);
    assert_eq!(inversions(&[2, 1, 0], 3), 3);
    assert_eq!(inversions(&[1, 1, 0], 2), 2);
}

#[test]
fn isotonic_fits_keep_the_order() {
    let ones = [1.0; 3];
    assert_eq!(isotonic(&[1.0, 2.0, 3.0], &ones), [1.0, 2.0, 3.0]);
    assert_eq!(isotonic(&[3.0, 1.0, 2.0], &ones), [2.0, 2.0, 2.0]);
    assert_eq!(isotonic(&[0.0, 4.0, 2.0], &ones), [0.0, 3.0, 3.0]);
    // A heavier target moves less.
    assert_eq!(isotonic(&[3.0, 1.0], &[3.0, 1.0]), [2.5, 2.5]);
}
