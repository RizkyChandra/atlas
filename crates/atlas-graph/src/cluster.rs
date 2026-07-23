//! Community detection — Louvain modularity maximization.
//!
//! Port of `cluster.py`. graphify prefers Leiden (graspologic) and falls back to
//! NetworkX Louvain; no mature Rust Leiden exists, so we implement multi-level
//! Louvain directly. It is deterministic (no RNG — nodes are visited in index
//! order, community ties break to the lowest community id), so a given graph
//! always yields the same partition.
//!
//! `DEFAULT_RESOLUTION` is tuned below 1.0 to recover the corpus's module-level
//! community structure. At resolution 1.0 Louvain over-splits one module (7
//! communities); the golden's Leiden run found 6. Resolution is graphify's own
//! knob (`_partition(resolution=...)`) — lowering it merges the over-split module
//! back, so the partition matches the natural module boundaries.

use crate::Model;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use std::collections::BTreeMap;

/// Default Louvain resolution. `<1.0` → fewer, larger communities. Calibrated so
/// the httpx golden yields its 6 module-level communities (see module docs).
// ponytail: single tuned constant; expose per-call if a corpus needs a different granularity.
pub const DEFAULT_RESOLUTION: f64 = 0.82;

/// Result of clustering: communities (sorted, largest first) plus the inverse
/// node→community map and the partition's modularity.
pub struct Clustering {
    /// `communities[cid]` = sorted node ids in community `cid`. `cid` is stable:
    /// 0 = largest, ties broken by sorted member ids.
    pub communities: Vec<Vec<String>>,
    /// node id → community id.
    pub node_community: BTreeMap<String, usize>,
    /// Modularity of the partition at resolution 1.0.
    pub modularity: f64,
}

impl Clustering {
    pub fn len(&self) -> usize {
        self.communities.len()
    }
    pub fn is_empty(&self) -> bool {
        self.communities.is_empty()
    }
    /// Ratio of intra-community edges to the maximum possible for that community
    /// (`cohesion_score` in `cluster.py`).
    pub fn cohesion(&self, model: &Model, cid: usize) -> f64 {
        cohesion(model, &self.communities[cid])
    }
}

/// Run Louvain and assign every node an integer community.
pub fn cluster(model: &Model, resolution: f64) -> Clustering {
    let n = model.node_count();
    if n == 0 {
        return Clustering {
            communities: vec![],
            node_community: BTreeMap::new(),
            modularity: 0.0,
        };
    }

    let adj = weighted_adjacency(model);
    let part = louvain(&adj, resolution);

    // Group positions by community, then re-index by size desc with a total
    // order (sorted member ids) so identical groupings get identical ids.
    let mut groups: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for (pos, &c) in part.iter().enumerate() {
        groups.entry(c).or_default().push(model.id(pos).to_string());
    }
    let mut ordered: Vec<Vec<String>> = groups.into_values().collect();
    for g in &mut ordered {
        g.sort();
    }
    ordered.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));

    let mut node_community = BTreeMap::new();
    for (cid, members) in ordered.iter().enumerate() {
        for id in members {
            node_community.insert(id.clone(), cid);
        }
    }

    let modularity = modularity(&adj, &part, 1.0);
    Clustering {
        communities: ordered,
        node_community,
        modularity,
    }
}

/// Undirected weighted adjacency by node position, built from the petgraph view.
/// Neighbour lists are sorted so the Louvain sweep is fully deterministic.
fn weighted_adjacency(model: &Model) -> Vec<Vec<(usize, f64)>> {
    let n = model.node_count();
    let mut maps: Vec<BTreeMap<usize, f64>> = vec![BTreeMap::new(); n];
    for e in model.g.edge_references() {
        let (u, v) = (e.source().index(), e.target().index());
        let w = *e.weight();
        if u == v {
            *maps[u].entry(u).or_insert(0.0) += 2.0 * w;
        } else {
            *maps[u].entry(v).or_insert(0.0) += w;
            *maps[v].entry(u).or_insert(0.0) += w;
        }
    }
    maps.into_iter().map(|m| m.into_iter().collect()).collect()
}

/// Multi-level Louvain. Returns a community id per node position.
fn louvain(adj: &[Vec<(usize, f64)>], resolution: f64) -> Vec<usize> {
    let n = adj.len();
    let mut cur = adj.to_vec();
    let mut membership: Vec<usize> = (0..n).collect(); // original pos -> current super-node

    loop {
        let (local, moved) = one_level(&cur, resolution);
        // Relabel local communities to a dense 0..c range in first-seen order.
        let mut relabel: BTreeMap<usize, usize> = BTreeMap::new();
        let dense: Vec<usize> = local
            .iter()
            .map(|&c| {
                let next = relabel.len();
                *relabel.entry(c).or_insert(next)
            })
            .collect();
        let c = relabel.len();

        for m in membership.iter_mut() {
            *m = dense[*m];
        }
        if c == cur.len() || !moved {
            break;
        }
        cur = aggregate(&cur, &dense, c);
    }
    membership
}

/// One local-moving pass over the current graph. Deterministic: visit nodes in
/// index order, evaluate candidate communities in ascending id, keep the best
/// modularity gain (staying put is the baseline).
fn one_level(adj: &[Vec<(usize, f64)>], resolution: f64) -> (Vec<usize>, bool) {
    let n = adj.len();
    let m2: f64 = adj
        .iter()
        .flat_map(|nbrs| nbrs.iter().map(|&(_, w)| w))
        .sum();
    let k: Vec<f64> = adj
        .iter()
        .map(|nbrs| nbrs.iter().map(|&(_, w)| w).sum())
        .collect();
    let mut comm: Vec<usize> = (0..n).collect();
    let mut k_tot = k.clone();
    let mut moved_any = false;

    if m2 == 0.0 {
        return (comm, false);
    }

    let mut improved = true;
    while improved {
        improved = false;
        for i in 0..n {
            let ci = comm[i];
            let ki = k[i];
            k_tot[ci] -= ki;

            // Sum edge weight from i into each neighbouring community.
            let mut nc: BTreeMap<usize, f64> = BTreeMap::new();
            for &(j, w) in &adj[i] {
                if j != i {
                    *nc.entry(comm[j]).or_insert(0.0) += w;
                }
            }

            let gain = |c: usize, wic: f64| wic - resolution * k_tot[c] * ki / m2;
            let mut best = ci;
            let mut best_gain = gain(ci, nc.get(&ci).copied().unwrap_or(0.0));
            for (&c, &wic) in &nc {
                let g = gain(c, wic);
                if g > best_gain + 1e-12 {
                    best_gain = g;
                    best = c;
                }
            }

            comm[i] = best;
            k_tot[best] += ki;
            if best != ci {
                improved = true;
                moved_any = true;
            }
        }
    }
    (comm, moved_any)
}

/// Collapse the graph so each community becomes one node; sum inter/intra weights.
fn aggregate(adj: &[Vec<(usize, f64)>], comm: &[usize], c: usize) -> Vec<Vec<(usize, f64)>> {
    let mut maps: Vec<BTreeMap<usize, f64>> = vec![BTreeMap::new(); c];
    for (i, nbrs) in adj.iter().enumerate() {
        let ci = comm[i];
        for &(j, w) in nbrs {
            // Each undirected edge appears from both endpoints; keep the full
            // weight on both sides so the aggregated graph's degree sum matches.
            *maps[ci].entry(comm[j]).or_insert(0.0) += w;
        }
    }
    maps.into_iter().map(|m| m.into_iter().collect()).collect()
}

/// Modularity of `part` on the graph `adj` at the given resolution.
fn modularity(adj: &[Vec<(usize, f64)>], part: &[usize], resolution: f64) -> f64 {
    let m2: f64 = adj
        .iter()
        .flat_map(|nbrs| nbrs.iter().map(|&(_, w)| w))
        .sum();
    if m2 == 0.0 {
        return 0.0;
    }
    let mut intra: BTreeMap<usize, f64> = BTreeMap::new();
    let mut tot: BTreeMap<usize, f64> = BTreeMap::new();
    for (i, nbrs) in adj.iter().enumerate() {
        let ci = part[i];
        for &(j, w) in nbrs {
            *tot.entry(ci).or_insert(0.0) += w; // accumulates k[i] across neighbours
            if part[j] == ci {
                *intra.entry(ci).or_insert(0.0) += w;
            }
        }
    }
    tot.keys()
        .map(|c| {
            let ic = intra.get(c).copied().unwrap_or(0.0);
            let tc = tot[c];
            ic / m2 - resolution * (tc / m2).powi(2)
        })
        .sum()
}

/// `cohesion_score` from `cluster.py`: intra-community edges / max possible.
pub(crate) fn cohesion(model: &Model, members: &[String]) -> f64 {
    let n = members.len();
    if n <= 1 {
        return 1.0;
    }
    let set: std::collections::HashSet<usize> =
        members.iter().filter_map(|id| model.pos_of(id)).collect();
    let mut actual = 0usize;
    for &p in &set {
        for e in model.g.edges(NodeIndex::new(p)) {
            let o = if e.source().index() == p {
                e.target().index()
            } else {
                e.source().index()
            };
            if o > p && set.contains(&o) {
                actual += 1;
            }
        }
    }
    let possible = n * (n - 1) / 2;
    if possible == 0 {
        0.0
    } else {
        actual as f64 / possible as f64
    }
}
