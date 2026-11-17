use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::storage::{LinkEdge, NoteStamp};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    pub hub: i64,
    /// Every member, hub included, by `latest_part_at` descending then id descending.
    pub members: Vec<i64>,
}

/// Undirected resolved adjacency: self-links and dangling edges dropped.
fn adjacency(edges: &[LinkEdge]) -> BTreeMap<i64, BTreeSet<i64>> {
    let mut adj: BTreeMap<i64, BTreeSet<i64>> = BTreeMap::new();
    for e in edges {
        let Some(to) = e.to else { continue };
        if to == e.from {
            continue;
        }
        adj.entry(e.from).or_default().insert(to);
        adj.entry(to).or_default().insert(e.from);
    }
    adj
}

/// Every thread in the vault; a note missing from `latest` sorts as the oldest.
pub fn threads(edges: &[LinkEdge], latest: &HashMap<i64, i64>) -> Vec<Thread> {
    let adj = adjacency(edges);
    let at = |id: &i64| latest.get(id).copied().unwrap_or(i64::MIN);
    let mut seen: HashSet<i64> = HashSet::new();
    let mut out = Vec::new();
    for &start in adj.keys() {
        if !seen.insert(start) {
            continue;
        }
        let mut members = vec![start];
        let mut stack = vec![start];
        while let Some(n) = stack.pop() {
            for &m in &adj[&n] {
                if seen.insert(m) {
                    members.push(m);
                    stack.push(m);
                }
            }
        }
        if members.len() < 2 {
            continue;
        }
        let hub = *members
            .iter()
            .max_by_key(|id| (adj[id].len(), at(id), std::cmp::Reverse(**id)))
            .expect("a component has members");
        members.sort_by_key(|id| std::cmp::Reverse((at(id), *id)));
        out.push(Thread { hub, members });
    }
    out
}

pub fn thread_index(threads: &[Thread]) -> HashMap<i64, i64> {
    threads
        .iter()
        .flat_map(|t| t.members.iter().map(move |m| (*m, t.hub)))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphScope {
    Everything,
    Recent {
        since: i64,
    },
    Book {
        book_id: i64,
    },
    /// One note, its neighbours out to `steps` (1 or 2), its unwritten targets and unlinked mentions.
    Around {
        note_id: i64,
        steps: u8,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NodeKey {
    Note(i64),
    /// ASCII-lowercased (SQLite `NOCASE`), so `write_links`' one-note spellings are one node.
    Title(String),
}

impl NodeKey {
    fn title(raw: &str) -> NodeKey {
        NodeKey::Title(raw.trim().to_ascii_lowercase())
    }

    /// `n<id>` or `t<lowercased title>`: the wire key.
    pub fn wire(&self) -> String {
        match self {
            NodeKey::Note(id) => format!("n{id}"),
            NodeKey::Title(t) => format!("t{t}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphEdgeKind {
    Link,
    /// `from` mentions `to`'s title without linking it; `Around` only.
    Mention,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphEdge {
    pub from: NodeKey,
    pub to: NodeKey,
    pub kind: GraphEdgeKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphNode {
    pub key: NodeKey,
    pub link_text: Option<String>,
    /// Distinct nodes sharing an edge with this one in this graph, not in the whole vault.
    pub degree: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Graph {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

pub const MENTION_EDGE_CAP: usize = 5;

/// Draws `scope`: edges are the induced edges among chosen nodes, deduped per `(from, to)`.
/// A note with no edge is not drawn, except `Around`'s own note.
#[expect(clippy::too_many_lines)]
pub fn build_graph(
    scope: GraphScope,
    edges: &[LinkEdge],
    stamps: &[NoteStamp],
    mentions: &[i64],
) -> Graph {
    let by_id: HashMap<i64, NoteStamp> = stamps.iter().map(|s| (s.id, *s)).collect();
    let adj = adjacency(edges);
    let live: Vec<(NodeKey, NodeKey, &str)> = edges
        .iter()
        .filter(|e| by_id.contains_key(&e.from) && e.to != Some(e.from))
        .map(|e| {
            let to = match e.to {
                Some(id) => NodeKey::Note(id),
                None => NodeKey::title(&e.target_title),
            };
            (NodeKey::Note(e.from), to, e.target_title.as_str())
        })
        .collect();

    let dangling_of = |ids: &HashSet<i64>| -> HashSet<NodeKey> {
        live.iter()
            .filter(|(f, t, _)| {
                matches!(t, NodeKey::Title(_)) && matches!(f, NodeKey::Note(id) if ids.contains(id))
            })
            .map(|(_, t, _)| t.clone())
            .collect()
    };
    let one_step = |seeds: &HashSet<i64>| -> HashSet<NodeKey> {
        let mut set: HashSet<NodeKey> = seeds.iter().map(|id| NodeKey::Note(*id)).collect();
        for id in seeds {
            for n in adj.get(id).into_iter().flatten() {
                set.insert(NodeKey::Note(*n));
            }
        }
        set.extend(dangling_of(seeds));
        set
    };

    let mut keep_alone: Option<NodeKey> = None;
    let mut mention_from: Vec<i64> = Vec::new();
    let chosen: HashSet<NodeKey> = match scope {
        GraphScope::Everything => live
            .iter()
            .flat_map(|(f, t, _)| [f.clone(), t.clone()])
            .collect(),
        GraphScope::Recent { since } => {
            let seeds = stamps
                .iter()
                .filter(|s| s.latest_part_at >= since)
                .map(|s| s.id)
                .collect();
            one_step(&seeds)
        }
        GraphScope::Book { book_id } => {
            let seeds = stamps
                .iter()
                .filter(|s| s.book_id == Some(book_id))
                .map(|s| s.id)
                .collect();
            one_step(&seeds)
        }
        GraphScope::Around { note_id, steps } => {
            let mut reached: HashSet<i64> = HashSet::from([note_id]);
            let mut frontier = vec![note_id];
            for _ in 0..steps {
                let mut next = Vec::new();
                for id in frontier {
                    for n in adj.get(&id).into_iter().flatten() {
                        if reached.insert(*n) {
                            next.push(*n);
                        }
                    }
                }
                frontier = next;
            }
            let mut set: HashSet<NodeKey> = reached.iter().map(|id| NodeKey::Note(*id)).collect();
            set.extend(dangling_of(&HashSet::from([note_id])));
            mention_from = mentions
                .iter()
                .copied()
                .filter(|m| *m != note_id && by_id.contains_key(m))
                .take(MENTION_EDGE_CAP)
                .collect();
            set.extend(mention_from.iter().map(|m| NodeKey::Note(*m)));
            keep_alone = Some(NodeKey::Note(note_id));
            set
        }
    };

    // In `Around`, a dangling target is drawn only from the centre note.
    let centre = match scope {
        GraphScope::Around { note_id, .. } => Some(note_id),
        _ => None,
    };
    let mut seen: HashSet<(NodeKey, NodeKey)> = HashSet::new();
    let mut graph_edges = Vec::new();
    let mut link_text: HashMap<NodeKey, String> = HashMap::new();
    for (f, t, raw) in &live {
        if !chosen.contains(f) || !chosen.contains(t) {
            continue;
        }
        if let (Some(c), NodeKey::Title(_)) = (centre, t)
            && *f != NodeKey::Note(c)
        {
            continue;
        }
        if seen.insert((f.clone(), t.clone())) {
            if matches!(t, NodeKey::Title(_)) {
                link_text
                    .entry(t.clone())
                    .or_insert_with(|| raw.trim().to_string());
            }
            graph_edges.push(GraphEdge {
                from: f.clone(),
                to: t.clone(),
                kind: GraphEdgeKind::Link,
            });
        }
    }
    if let Some(c) = centre {
        for m in &mention_from {
            let pair = (NodeKey::Note(*m), NodeKey::Note(c));
            if seen.insert(pair.clone()) {
                graph_edges.push(GraphEdge {
                    from: pair.0,
                    to: pair.1,
                    kind: GraphEdgeKind::Mention,
                });
            }
        }
    }

    let mut neighbours: HashMap<NodeKey, HashSet<NodeKey>> = HashMap::new();
    for e in &graph_edges {
        neighbours
            .entry(e.from.clone())
            .or_default()
            .insert(e.to.clone());
        neighbours
            .entry(e.to.clone())
            .or_default()
            .insert(e.from.clone());
    }
    let mut nodes: Vec<GraphNode> = chosen
        .into_iter()
        .filter(|k| neighbours.contains_key(k) || Some(k) == keep_alone.as_ref())
        .map(|k| GraphNode {
            degree: neighbours.get(&k).map_or(0, |n| n.len() as i64),
            link_text: link_text.get(&k).cloned(),
            key: k,
        })
        .collect();
    let at = |k: &NodeKey| match k {
        NodeKey::Note(id) => by_id.get(id).map_or(i64::MIN, |s| s.latest_part_at),
        NodeKey::Title(_) => i64::MIN,
    };
    nodes.sort_by(|a, b| {
        let rank = |k: &NodeKey| matches!(k, NodeKey::Title(_));
        rank(&a.key)
            .cmp(&rank(&b.key))
            .then(at(&b.key).cmp(&at(&a.key)))
            .then_with(|| match (&a.key, &b.key) {
                (NodeKey::Note(x), NodeKey::Note(y)) => y.cmp(x),
                _ => a.key.cmp(&b.key),
            })
    });
    Graph {
        nodes,
        edges: graph_edges,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(from: i64, to: Option<i64>, title: &str) -> LinkEdge {
        LinkEdge {
            from,
            to,
            target_title: title.into(),
        }
    }

    fn stamp(id: i64, at: i64, book: Option<i64>) -> NoteStamp {
        NoteStamp {
            id,
            latest_part_at: at,
            book_id: book,
        }
    }

    #[test]
    fn the_hub_has_the_most_neighbours_and_ties_go_to_the_newest() {
        let edges = [
            link(2, Some(1), "a"),
            link(3, Some(1), "a"),
            link(4, Some(5), "e"),
            link(6, None, "nobody"),
        ];
        let latest = HashMap::from([(1, 10), (2, 30), (3, 20), (4, 5), (5, 6)]);
        let t = threads(&edges, &latest);
        assert_eq!(t.len(), 2, "a dangling edge joins nothing");
        assert_eq!(t[0].hub, 1);
        assert_eq!(t[0].members, vec![2, 3, 1], "newest first");
        assert_eq!(t[1].hub, 5);
    }

    #[test]
    fn around_draws_the_note_its_reach_its_own_unwritten_links_and_mentions() {
        let edges = [
            link(1, Some(2), "Two"),
            link(2, Some(3), "Three"),
            link(1, None, "Maps That Lie"),
            link(1, None, "maps that lie"),
            link(3, None, "elsewhere"),
        ];
        let stamps = [
            stamp(1, 1, None),
            stamp(2, 2, None),
            stamp(3, 3, None),
            stamp(9, 9, None),
        ];
        let one = build_graph(
            GraphScope::Around {
                note_id: 1,
                steps: 1,
            },
            &edges,
            &stamps,
            &[9],
        );
        let keys: BTreeSet<String> = one.nodes.iter().map(|n| n.key.wire()).collect();
        assert_eq!(
            keys,
            BTreeSet::from([
                "n1".into(),
                "n2".into(),
                "n9".into(),
                "tmaps that lie".into()
            ])
        );
        assert_eq!(
            one.edges.len(),
            3,
            "two spellings of one target are one edge"
        );
        assert!(one.edges.iter().any(|e| e.kind == GraphEdgeKind::Mention));

        let two = build_graph(
            GraphScope::Around {
                note_id: 1,
                steps: 2,
            },
            &edges,
            &stamps,
            &[],
        );
        let keys: BTreeSet<String> = two.nodes.iter().map(|n| n.key.wire()).collect();
        assert!(keys.contains("n3"));
        assert!(
            !keys.contains("telsewhere"),
            "a neighbour's forward references are its own"
        );
    }

    #[test]
    fn a_note_with_no_links_is_not_drawn_except_as_the_subject() {
        let stamps = [stamp(1, 1, Some(7)), stamp(2, 2, Some(7))];
        let edges = [link(2, None, "x")];
        let g = build_graph(GraphScope::Book { book_id: 7 }, &edges, &stamps, &[]);
        let keys: Vec<String> = g.nodes.iter().map(|n| n.key.wire()).collect();
        assert_eq!(keys, vec!["n2".to_string(), "tx".to_string()]);
        let alone = build_graph(
            GraphScope::Around {
                note_id: 1,
                steps: 1,
            },
            &edges,
            &stamps,
            &[],
        );
        assert_eq!(alone.nodes.len(), 1);
        assert_eq!(alone.nodes[0].degree, 0);
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use proptest::prelude::*;

    fn edges_strategy() -> impl Strategy<Value = Vec<LinkEdge>> {
        proptest::collection::vec((1i64..12, proptest::option::of(1i64..12)), 0..30).prop_map(|v| {
            v.into_iter()
                .map(|(from, to)| LinkEdge {
                    from,
                    to,
                    target_title: format!("t{}", to.unwrap_or(99)),
                })
                .collect()
        })
    }

    proptest! {
        #[test]
        fn threads_partition_the_linked_notes(
            edges in edges_strategy(),
            ats in proptest::collection::vec(0i64..5, 12),
        ) {
            let latest: HashMap<i64, i64> = (1..12).map(|id| (id, ats[id as usize])).collect();
            let ts = threads(&edges, &latest);
            let index = thread_index(&ts);
            let linked: BTreeSet<i64> = adjacency(&edges).keys().copied().collect();
            let mut covered = BTreeSet::new();
            for t in &ts {
                prop_assert!(t.members.len() >= 2);
                prop_assert!(t.members.contains(&t.hub));
                for m in &t.members {
                    prop_assert!(covered.insert(*m), "{} in two threads", m);
                    prop_assert_eq!(index[m], t.hub);
                }
                let adj = adjacency(&edges);
                let best = t.members.iter().map(|m| adj[m].len()).max().unwrap();
                prop_assert_eq!(adj[&t.hub].len(), best);
            }
            prop_assert_eq!(covered, linked);
        }

        #[test]
        fn a_graph_is_closed_and_deduped(edges in edges_strategy(), since in 0i64..12) {
            let stamps: Vec<NoteStamp> = (1..12)
                .map(|id| NoteStamp { id, latest_part_at: id, book_id: Some(id % 3) })
                .collect();
            for scope in [
                GraphScope::Everything,
                GraphScope::Recent { since },
                GraphScope::Book { book_id: 1 },
                GraphScope::Around { note_id: 1 + since % 11, steps: 2 },
            ] {
                let g = build_graph(scope, &edges, &stamps, &[2, 3]);
                let keys: HashSet<&NodeKey> = g.nodes.iter().map(|n| &n.key).collect();
                let mut pairs = HashSet::new();
                for e in &g.edges {
                    prop_assert!(keys.contains(&e.from) && keys.contains(&e.to));
                    prop_assert!(pairs.insert((&e.from, &e.to)));
                    prop_assert!(e.from != e.to);
                }
            }
        }
    }
}
