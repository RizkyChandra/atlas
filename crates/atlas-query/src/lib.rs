//! atlas-query — read-only query/path/explain over a graphify `graph.json`.
//!
//! Ported from graphify `graphify/cli.py` (`query`/`path`/`explain`) and the
//! helpers in `graphify/serve.py`. Works entirely from a committed `graph.json`
//! (via [`atlas_core::Graph`]); it never touches the extractor.
//!
//! Deliberate simplifications vs. graphify (all behaviour-preserving for the
//! documented semantics, not byte-identical output):
//!   * Node resolution keeps graphify's exact→prefix→substring precedence over
//!     label/id, plus an exact `make_id(query)` match, but drops the trigram
//!     prefilter and IDF/coverage-weighted scorer — a linear scan is fine at
//!     graph-json scale (hundreds–thousands of nodes).
//!   * `query`'s `--budget` caps the number of NODES in the returned subgraph.
//!     graphify's budget is a ~token/char cap on rendered text; a node cap is
//!     the same intent (bound the result) and is what the gate checks.
//!   ponytail: linear scans + node-count budget; swap in graphify's trigram/IDF
//!   scorer only if a graph.json ever gets large enough to feel it.

use atlas_core::ids::make_id;
use atlas_core::{Attrs, Graph as AtlasGraph};
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::Direction;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

/// `--budget` default: max nodes in a query subgraph.
pub const DEFAULT_BUDGET: usize = 1500;
/// BFS/DFS expansion depth for `query` (graphify cli passes depth=2).
const QUERY_DEPTH: usize = 2;

fn attr<'a>(a: &'a Attrs, k: &str) -> &'a str {
    a.get(k).and_then(|v| v.as_str()).unwrap_or("")
}

/// Word tokens, lowercased. Unicode-aware split on non-alphanumerics — the same
/// intent as graphify's `_search_tokens` (`\w+`, lowered) without a regex dep.
fn tokens(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
}

/// Question/filler words dropped from query terms so content words drive seeding
/// (graphify `_QUERY_STOPWORDS`, English subset — enough for the port's intent).
const STOPWORDS: &[&str] = &[
    "how", "what", "why", "when", "where", "which", "who", "does", "did", "is", "are", "was",
    "were", "be", "can", "could", "should", "would", "will", "may", "might", "must", "has", "have",
    "had", "the", "and", "but", "not", "for", "from", "with", "into", "that", "this", "these",
    "those", "there", "here", "its", "their", "them", "they", "about", "any", "all", "some",
    "work", "works", "working", "a", "an", "of", "to", "in", "on", "do",
];

/// graphify `_is_searchable`: pure-ASCII-letter tokens must be >2 chars.
fn searchable(t: &str) -> bool {
    if t.chars().all(|c| c.is_ascii_lowercase()) {
        t.len() > 2
    } else {
        true
    }
}

/// A loaded graph.json wrapped in a petgraph digraph for traversal.
pub struct QGraph {
    g: DiGraph<usize, usize>, // node weight = index into atlas.nodes; edge weight = index into atlas.links
    idx: HashMap<String, NodeIndex>,
    atlas: AtlasGraph,
}

/// One neighbour connection in an [`Explain`].
#[derive(Debug, Clone)]
pub struct Conn {
    pub direction: &'static str, // "out" | "in"
    pub neighbor: String,        // neighbour label
    pub relation: String,
    pub confidence: String,
}

/// Result of [`QGraph::explain`].
#[derive(Debug, Clone)]
pub struct Explain {
    pub id: String,
    pub label: String,
    pub source: String,
    pub community: String,
    pub degree: usize,
    pub connections: Vec<Conn>,
}

/// One hop in a [`PathResult`]: `from --relation--> to` (or reverse).
#[derive(Debug, Clone)]
pub struct Hop {
    pub from: String, // node id
    pub to: String,   // node id
    pub forward: bool,
    pub relation: String,
    pub confidence: String,
}

/// Result of [`QGraph::path`].
#[derive(Debug, Clone)]
pub struct PathResult {
    pub src: String, // resolved node id
    pub tgt: String,
    pub hops: Vec<Hop>, // empty + no error only if src==tgt
}

/// Result of [`QGraph::query`].
#[derive(Debug, Clone)]
pub struct QueryResult {
    pub mode: &'static str,           // "bfs" | "dfs"
    pub seeds: Vec<String>,           // resolved seed node ids
    pub nodes: Vec<String>,           // subgraph node ids (seeds first)
    pub edges: Vec<(String, String)>, // discovery edges (source id, target id)
}

impl QGraph {
    pub fn from_atlas(atlas: AtlasGraph) -> Self {
        let mut g = DiGraph::new();
        let mut idx = HashMap::new();
        for (i, n) in atlas.nodes.iter().enumerate() {
            let ni = g.add_node(i);
            idx.insert(attr(n, "id").to_string(), ni);
        }
        for (li, e) in atlas.links.iter().enumerate() {
            // Dangling edges (endpoint not a real node) are silently skipped —
            // they are a separately-tracked bug class, not a query concern.
            if let (Some(&a), Some(&b)) = (idx.get(attr(e, "source")), idx.get(attr(e, "target"))) {
                g.add_edge(a, b, li);
            }
        }
        QGraph { g, idx, atlas }
    }

    pub fn from_file(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        Ok(Self::from_atlas(AtlasGraph::from_file(path)?))
    }

    /// Display label for a node id (falls back to the id itself). For rendering.
    pub fn label_for_id(&self, id: &str) -> String {
        self.idx
            .get(id)
            .map(|&ni| self.label_of(ni))
            .unwrap_or_else(|| id.to_string())
    }

    fn node_attrs(&self, ni: NodeIndex) -> &Attrs {
        &self.atlas.nodes[self.g[ni]]
    }
    fn id_of(&self, ni: NodeIndex) -> &str {
        attr(self.node_attrs(ni), "id")
    }
    /// Display label, falling back to the id (graphify `d.get('label', nid)`).
    fn label_of(&self, ni: NodeIndex) -> String {
        let a = self.node_attrs(ni);
        let l = attr(a, "label");
        if l.is_empty() {
            attr(a, "id").to_string()
        } else {
            l.to_string()
        }
    }
    /// Total incident edges, counting parallels (matches NetworkX DiGraph.degree
    /// = in_degree + out_degree on the multigraph graphify loads for `explain`).
    fn degree(&self, ni: NodeIndex) -> usize {
        self.g.edges_directed(ni, Direction::Outgoing).count()
            + self.g.edges_directed(ni, Direction::Incoming).count()
    }

    /// Resolve a query string to node ids, graphify `_find_node`-style:
    /// exact `make_id`/id/label, then prefix, then substring; best match first.
    pub fn find_node(&self, query: &str) -> Vec<NodeIndex> {
        // Strongest signal: an exact canonical-id hit.
        let canon = make_id([query]);
        if let Some(&ni) = self.idx.get(canon.as_str()) {
            return std::iter::once(ni)
                .chain(self.find_fuzzy(query).into_iter().filter(|&n| n != ni))
                .collect();
        }
        self.find_fuzzy(query)
    }

    fn find_fuzzy(&self, query: &str) -> Vec<NodeIndex> {
        let term = tokens(query).join(" ");
        if term.is_empty() {
            return Vec::new();
        }
        let (mut exact, mut prefix, mut substring) = (Vec::new(), Vec::new(), Vec::new());
        for ni in self.g.node_indices() {
            let a = self.node_attrs(ni);
            let label_tokens = tokens(attr(a, "label")).join(" ");
            let nid = attr(a, "id").to_lowercase();
            if term == label_tokens || term == nid {
                exact.push(ni);
            } else if label_tokens.starts_with(&term) || nid.starts_with(&term) {
                prefix.push(ni);
            } else if label_tokens.contains(&term) || nid.contains(&term) {
                substring.push(ni);
            }
        }
        exact.extend(prefix);
        exact.extend(substring);
        exact
    }

    /// `explain(node)`: resolve, then report source/community/degree and every
    /// incoming/outgoing connection (relation + confidence), degree-sorted.
    pub fn explain(&self, query: &str) -> Option<Explain> {
        let ni = *self.find_node(query).first()?;
        let a = self.node_attrs(ni);
        let source = format!("{} {}", attr(a, "source_file"), attr(a, "source_location"))
            .trim()
            .to_string();
        let community = {
            let cn = attr(a, "community_name");
            if !cn.is_empty() {
                cn.to_string()
            } else {
                a.get("community")
                    .map(|v| v.to_string())
                    .unwrap_or_default()
            }
        };
        let mut connections = Vec::new();
        for e in self.g.edges_directed(ni, Direction::Outgoing) {
            let l = &self.atlas.links[*e.weight()];
            connections.push(Conn {
                direction: "out",
                neighbor: self.label_of(petgraph::visit::EdgeRef::target(&e)),
                relation: attr(l, "relation").to_string(),
                confidence: attr(l, "confidence").to_string(),
            });
        }
        for e in self.g.edges_directed(ni, Direction::Incoming) {
            let l = &self.atlas.links[*e.weight()];
            connections.push(Conn {
                direction: "in",
                neighbor: self.label_of(petgraph::visit::EdgeRef::source(&e)),
                relation: attr(l, "relation").to_string(),
                confidence: attr(l, "confidence").to_string(),
            });
        }
        Some(Explain {
            id: self.id_of(ni).to_string(),
            label: self.label_of(ni),
            source,
            community,
            degree: self.degree(ni),
            connections,
        })
    }

    /// `path(a, b)`: shortest path via BFS over undirected reachability (both
    /// callers and callees). `Ok(None)` = the pair is disconnected ("no path").
    /// `Err` = an endpoint didn't resolve, or both resolved to the same node.
    pub fn path(&self, a: &str, b: &str) -> Result<Option<PathResult>, String> {
        let src = *self
            .find_node(a)
            .first()
            .ok_or_else(|| format!("No node matching '{a}' found."))?;
        let tgt = *self
            .find_node(b)
            .first()
            .ok_or_else(|| format!("No node matching '{b}' found."))?;
        if src == tgt {
            return Err(format!(
                "'{a}' and '{b}' both resolved to the same node '{}'. Use a more specific label \
                 or the exact node ID.",
                self.id_of(src)
            ));
        }
        // BFS over undirected neighbours, recording parents to rebuild the path.
        let mut parent: HashMap<NodeIndex, NodeIndex> = HashMap::new();
        let mut seen: HashSet<NodeIndex> = HashSet::from([src]);
        let mut q = VecDeque::from([src]);
        while let Some(n) = q.pop_front() {
            if n == tgt {
                break;
            }
            // Deterministic order: sort neighbours by node index.
            let mut nbrs: Vec<NodeIndex> = self.g.neighbors_undirected(n).collect();
            nbrs.sort();
            nbrs.dedup();
            for nb in nbrs {
                if seen.insert(nb) {
                    parent.insert(nb, n);
                    q.push_back(nb);
                }
            }
        }
        if !seen.contains(&tgt) {
            return Ok(None); // disconnected
        }
        // Reconstruct src..tgt.
        let mut chain = vec![tgt];
        let mut cur = tgt;
        while cur != src {
            cur = parent[&cur];
            chain.push(cur);
        }
        chain.reverse();
        let mut hops = Vec::new();
        for w in chain.windows(2) {
            let (u, v) = (w[0], w[1]);
            let (li, forward) = self.edge_between(u, v);
            let l = &self.atlas.links[li];
            hops.push(Hop {
                from: self.id_of(u).to_string(),
                to: self.id_of(v).to_string(),
                forward,
                relation: attr(l, "relation").to_string(),
                confidence: attr(l, "confidence").to_string(),
            });
        }
        Ok(Some(PathResult {
            src: self.id_of(src).to_string(),
            tgt: self.id_of(tgt).to_string(),
            hops,
        }))
    }

    /// A stored edge between u and v (either direction); `forward` = u→v exists.
    fn edge_between(&self, u: NodeIndex, v: NodeIndex) -> (usize, bool) {
        if let Some(e) = self.g.find_edge(u, v) {
            (self.g[e], true)
        } else {
            let e = self
                .g
                .find_edge(v, u)
                .expect("path neighbours share an edge");
            (self.g[e], false)
        }
    }

    /// `query(question)`: keyword-match terms to seed nodes, then BFS- (or DFS-,
    /// with `dfs`) expand a scoped subgraph, capping node count at `budget`.
    pub fn query(&self, question: &str, budget: usize, dfs: bool) -> QueryResult {
        // Content terms drive seeding (graphify `_query_terms`).
        let mut terms: Vec<String> = tokens(question)
            .into_iter()
            .filter(|t| searchable(t) && !STOPWORDS.contains(&t.as_str()))
            .collect();
        if terms.is_empty() {
            // All-stopword query: fall back to raw searchable tokens.
            terms = tokens(question)
                .into_iter()
                .filter(|t| searchable(t))
                .collect();
        }
        // One seed per term: its best `find_node` match. Dedup, keep order.
        let mut seeds: Vec<NodeIndex> = Vec::new();
        for t in &terms {
            if let Some(&ni) = self.find_node(t).first() {
                if !seeds.contains(&ni) {
                    seeds.push(ni);
                }
            }
        }
        let mode = if dfs { "dfs" } else { "bfs" };
        if seeds.is_empty() {
            return QueryResult {
                mode,
                seeds: Vec::new(),
                nodes: Vec::new(),
                edges: Vec::new(),
            };
        }
        let (nodes, edges) = self.expand(&seeds, budget, dfs);
        QueryResult {
            mode,
            seeds: seeds.iter().map(|&n| self.id_of(n).to_string()).collect(),
            nodes: nodes.iter().map(|&n| self.id_of(n).to_string()).collect(),
            edges: edges
                .iter()
                .map(|&(u, v)| (self.id_of(u).to_string(), self.id_of(v).to_string()))
                .collect(),
        }
    }

    /// BFS/DFS subgraph expansion from seeds, mirroring graphify `_bfs`/`_dfs`:
    /// don't transit through high-degree hubs (p99 degree, floored at 50), and
    /// stop once `budget` nodes are collected. Seeds are always kept first.
    fn expand(
        &self,
        seeds: &[NodeIndex],
        budget: usize,
        dfs: bool,
    ) -> (Vec<NodeIndex>, Vec<(NodeIndex, NodeIndex)>) {
        let hub_threshold = self.hub_threshold();
        let seed_set: HashSet<NodeIndex> = seeds.iter().copied().collect();
        let mut visited: Vec<NodeIndex> = seeds.to_vec();
        let mut in_visited: HashSet<NodeIndex> = seed_set.clone();
        let mut edges: Vec<(NodeIndex, NodeIndex)> = Vec::new();

        let neighbours = |n: NodeIndex| -> Vec<NodeIndex> {
            let mut v: Vec<NodeIndex> = self.g.neighbors_undirected(n).collect();
            v.sort();
            v.dedup();
            v
        };
        let expandable = |n: NodeIndex| seed_set.contains(&n) || self.degree(n) < hub_threshold;

        if dfs {
            // Depth-limited DFS.
            let mut stack: Vec<(NodeIndex, usize)> =
                seeds.iter().rev().map(|&n| (n, 0usize)).collect();
            let mut dfs_visited: HashSet<NodeIndex> = HashSet::new();
            while let Some((n, d)) = stack.pop() {
                if !dfs_visited.insert(n) {
                    continue;
                }
                if in_visited.insert(n) {
                    visited.push(n);
                }
                if visited.len() >= budget || d >= QUERY_DEPTH || !expandable(n) {
                    continue;
                }
                for nb in neighbours(n).into_iter().rev() {
                    if !dfs_visited.contains(&nb) {
                        edges.push((n, nb));
                        stack.push((nb, d + 1));
                    }
                }
            }
        } else {
            // Layered BFS to QUERY_DEPTH.
            let mut frontier: Vec<NodeIndex> = seeds.to_vec();
            for _ in 0..QUERY_DEPTH {
                if visited.len() >= budget {
                    break;
                }
                let mut next: Vec<NodeIndex> = Vec::new();
                for &n in &frontier {
                    if !expandable(n) {
                        continue;
                    }
                    for nb in neighbours(n) {
                        if in_visited.insert(nb) {
                            edges.push((n, nb));
                            visited.push(nb);
                            next.push(nb);
                            if visited.len() >= budget {
                                break;
                            }
                        }
                    }
                    if visited.len() >= budget {
                        break;
                    }
                }
                frontier = next;
            }
        }
        visited.truncate(budget);
        (visited, edges)
    }

    /// p99 of the degree distribution, floored at 50 (graphify `_bfs`).
    fn hub_threshold(&self) -> usize {
        let mut degs: Vec<usize> = self.g.node_indices().map(|n| self.degree(n)).collect();
        if degs.is_empty() {
            return 50;
        }
        degs.sort_unstable();
        let p99 = degs[(degs.len() * 99) / 100];
        p99.max(50)
    }
}

// ---- Text rendering (CLI output; not exercised by the semantic gate) ----

impl Explain {
    pub fn render(&self) -> String {
        let mut out = format!(
            "Node: {}\n  ID:        {}\n  Source:    {}\n  Community: {}\n  Degree:    {}\n",
            self.label, self.id, self.source, self.community, self.degree
        );
        let mut conns = self.connections.clone();
        // graphify sorts connections by neighbour degree desc; we lack neighbour
        // degree here cheaply, so keep insertion (out-then-in) order — same set.
        conns.truncate(20);
        if !self.connections.is_empty() {
            out.push_str(&format!("\nConnections ({}):\n", self.connections.len()));
            for c in &conns {
                let arrow = if c.direction == "out" { "-->" } else { "<--" };
                out.push_str(&format!(
                    "  {arrow} {} [{}] [{}]\n",
                    c.neighbor, c.relation, c.confidence
                ));
            }
            if self.connections.len() > 20 {
                out.push_str(&format!("  ... and {} more\n", self.connections.len() - 20));
            }
        }
        out
    }
}

impl PathResult {
    pub fn render(&self, label_of: impl Fn(&str) -> String) -> String {
        let mut seg = String::new();
        if let Some(first) = self.hops.first() {
            seg.push_str(&label_of(&first.from));
        }
        for h in &self.hops {
            let conf = if h.confidence.is_empty() {
                String::new()
            } else {
                format!(" [{}]", h.confidence)
            };
            let rel = if h.relation.is_empty() {
                "related"
            } else {
                &h.relation
            };
            if h.forward {
                seg.push_str(&format!(" --{rel}{conf}--> {}", label_of(&h.to)));
            } else {
                seg.push_str(&format!(" <--{rel}{conf}-- {}", label_of(&h.to)));
            }
        }
        format!("Shortest path ({} hops):\n  {seg}", self.hops.len())
    }
}
