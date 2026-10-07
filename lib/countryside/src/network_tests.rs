use super::*;
use crate::key::{Key, Stage};

fn scattered(count: usize) -> Vec<Node> {
    let mut draws = Key::new(12).draws(Stage::Village, (0, 0));
    (0..count)
        .map(|index| Node {
            placed: Placed::Gateway(HoldingId::new(i32::try_from(index).expect("few"), 0), 1),
            at: Point::new(draws.range(0.0, 1000.0), draws.range(0.0, 1000.0)),
        })
        .collect()
}

/// Every edge of `graph` over `nodes`, node by node, as a layout plans it.
fn graph_of(
    graph: Graph,
    nodes: &[Node],
    longest: f64,
    keeps: &dyn Fn(&Node, &Node) -> bool,
) -> Vec<(usize, usize)> {
    let filed = Filed::new(nodes.iter().map(|node| node.at), longest).expect("room");
    (0..nodes.len())
        .flat_map(|a| {
            edges_from(graph, (nodes, &filed), (a, longest), keeps)
                .expect("room")
                .into_iter()
                .map(move |b| (a, b))
        })
        .collect()
}

fn neighbourhood(
    nodes: &[Node],
    longest: f64,
    keeps: &dyn Fn(&Node, &Node) -> bool,
) -> Vec<(usize, usize)> {
    graph_of(Graph::Neighbourhood, nodes, longest, keeps)
}

fn gabriel_only(
    nodes: &[Node],
    longest: f64,
    keeps: &dyn Fn(&Node, &Node) -> bool,
) -> Vec<(usize, usize)> {
    graph_of(Graph::GabrielOnly, nodes, longest, keeps)
}

fn connected(count: usize, edges: &[(usize, usize)]) -> bool {
    let mut reached = alloc::vec![false; count];
    let mut stack = alloc::vec![0];
    reached[0] = true;
    while let Some(at) = stack.pop() {
        for &(a, b) in edges {
            for (from, to) in [(a, b), (b, a)] {
                if from == at && !reached[to] {
                    reached[to] = true;
                    stack.push(to);
                }
            }
        }
    }
    reached.iter().all(|&reached| reached)
}

#[test]
fn the_neighbourhood_graph_joins_every_node_and_no_third_lies_in_any_lune() {
    let nodes = scattered(160);
    let edges = neighbourhood(&nodes, 2000.0, &|_, _| true);
    assert!(
        connected(nodes.len(), &edges),
        "a relative-neighbourhood graph is connected"
    );
    for &(a, b) in &edges {
        let span = squared(nodes[a].at, nodes[b].at);
        for (c, node) in nodes.iter().enumerate() {
            if c != a && c != b {
                let far = squared(node.at, nodes[a].at).max(squared(node.at, nodes[b].at));
                assert!(far >= span, "{c} lies in the lune of {a}–{b}");
            }
        }
    }
    // Every node's nearest neighbour is joined to it.
    for (a, node) in nodes.iter().enumerate() {
        let nearest = (0..nodes.len())
            .filter(|&b| b != a)
            .min_by(|&x, &y| {
                squared(nodes[x].at, node.at).total_cmp(&squared(nodes[y].at, node.at))
            })
            .expect("others");
        assert!(edges.contains(&(a.min(nearest), a.max(nearest))));
    }
}

#[test]
fn a_shortcut_is_a_gabriel_edge_the_neighbourhood_graph_leaves_out() {
    let nodes = scattered(120);
    let lanes = neighbourhood(&nodes, 2000.0, &|_, _| true);
    let paths = gabriel_only(&nodes, 2000.0, &|_, _| true);
    assert!(!paths.is_empty());
    for &(a, b) in &paths {
        assert!(lanes.binary_search(&(a, b)).is_err());
        let middle = nodes[a].at.lerp(nodes[b].at, 0.5);
        let span = squared(nodes[a].at, nodes[b].at);
        assert!(nodes
            .iter()
            .enumerate()
            .all(|(c, node)| c == a || c == b || squared(node.at, middle) >= 0.25 * span));
    }
}

#[test]
fn the_graph_is_the_same_whatever_order_its_nodes_come_in() {
    let nodes = scattered(90);
    let mut reversed = nodes.clone();
    reversed.reverse();
    let named = |nodes: &[Node], edges: Vec<(usize, usize)>| {
        let mut named: Vec<(Placed, Placed)> = edges
            .into_iter()
            .map(|(a, b)| {
                let (a, b) = (nodes[a].placed, nodes[b].placed);
                (a.min(b), a.max(b))
            })
            .collect();
        named.sort_unstable();
        named
    };
    let forward = named(&nodes, neighbourhood(&nodes, 400.0, &|_, _| true));
    let backward = named(&reversed, neighbourhood(&reversed, 400.0, &|_, _| true));
    assert_eq!(forward, backward);
}

#[test]
fn no_edge_is_longer_than_the_longest_or_one_keeps_refuses() {
    let nodes = scattered(100);
    let edges = neighbourhood(&nodes, 150.0, &|a, b| a.at.x < 500.0 && b.at.x < 500.0);
    for (a, b) in edges {
        assert!(squared(nodes[a].at, nodes[b].at) <= 150.0 * 150.0);
        assert!(nodes[a].at.x < 500.0 && nodes[b].at.x < 500.0);
    }
}
