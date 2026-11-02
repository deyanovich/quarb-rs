//! The graph algorithms of Quarb's `graph:` command library (spec:
//! Aggregate Commands, "The graph library") and of the web level's
//! stored rank — one implementation, so `//page @| graph:pagerank(->link)`
//! equals `::pagerank` by construction. Pure functions over an edge
//! list: the engine frames the induced subgraph and lands the values;
//! this crate knows nodes as indices `0..n`.

use std::collections::HashMap;

/// The edges of a graph over the nodes `0..n`, folded the way the
/// algorithms read them: a self-loop is ignored, and parallel edges
/// are one edge — counting once when unweighted, their weights
/// added when weighted.
#[derive(Clone, Debug, PartialEq)]
pub struct Edges {
    n: usize,
    /// Per source, the target and the edge's weight, in first-seen
    /// order of the targets.
    out: Vec<Vec<(usize, f64)>>,
}

impl Edges {
    /// Fold `(source, target, weight)` triples; a weight is `None`
    /// where the graph is unweighted (every edge weighs one). A
    /// weight that is no finite number is dropped; a negative one is
    /// kept for the algorithm to refuse (`distance`) or the caller
    /// to screen.
    pub fn fold(n: usize, edges: impl IntoIterator<Item = (usize, usize, Option<f64>)>) -> Edges {
        let mut out: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
        let mut slot: HashMap<(usize, usize), usize> = HashMap::new();
        for (a, b, w) in edges {
            if a == b || a >= n || b >= n {
                continue;
            }
            match w {
                None => {
                    if let std::collections::hash_map::Entry::Vacant(e) = slot.entry((a, b)) {
                        e.insert(out[a].len());
                        out[a].push((b, 1.0));
                    }
                }
                Some(w) => {
                    if !w.is_finite() {
                        continue;
                    }
                    match slot.entry((a, b)) {
                        std::collections::hash_map::Entry::Occupied(e) => out[a][*e.get()].1 += w,
                        std::collections::hash_map::Entry::Vacant(e) => {
                            e.insert(out[a].len());
                            out[a].push((b, w));
                        }
                    }
                }
            }
        }
        Edges { n, out }
    }

    /// The number of nodes.
    pub fn len(&self) -> usize {
        self.n
    }

    /// Whether the graph has no node.
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// The distinct out-neighbours of `node`, with their weights.
    pub fn out(&self, node: usize) -> &[(usize, f64)] {
        &self.out[node]
    }

    /// The out-degree: distinct targets.
    pub fn out_degree(&self, node: usize) -> usize {
        self.out[node].len()
    }

    /// The in-degree: distinct sources.
    pub fn in_degrees(&self) -> Vec<usize> {
        let mut ins = vec![0usize; self.n];
        for targets in &self.out {
            for &(b, _) in targets {
                ins[b] += 1;
            }
        }
        ins
    }
}

/// PageRank (spec: "The graph library"): from the uniform start
/// `1/N`, `rounds` rounds in which every node takes `(1 - d)/N` plus
/// `d` times the sum over its in-neighbours of their rank times the
/// edge's share of the source's out-weight (every edge weighing one
/// when unweighted, so the share is `1/out_degree`); the rank of the
/// nodes with no out-edge is spread evenly over all. The ranks sum
/// to one. An empty graph answers nothing.
pub fn pagerank(edges: &Edges, damping: f64, rounds: usize) -> Vec<f64> {
    let n = edges.len();
    if n == 0 {
        return Vec::new();
    }
    let nf = n as f64;
    let totals: Vec<f64> = (0..n)
        .map(|i| edges.out(i).iter().map(|(_, w)| w).sum::<f64>())
        .collect();
    let mut pr = vec![1.0 / nf; n];
    for _ in 0..rounds {
        let mut next = vec![(1.0 - damping) / nf; n];
        let mut dangling = 0.0;
        for i in 0..n {
            if totals[i] <= 0.0 {
                dangling += pr[i];
                continue;
            }
            let per_weight = damping * pr[i] / totals[i];
            for &(t, w) in edges.out(i) {
                next[t] += per_weight * w;
            }
        }
        let spread = damping * dangling / nf;
        for v in &mut next {
            *v += spread;
        }
        pr = next;
    }
    pr
}

/// The web level's parameters: the damping and the rounds the
/// stored `pagerank` column is computed with.
pub const DAMPING: f64 = 0.85;
pub const ROUNDS: usize = 20;

/// Shortest distances from the seed set (spec: "The graph library",
/// `graph:distance`): along the edges as folded, a seed reads zero,
/// several seeds give the nearest, a node no seed reaches is `None`.
/// Unweighted (every edge one) it is the breadth-first count of
/// hops; weighted, Dijkstra over the folded weights, which must not
/// be negative (`Err` names the first that is).
pub fn distance(edges: &Edges, seeds: &[usize]) -> Result<Vec<Option<f64>>, String> {
    let n = edges.len();
    let mut dist: Vec<Option<f64>> = vec![None; n];
    for i in 0..n {
        for &(t, w) in edges.out(i) {
            if w < 0.0 {
                return Err(format!(
                    "a distance takes no negative weight ({w} on an edge of node {i})"
                ));
            }
            let _ = t;
        }
    }
    // Dijkstra with a binary heap on (distance, node); the
    // unweighted graph has every weight one and runs the same.
    use std::cmp::Ordering;
    use std::collections::BinaryHeap;
    #[derive(PartialEq)]
    struct At(f64, usize);
    impl Eq for At {}
    impl PartialOrd for At {
        fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
            Some(self.cmp(o))
        }
    }
    impl Ord for At {
        fn cmp(&self, o: &Self) -> Ordering {
            // a min-heap: the smaller distance is the greater item
            o.0.partial_cmp(&self.0)
                .unwrap_or(Ordering::Equal)
                .then_with(|| o.1.cmp(&self.1))
        }
    }
    let mut heap = BinaryHeap::new();
    for &s in seeds {
        if s < n && dist[s].is_none() {
            dist[s] = Some(0.0);
            heap.push(At(0.0, s));
        }
    }
    while let Some(At(d, u)) = heap.pop() {
        if dist[u].is_some_and(|x| x < d) {
            continue;
        }
        for &(v, w) in edges.out(u) {
            let nd = d + w;
            if dist[v].is_none_or(|x| nd < x) {
                dist[v] = Some(nd);
                heap.push(At(nd, v));
            }
        }
    }
    Ok(dist)
}

/// The core number of every node (spec: "The graph library",
/// `graph:kcore`): the largest k such that the node is in the
/// subgraph where every node keeps at least k neighbours. The edges
/// are read undirected and a neighbour counts once whatever the
/// weights; a node without neighbours is in core 0. The peeling
/// order of Batagelj and Zaversnik, linear in the edges.
pub fn kcore(edges: &Edges) -> Vec<usize> {
    let n = edges.len();
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for a in 0..n {
        for &(b, _) in edges.out(a) {
            if !adj[a].contains(&b) {
                adj[a].push(b);
            }
            if !adj[b].contains(&a) {
                adj[b].push(a);
            }
        }
    }
    let mut degree: Vec<usize> = adj.iter().map(Vec::len).collect();
    let max_degree = degree.iter().copied().max().unwrap_or(0);
    // bucket sort by degree
    let mut bin = vec![0usize; max_degree + 1];
    for &d in &degree {
        bin[d] += 1;
    }
    let mut start = 0;
    for b in bin.iter_mut() {
        let count = *b;
        *b = start;
        start += count;
    }
    let mut pos = vec![0usize; n];
    let mut vert = vec![0usize; n];
    for v in 0..n {
        pos[v] = bin[degree[v]];
        vert[pos[v]] = v;
        bin[degree[v]] += 1;
    }
    for d in (1..=max_degree).rev() {
        bin[d] = bin[d - 1];
    }
    if max_degree > 0 {
        bin[0] = 0;
    }
    for i in 0..n {
        let v = vert[i];
        for &u in &adj[v] {
            if degree[u] > degree[v] {
                let du = degree[u];
                let pu = pos[u];
                let pw = bin[du];
                let w = vert[pw];
                if u != w {
                    pos[u] = pw;
                    vert[pu] = w;
                    pos[w] = pu;
                    vert[pw] = u;
                }
                bin[du] += 1;
                degree[u] -= 1;
            }
        }
    }
    degree
}

/// Betweenness centrality (spec: "The graph library",
/// `graph:betweenness`), Brandes's algorithm: for every pair of
/// other nodes, the share of the shortest paths between them that
/// pass through the node, a pair with several shortest paths
/// dividing its unit among them. Over the folded edges as given —
/// every ordered pair — so an undirected graph, whose fold carries
/// each edge both ways, is halved by the caller (each unordered pair
/// once). Unweighted it counts hops; weighted it takes the folded
/// weights as lengths (Dijkstra; a negative weight is an `Err`).
pub fn betweenness(edges: &Edges, weighted: bool) -> Result<Vec<f64>, String> {
    let n = edges.len();
    let mut cb = vec![0.0f64; n];
    if weighted {
        for i in 0..n {
            for &(_, w) in edges.out(i) {
                if w < 0.0 {
                    return Err(format!(
                        "a betweenness takes no negative weight ({w} on an edge of node {i})"
                    ));
                }
            }
        }
    }
    for s in 0..n {
        let (mut stack, pred, sigma) = shortest_paths(edges, s, weighted);
        // accumulation, farthest first
        let mut delta = vec![0.0f64; n];
        while let Some(w) = stack.pop() {
            for &v in &pred[w] {
                delta[v] += sigma[v] / sigma[w] * (1.0 + delta[w]);
            }
            if w != s {
                cb[w] += delta[w];
            }
        }
    }
    Ok(cb)
}

/// The connected components (spec: "The graph library",
/// `graph:conncomp`): each node's component, numbered from 1 in the
/// order in which the components' first nodes stand. Over the
/// folded edges as given: the strongly connected components of a
/// directed graph (Tarjan), which over an undirected fold — every
/// edge both ways — are the connected components. A node without an
/// edge is its own component.
pub fn conncomp(edges: &Edges) -> Vec<usize> {
    let n = edges.len();
    // Tarjan, iterative
    let mut index = vec![usize::MAX; n];
    let mut low = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut comp = vec![usize::MAX; n];
    let mut next_index = 0;
    let mut next_comp = 0;
    for root in 0..n {
        if index[root] != usize::MAX {
            continue;
        }
        // (node, position in its out list)
        let mut work: Vec<(usize, usize)> = vec![(root, 0)];
        index[root] = next_index;
        low[root] = next_index;
        next_index += 1;
        stack.push(root);
        on_stack[root] = true;
        while let Some(&mut (v, ref mut i)) = work.last_mut() {
            if *i < edges.out(v).len() {
                let w = edges.out(v)[*i].0;
                *i += 1;
                if index[w] == usize::MAX {
                    index[w] = next_index;
                    low[w] = next_index;
                    next_index += 1;
                    stack.push(w);
                    on_stack[w] = true;
                    work.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
            } else {
                work.pop();
                if let Some(&(u, _)) = work.last() {
                    low[u] = low[u].min(low[v]);
                }
                if low[v] == index[v] {
                    while let Some(w) = stack.pop() {
                        on_stack[w] = false;
                        comp[w] = next_comp;
                        if w == v {
                            break;
                        }
                    }
                    next_comp += 1;
                }
            }
        }
    }
    // number the components by their first node in context order
    let mut number: Vec<usize> = vec![0; next_comp];
    let mut seen = 0;
    for &c in &comp {
        if number[c] == 0 {
            seen += 1;
            number[c] = seen;
        }
    }
    comp.into_iter().map(|c| number[c]).collect()
}

impl Edges {
    /// The undirected reading: every edge in both directions, weight
    /// one, parallel edges one. What the undirected notions (bridges,
    /// articulation points, triangles) read a directed hop as.
    pub fn symmetric(&self) -> Edges {
        let n = self.n;
        Edges::fold(
            n,
            (0..n).flat_map(|a| {
                self.out[a]
                    .iter()
                    .flat_map(move |&(b, _)| [(a, b, None), (b, a, None)])
            }),
        )
    }
}

/// The depth-first numbering Tarjan's bridge and articulation-point
/// algorithms share, over the undirected reading: discovery index,
/// low-link, the DFS parent, and the tree children per node.
struct Lowlink {
    disc: Vec<usize>,
    low: Vec<usize>,
    parent: Vec<usize>,
    children: Vec<usize>,
}

fn lowlink(g: &Edges) -> Lowlink {
    let n = g.len();
    let mut disc = vec![usize::MAX; n];
    let mut low = vec![0usize; n];
    let mut parent = vec![usize::MAX; n];
    let mut children = vec![0usize; n];
    let mut next = 0;
    for root in 0..n {
        if disc[root] != usize::MAX {
            continue;
        }
        disc[root] = next;
        low[root] = next;
        next += 1;
        // (node, position in its neighbour list)
        let mut work: Vec<(usize, usize)> = vec![(root, 0)];
        while let Some(&mut (v, ref mut i)) = work.last_mut() {
            if *i < g.out(v).len() {
                let w = g.out(v)[*i].0;
                *i += 1;
                if disc[w] == usize::MAX {
                    disc[w] = next;
                    low[w] = next;
                    next += 1;
                    parent[w] = v;
                    children[v] += 1;
                    work.push((w, 0));
                } else if w != parent[v] {
                    low[v] = low[v].min(disc[w]);
                }
            } else {
                work.pop();
                if let Some(&(u, _)) = work.last() {
                    low[u] = low[u].min(low[v]);
                }
            }
        }
    }
    Lowlink {
        disc,
        low,
        parent,
        children,
    }
}

/// The articulation points (spec: "The graph library",
/// `graph:artpoints`): whether removing the node disconnects the
/// graph it stands in, read undirected whatever the edges'
/// direction. Tarjan: a DFS root with two or more tree children, or
/// another node with a child whose low-link does not climb above
/// the node's discovery.
pub fn artpoints(edges: &Edges) -> Vec<bool> {
    let g = edges.symmetric();
    let n = g.len();
    let t = lowlink(&g);
    let mut cut = vec![false; n];
    for w in 0..n {
        let v = t.parent[w];
        if v == usize::MAX {
            continue;
        }
        if t.parent[v] == usize::MAX {
            // a root cuts when it has two or more tree children
            cut[v] = t.children[v] >= 2;
        } else if t.low[w] >= t.disc[v] {
            cut[v] = true;
        }
    }
    cut
}

/// The bridges (spec: "The graph library", `graph:bridges`): the
/// edges whose removal disconnects the graph, read undirected, each
/// as the pair `(smaller, larger)` of its nodes, in that order of
/// pairs. Tarjan: a tree edge whose child's low-link does not reach
/// back to the parent. Parallel edges are one edge in the fold, so a
/// doubled edge is a bridge as a single one is.
pub fn bridges(edges: &Edges) -> Vec<(usize, usize)> {
    let g = edges.symmetric();
    let t = lowlink(&g);
    let mut out: Vec<(usize, usize)> = (0..g.len())
        .filter(|&w| t.parent[w] != usize::MAX && t.low[w] > t.disc[t.parent[w]])
        .map(|w| {
            let v = t.parent[w];
            (v.min(w), v.max(w))
        })
        .collect();
    out.sort_unstable();
    out
}

/// Edge betweenness (spec: "The graph library",
/// `graph:betweenness(--edges)`): for every edge, the share of the
/// shortest paths between all ordered pairs that run along it, from
/// the same Brandes pass as the nodes' measure, aligned with
/// `edges.out(i)`: `result[i][k]` is the value of the edge
/// `edges.out(i)[k]`. Over the folded edges as given; an undirected
/// fold carries each edge both ways and the caller folds the two
/// values. Weighted, the folded weights are lengths (a negative one
/// is an `Err`).
pub fn edge_betweenness(edges: &Edges, weighted: bool) -> Result<Vec<Vec<f64>>, String> {
    let n = edges.len();
    let mut eb: Vec<Vec<f64>> = (0..n).map(|i| vec![0.0; edges.out(i).len()]).collect();
    // the position of the edge v -> w in v's out list
    let slot = |v: usize, w: usize| edges.out(v).iter().position(|&(t, _)| t == w);
    if weighted {
        for i in 0..n {
            for &(_, w) in edges.out(i) {
                if w < 0.0 {
                    return Err(format!(
                        "a betweenness takes no negative weight ({w} on an edge of node {i})"
                    ));
                }
            }
        }
    }
    for s in 0..n {
        let (stack, pred, sigma) = shortest_paths(edges, s, weighted);
        let mut delta = vec![0.0f64; n];
        let mut stack = stack;
        while let Some(w) = stack.pop() {
            for &v in &pred[w] {
                let c = sigma[v] / sigma[w] * (1.0 + delta[w]);
                if let Some(k) = slot(v, w) {
                    eb[v][k] += c;
                }
                delta[v] += c;
            }
        }
    }
    Ok(eb)
}

/// Single-source shortest paths for Brandes: the nodes in order of
/// non-decreasing distance, the predecessors on shortest paths, the
/// path counts.
fn shortest_paths(
    edges: &Edges,
    s: usize,
    weighted: bool,
) -> (Vec<usize>, Vec<Vec<usize>>, Vec<f64>) {
    let n = edges.len();
    let mut stack: Vec<usize> = Vec::with_capacity(n);
    let mut pred: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut sigma = vec![0.0f64; n];
    let mut dist: Vec<Option<f64>> = vec![None; n];
    sigma[s] = 1.0;
    dist[s] = Some(0.0);
    if weighted {
        let mut heap = std::collections::BinaryHeap::new();
        heap.push(Nearest(0.0, s));
        let mut settled = vec![false; n];
        while let Some(Nearest(d, v)) = heap.pop() {
            if settled[v] || dist[v].is_some_and(|x| x < d) {
                continue;
            }
            settled[v] = true;
            stack.push(v);
            for &(w, len) in edges.out(v) {
                let nd = d + len;
                match dist[w] {
                    Some(x) if (x - nd).abs() <= 1e-12 * x.abs().max(1.0) => {
                        sigma[w] += sigma[v];
                        pred[w].push(v);
                    }
                    Some(x) if x > nd => {
                        dist[w] = Some(nd);
                        sigma[w] = sigma[v];
                        pred[w] = vec![v];
                        heap.push(Nearest(nd, w));
                    }
                    Some(_) => {}
                    None => {
                        dist[w] = Some(nd);
                        sigma[w] = sigma[v];
                        pred[w] = vec![v];
                        heap.push(Nearest(nd, w));
                    }
                }
            }
        }
    } else {
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(s);
        while let Some(v) = queue.pop_front() {
            stack.push(v);
            let dv = dist[v].expect("queued");
            for &(w, _) in edges.out(v) {
                match dist[w] {
                    None => {
                        dist[w] = Some(dv + 1.0);
                        queue.push_back(w);
                        sigma[w] += sigma[v];
                        pred[w].push(v);
                    }
                    Some(dw) if dw == dv + 1.0 => {
                        sigma[w] += sigma[v];
                        pred[w].push(v);
                    }
                    Some(_) => {}
                }
            }
        }
    }
    (stack, pred, sigma)
}

/// A heap item for Dijkstra: the smaller distance is the greater
/// item, ties by the smaller node.
#[derive(PartialEq)]
struct Nearest(f64, usize);
impl Eq for Nearest {}
impl PartialOrd for Nearest {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Nearest {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        o.0.partial_cmp(&self.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| o.1.cmp(&self.1))
    }
}

/// The shortest route from the seed set to every node (spec: "The
/// graph library", `graph:route`): the nodes in order, the seed
/// first and the node itself last, the seed's own route the
/// one-element list, `None` where no seed reaches. Several seeds
/// give the nearest. Along the edges as folded: unweighted, the
/// fewest hops; weighted, Dijkstra over the folded weights (a
/// negative one is an `Err`). Where two routes tie, the predecessor
/// with the smaller index wins, so the route is deterministic for a
/// given numbering (the engine numbers nodes in document order).
pub fn route(
    edges: &Edges,
    seeds: &[usize],
    weighted: bool,
) -> Result<Vec<Option<Vec<usize>>>, String> {
    let n = edges.len();
    for i in 0..n {
        for &(_, w) in edges.out(i) {
            if weighted && w < 0.0 {
                return Err(format!(
                    "a route takes no negative weight ({w} on an edge of node {i})"
                ));
            }
        }
    }
    let mut dist: Vec<Option<f64>> = vec![None; n];
    let mut pred: Vec<usize> = vec![usize::MAX; n];
    let mut heap = std::collections::BinaryHeap::new();
    for &s in seeds {
        if s < n && dist[s].is_none() {
            dist[s] = Some(0.0);
            heap.push(Nearest(0.0, s));
        }
    }
    while let Some(Nearest(d, u)) = heap.pop() {
        if dist[u].is_some_and(|x| x < d) {
            continue;
        }
        for &(v, w) in edges.out(u) {
            let nd = d + if weighted { w } else { 1.0 };
            match dist[v] {
                None => {
                    dist[v] = Some(nd);
                    pred[v] = u;
                    heap.push(Nearest(nd, v));
                }
                Some(x) if nd < x - 1e-12 * x.abs().max(1.0) => {
                    dist[v] = Some(nd);
                    pred[v] = u;
                    heap.push(Nearest(nd, v));
                }
                Some(x) if (nd - x).abs() <= 1e-12 * x.abs().max(1.0) && u < pred[v] => {
                    pred[v] = u;
                }
                Some(_) => {}
            }
        }
    }
    Ok((0..n)
        .map(|v| {
            dist[v]?;
            let mut path = vec![v];
            let mut at = v;
            while pred[at] != usize::MAX {
                at = pred[at];
                path.push(at);
            }
            path.reverse();
            Some(path)
        })
        .collect())
}

/// The triangles through every node (spec: "The graph library",
/// `graph:triangles`): the pairs of its distinct neighbours that are
/// neighbours of each other, read undirected.
pub fn triangles(edges: &Edges) -> Vec<usize> {
    let g = edges.symmetric();
    let n = g.len();
    let nbrs: Vec<Vec<usize>> = (0..n)
        .map(|v| {
            let mut a: Vec<usize> = g.out(v).iter().map(|&(t, _)| t).collect();
            a.sort_unstable();
            a
        })
        .collect();
    (0..n)
        .map(|v| {
            let nv = &nbrs[v];
            let mut t = 0;
            for (i, &a) in nv.iter().enumerate() {
                for &b in &nv[i + 1..] {
                    if nbrs[a].binary_search(&b).is_ok() {
                        t += 1;
                    }
                }
            }
            t
        })
        .collect()
}

/// The local clustering coefficient (spec: "The graph library",
/// `graph:triangles(--coefficient)`): `2t / (k (k - 1))` for a node
/// with `t` triangles and `k` distinct neighbours, zero when `k < 2`.
pub fn clustering(edges: &Edges) -> Vec<f64> {
    let g = edges.symmetric();
    triangles(edges)
        .into_iter()
        .enumerate()
        .map(|(v, t)| {
            let k = g.out_degree(v) as f64;
            if k < 2.0 {
                0.0
            } else {
                2.0 * t as f64 / (k * (k - 1.0))
            }
        })
        .collect()
}

/// Closeness centrality (spec: "The graph library", `graph:closeness`),
/// Wasserman and Faust's form for a graph that need not be connected:
/// with `r` the other nodes the node reaches along the edges as
/// folded and `n` the nodes, `r/(n-1) × r/Σd`; a node adjacent to
/// every other reads one, a node reaching none zero, and a connected
/// graph gives the inverse of the mean distance. `harmonic` answers
/// `Σ 1/d` over the others divided by `n-1` instead, an unreached
/// node adding nothing. Distances as `distance` measures them, in
/// the edges' direction.
pub fn closeness(edges: &Edges, weighted: bool, harmonic: bool) -> Result<Vec<f64>, String> {
    let n = edges.len();
    if n < 2 {
        return Ok(vec![0.0; n]);
    }
    let unit = if weighted {
        edges.clone()
    } else {
        Edges::fold(
            n,
            (0..n).flat_map(|a| edges.out(a).iter().map(move |&(b, _)| (a, b, None))),
        )
    };
    let mut out = Vec::with_capacity(n);
    for s in 0..n {
        let dist = distance(&unit, &[s]).map_err(|e| e.replace("a distance", "a closeness"))?;
        let mut reached = 0.0f64;
        let mut total = 0.0f64;
        let mut inverse = 0.0f64;
        for (v, d) in dist.iter().enumerate() {
            if v == s {
                continue;
            }
            if let Some(d) = d
                && *d > 0.0
            {
                reached += 1.0;
                total += d;
                inverse += 1.0 / d;
            }
        }
        let others = (n - 1) as f64;
        out.push(if harmonic {
            inverse / others
        } else if reached == 0.0 {
            0.0
        } else {
            (reached / others) * (reached / total)
        });
    }
    Ok(out)
}

/// Modularity (spec: "The graph library", `graph:modularity`) of the
/// partition `part` (a community index per node): Newman and
/// Girvan's measure, the share of the edge weight inside the parts
/// minus the share a random graph of the same degrees would place
/// there, scaled by `resolution`. Over the folded edges as given:
/// an undirected fold (each edge both ways) gives the original form,
/// the degrees symmetric and the weight counted from both ends; a
/// directed fold gives Leicht and Newman's form, the out- and
/// in-degrees of each part multiplied.
pub fn modularity(edges: &Edges, resolution: f64, part: &[usize]) -> f64 {
    let n = edges.len();
    let mut out_deg = vec![0.0f64; n];
    let mut in_deg = vec![0.0f64; n];
    let mut total = 0.0f64;
    let mut inside = 0.0f64;
    for a in 0..n {
        for &(b, w) in edges.out(a) {
            out_deg[a] += w;
            in_deg[b] += w;
            total += w;
            if part.get(a) == part.get(b) {
                inside += w;
            }
        }
    }
    if total == 0.0 {
        return 0.0;
    }
    let communities = part.iter().copied().max().map_or(0, |c| c + 1);
    let mut k_out = vec![0.0f64; communities];
    let mut k_in = vec![0.0f64; communities];
    for v in 0..n {
        if let Some(&c) = part.get(v) {
            k_out[c] += out_deg[v];
            k_in[c] += in_deg[v];
        }
    }
    let expected: f64 = (0..communities).map(|c| k_out[c] * k_in[c]).sum::<f64>() / (total * total);
    inside / total - resolution * expected
}

/// The undirected weighted adjacency the community detection works
/// on: both directions merged, parallel edges' weights added.
struct Undirected {
    adj: Vec<Vec<(usize, f64)>>,
    degree: Vec<f64>,
    /// Twice the total edge weight (every edge counted from both ends).
    two_m: f64,
}

impl Undirected {
    fn of(edges: &Edges) -> Undirected {
        let n = edges.len();
        let mut weight: Vec<std::collections::BTreeMap<usize, f64>> = vec![Default::default(); n];
        for a in 0..n {
            for &(b, w) in edges.out(a) {
                *weight[a].entry(b).or_insert(0.0) += w;
                *weight[b].entry(a).or_insert(0.0) += w;
            }
        }
        Undirected::from_maps(weight)
    }

    fn from_maps(weight: Vec<std::collections::BTreeMap<usize, f64>>) -> Undirected {
        let adj: Vec<Vec<(usize, f64)>> = weight
            .into_iter()
            .map(|m| m.into_iter().collect())
            .collect();
        let degree: Vec<f64> = adj.iter().map(|a| a.iter().map(|(_, w)| w).sum()).collect();
        let two_m = degree.iter().sum();
        Undirected { adj, degree, two_m }
    }

    fn len(&self) -> usize {
        self.adj.len()
    }
}

/// Community detection (spec: "The graph library",
/// `graph:communities`), the Leiden algorithm of Traag, Waltman and
/// van Eck (2019) towards modularity at `resolution`: from one node
/// per community, nodes moved in index order to the neighbouring
/// community of the greatest gain (a tie to the earlier community),
/// each community then refined from singletons into well-joined
/// parts, the refined parts aggregated into one node each and the
/// moving repeated on the aggregate, until nothing moves. The edges
/// are read without direction, a repeated edge adding its weight.
/// Each node's community is numbered from one in the order in which
/// the communities' first nodes stand.
pub fn communities(edges: &Edges, resolution: f64) -> Vec<usize> {
    let n = edges.len();
    if n == 0 {
        return Vec::new();
    }
    let mut graph = Undirected::of(edges);
    let mut membership: Vec<usize> = (0..n).collect();
    // which original nodes each aggregate node stands for
    let mut stands_for: Vec<Vec<usize>> = (0..n).map(|v| vec![v]).collect();
    loop {
        let mut part: Vec<usize> = (0..graph.len()).collect();
        let moved = move_nodes(&graph, &mut part, resolution);
        for (agg, originals) in stands_for.iter().enumerate() {
            for &o in originals {
                membership[o] = part[agg];
            }
        }
        let refined = refine(&graph, &part, resolution);
        let parts = refined.iter().copied().max().map_or(0, |m| m + 1);
        if !moved || parts == graph.len() {
            break;
        }
        // aggregate on the refined partition
        let mut next_stands: Vec<Vec<usize>> = vec![Vec::new(); parts];
        for (agg, originals) in stands_for.iter().enumerate() {
            next_stands[refined[agg]].extend(originals.iter().copied());
        }
        let mut weight: Vec<std::collections::BTreeMap<usize, f64>> =
            vec![Default::default(); parts];
        for a in 0..graph.len() {
            for &(b, w) in &graph.adj[a] {
                *weight[refined[a]].entry(refined[b]).or_insert(0.0) += w;
            }
        }
        graph = Undirected::from_maps(weight);
        stands_for = next_stands;
    }
    // numbered from one by the first node of each community
    let mut seen: Vec<Option<usize>> = vec![None; n];
    let mut next = 1;
    let mut out = vec![0usize; n];
    for v in 0..n {
        let c = membership[v];
        let number = match seen[c] {
            Some(k) => k,
            None => {
                seen[c] = Some(next);
                next += 1;
                next - 1
            }
        };
        out[v] = number;
    }
    out
}

/// Renumber a partition densely by first appearance.
fn dense(part: &mut [usize]) {
    let mut map: Vec<Option<usize>> = vec![None; part.len()];
    let mut next = 0;
    for c in part.iter_mut() {
        let k = match map[*c] {
            Some(k) => k,
            None => {
                map[*c] = Some(next);
                next += 1;
                next - 1
            }
        };
        *c = k;
    }
}

/// The modularity gain of a node of degree `k` joining a community
/// with `into` weight towards it and `community_degree` in all, up to
/// the shared factor `1/(2m)`.
fn gain(g: &Undirected, v: usize, into: f64, community_degree: f64, resolution: f64) -> f64 {
    into - resolution * g.degree[v] * community_degree / g.two_m
}

/// Leiden's fast local moving: nodes in index order, each moved to
/// the neighbouring community of the greatest gain, a tie keeping
/// the earlier community, until a full pass moves nothing. Whether
/// anything moved; the partition numbered densely after.
fn move_nodes(g: &Undirected, part: &mut [usize], resolution: f64) -> bool {
    let n = g.len();
    let mut community_degree: Vec<f64> = vec![0.0; n];
    for v in 0..n {
        community_degree[part[v]] += g.degree[v];
    }
    let mut moved_any = false;
    loop {
        let mut moved = false;
        for v in 0..n {
            let from = part[v];
            let mut into: std::collections::BTreeMap<usize, f64> = Default::default();
            for &(u, w) in &g.adj[v] {
                if u != v {
                    *into.entry(part[u]).or_insert(0.0) += w;
                }
            }
            let stay = gain(
                g,
                v,
                into.get(&from).copied().unwrap_or(0.0),
                community_degree[from] - g.degree[v],
                resolution,
            );
            let mut best = (stay, from);
            for (&c, &w) in &into {
                if c == from {
                    continue;
                }
                let value = gain(g, v, w, community_degree[c], resolution);
                if value > best.0 + 1e-12 || ((value - best.0).abs() <= 1e-12 && c < best.1) {
                    best = (value, c);
                }
            }
            if best.1 != from {
                community_degree[from] -= g.degree[v];
                community_degree[best.1] += g.degree[v];
                part[v] = best.1;
                moved = true;
                moved_any = true;
            }
        }
        if !moved {
            break;
        }
    }
    dense(part);
    moved_any
}

/// Leiden's refinement: inside each community of `part`, from
/// singletons, each node still alone (in index order) joins the
/// part of its own community that gains most, when any gains; the
/// refined partition, numbered densely.
fn refine(g: &Undirected, part: &[usize], resolution: f64) -> Vec<usize> {
    let n = g.len();
    let mut refined: Vec<usize> = (0..n).collect();
    let mut sub_degree: Vec<f64> = g.degree.clone();
    let mut sub_size: Vec<usize> = vec![1; n];
    for v in 0..n {
        if sub_size[refined[v]] != 1 {
            continue;
        }
        let mut into: std::collections::BTreeMap<usize, f64> = Default::default();
        for &(u, w) in &g.adj[v] {
            if u != v && part[u] == part[v] {
                *into.entry(refined[u]).or_insert(0.0) += w;
            }
        }
        let mut best: Option<(f64, usize)> = None;
        for (&c, &w) in &into {
            if c == refined[v] {
                continue;
            }
            let value = gain(g, v, w, sub_degree[c], resolution);
            if value > 0.0
                && best.is_none_or(|(b, bc)| {
                    value > b + 1e-12 || ((value - b).abs() <= 1e-12 && c < bc)
                })
            {
                best = Some((value, c));
            }
        }
        if let Some((_, c)) = best {
            let from = refined[v];
            sub_degree[from] -= g.degree[v];
            sub_size[from] -= 1;
            sub_degree[c] += g.degree[v];
            sub_size[c] += 1;
            refined[v] = c;
        }
    }
    dense(&mut refined);
    refined
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-12
    }

    #[test]
    fn a_cycle_ranks_evenly_and_sums_to_one() {
        let e = Edges::fold(3, [(0, 1, None), (1, 2, None), (2, 0, None)]);
        let pr = pagerank(&e, DAMPING, ROUNDS);
        assert!(pr.iter().all(|&r| close(r, 1.0 / 3.0)), "{pr:?}");
        assert!(close(pr.iter().sum::<f64>(), 1.0));
    }

    #[test]
    fn dangling_mass_is_spread_and_self_loops_and_repeats_are_folded() {
        // 0 -> 1 twice, 1 -> 1: one edge 0 -> 1, node 1 dangling
        let e = Edges::fold(2, [(0, 1, None), (0, 1, None), (1, 1, None)]);
        assert_eq!(e.out_degree(0), 1);
        assert_eq!(e.out_degree(1), 0);
        let pr = pagerank(&e, DAMPING, ROUNDS);
        assert!(close(pr.iter().sum::<f64>(), 1.0));
        assert!(pr[1] > pr[0]);
        // the same numbers as the web level's hand-rolled loop
        let (n, d) = (2.0, DAMPING);
        let (mut a, mut b) = (0.5, 0.5);
        for _ in 0..ROUNDS {
            let spread = d * b / n;
            let (na, nb) = ((1.0 - d) / n + spread, (1.0 - d) / n + d * a + spread);
            a = na;
            b = nb;
        }
        assert!(close(pr[0], a) && close(pr[1], b), "{pr:?} vs {a} {b}");
    }

    #[test]
    fn weights_share_the_rank_in_proportion_and_parallel_edges_add() {
        // 0 -> 1 weight 3, 0 -> 2 weight 1: node 1 takes three
        // quarters of what 0 gives
        let e = Edges::fold(3, [(0, 1, Some(3.0)), (0, 2, Some(1.0))]);
        let pr = pagerank(&e, DAMPING, 1);
        let given = DAMPING * (1.0 / 3.0);
        let base = (1.0 - DAMPING) / 3.0 + DAMPING * (2.0 / 3.0) / 3.0;
        assert!(close(pr[1], base + given * 0.75), "{pr:?}");
        assert!(close(pr[2], base + given * 0.25), "{pr:?}");
        // parallel weighted edges add: 0 -> 1 (2) and 0 -> 1 (1) is 0 -> 1 (3)
        let e2 = Edges::fold(3, [(0, 1, Some(2.0)), (0, 1, Some(1.0)), (0, 2, Some(1.0))]);
        let same = |a: &[f64], b: &[f64]| a.iter().zip(b).all(|(x, y)| close(*x, *y));
        assert!(same(&pagerank(&e2, DAMPING, 1), &pr));
        // a uniform weight is the unweighted graph
        let u = Edges::fold(3, [(0, 1, Some(7.0)), (0, 2, Some(7.0))]);
        let w = Edges::fold(3, [(0, 1, None), (0, 2, None)]);
        assert!(same(
            &pagerank(&u, DAMPING, ROUNDS),
            &pagerank(&w, DAMPING, ROUNDS)
        ));
    }

    #[test]
    fn distances_count_hops_or_sum_weights_from_the_nearest_seed() {
        // 0 -> 1 -> 2 -> 3, 0 -> 3 (weight 10), seed 0
        let e = Edges::fold(5, [(0, 1, None), (1, 2, None), (2, 3, None), (0, 3, None)]);
        assert_eq!(
            distance(&e, &[0]).unwrap(),
            [Some(0.0), Some(1.0), Some(2.0), Some(1.0), None]
        );
        let w = Edges::fold(
            4,
            [
                (0, 1, Some(1.0)),
                (1, 2, Some(1.0)),
                (2, 3, Some(1.0)),
                (0, 3, Some(10.0)),
            ],
        );
        assert_eq!(
            distance(&w, &[0]).unwrap(),
            [Some(0.0), Some(1.0), Some(2.0), Some(3.0)]
        );
        // two seeds: the nearest; a seed outside the graph is nothing
        assert_eq!(
            distance(&e, &[3, 9]).unwrap(),
            [None, None, None, Some(0.0), None]
        );
        assert!(distance(&Edges::fold(2, [(0, 1, Some(-1.0))]), &[0]).is_err());
    }

    #[test]
    fn core_numbers_peel_the_graph() {
        // a triangle 0-1-2 with a tail 2-3 and an isolated 4: the
        // triangle is core 2, the tail core 1, the island core 0
        let e = Edges::fold(5, [(0, 1, None), (1, 2, None), (2, 0, None), (2, 3, None)]);
        assert_eq!(kcore(&e), [2, 2, 2, 1, 0]);
        // direction and repeats do not matter
        let both = Edges::fold(
            5,
            [
                (0, 1, None),
                (1, 0, None),
                (1, 2, None),
                (2, 0, None),
                (2, 3, None),
                (3, 2, None),
            ],
        );
        assert_eq!(kcore(&both), [2, 2, 2, 1, 0]);
    }

    #[test]
    fn betweenness_counts_the_shortest_paths_through_a_node() {
        // a path 0 - 1 - 2 - 3, undirected (both ways): the middle
        // nodes carry the pairs that cross them — 1 carries (0,2),
        // (0,3); 2 carries (0,3), (1,3) — twice over ordered pairs
        let mut both = Vec::new();
        for (a, b) in [(0, 1), (1, 2), (2, 3)] {
            both.push((a, b, None));
            both.push((b, a, None));
        }
        let e = Edges::fold(4, both);
        let cb: Vec<f64> = betweenness(&e, false)
            .unwrap()
            .into_iter()
            .map(|x| x / 2.0)
            .collect();
        assert_eq!(cb, [0.0, 2.0, 2.0, 0.0]);
        // a diamond 0 -> 1 -> 3, 0 -> 2 -> 3: the pair (0,3) splits
        // its unit between 1 and 2
        let d = Edges::fold(4, [(0, 1, None), (0, 2, None), (1, 3, None), (2, 3, None)]);
        assert_eq!(betweenness(&d, false).unwrap(), [0.0, 0.5, 0.5, 0.0]);
        // weighted: a long direct edge loses to the two-step way
        let w = Edges::fold(3, [(0, 1, Some(1.0)), (1, 2, Some(1.0)), (0, 2, Some(5.0))]);
        assert_eq!(betweenness(&w, true).unwrap(), [0.0, 1.0, 0.0]);
        assert!(betweenness(&Edges::fold(2, [(0, 1, Some(-1.0))]), true).is_err());
    }

    #[test]
    fn components_are_numbered_by_their_first_node() {
        // 0 <-> 1, 2 -> 3 (one way), 4 alone: strongly, {0,1} {2}
        // {3} {4}; undirected, {0,1} {2,3} {4}
        let directed = Edges::fold(5, [(0, 1, None), (1, 0, None), (2, 3, None)]);
        assert_eq!(conncomp(&directed), [1, 1, 2, 3, 4]);
        let undirected = Edges::fold(5, [(0, 1, None), (1, 0, None), (2, 3, None), (3, 2, None)]);
        assert_eq!(conncomp(&undirected), [1, 1, 2, 2, 3]);
        // the numbering follows the first node, whatever the search
        // order found first
        let late = Edges::fold(4, [(3, 2, None), (2, 3, None), (0, 1, None), (1, 0, None)]);
        assert_eq!(conncomp(&late), [1, 1, 2, 2]);
    }

    #[test]
    fn an_empty_graph_answers_nothing() {
        assert!(pagerank(&Edges::fold(0, []), DAMPING, ROUNDS).is_empty());
    }

    #[test]
    fn bridges_and_articulation_points_read_undirected() {
        // a triangle 0-1-2 with a pendant 3 on 0 and a path 3-4
        let e = Edges::fold(
            5,
            [
                (0, 1, None),
                (1, 2, None),
                (2, 0, None),
                (0, 3, None),
                (3, 4, None),
            ],
        );
        assert_eq!(artpoints(&e), vec![true, false, false, true, false]);
        assert_eq!(bridges(&e), vec![(0, 3), (3, 4)]);
        // the same graph with every edge the other way reads alike
        let r = Edges::fold(
            5,
            [
                (1, 0, None),
                (2, 1, None),
                (0, 2, None),
                (3, 0, None),
                (4, 3, None),
            ],
        );
        assert_eq!(artpoints(&r), artpoints(&e));
        assert_eq!(bridges(&r), bridges(&e));
        // a bowtie: two triangles sharing node 2, no bridge, one cut
        let b = Edges::fold(
            5,
            [
                (0, 1, None),
                (1, 2, None),
                (2, 0, None),
                (2, 3, None),
                (3, 4, None),
                (4, 2, None),
            ],
        );
        assert_eq!(artpoints(&b), vec![false, false, true, false, false]);
        assert!(bridges(&b).is_empty());
        // a path: every inner node cuts, every edge is a bridge
        let p = Edges::fold(4, [(0, 1, None), (1, 2, None), (2, 3, None)]);
        assert_eq!(artpoints(&p), vec![false, true, true, false]);
        assert_eq!(bridges(&p), vec![(0, 1), (1, 2), (2, 3)]);
        // a root with two children cuts; an isolated node does not
        let star = Edges::fold(4, [(0, 1, None), (0, 2, None)]);
        assert_eq!(artpoints(&star), vec![true, false, false, false]);
        assert_eq!(bridges(&star), vec![(0, 1), (0, 2)]);
    }

    #[test]
    fn edge_betweenness_counts_the_paths_along_each_edge() {
        // a directed path 0 -> 1 -> 2: the pairs (0,1), (0,2) run along
        // the first edge, (0,2), (1,2) along the second
        let e = Edges::fold(3, [(0, 1, None), (1, 2, None)]);
        let eb = edge_betweenness(&e, false).unwrap();
        assert_eq!(eb, vec![vec![2.0], vec![2.0], vec![]]);
        // a square 0-1-3, 0-2-3 undirected: 0 to 3 splits between the
        // two routes, so each edge carries half of that pair twice
        let sq =
            Edges::fold(4, [(0, 1, None), (1, 3, None), (0, 2, None), (2, 3, None)]).symmetric();
        let eb = edge_betweenness(&sq, false).unwrap();
        let total: f64 = eb.iter().flatten().sum();
        // ordered pairs of distinct nodes: 12, each path's length in edges
        // summed: 8 pairs at distance 1 + 4 pairs at distance 2 = 16
        assert!(close(total, 16.0), "{eb:?}");
        let e01 = eb[0][sq.out(0).iter().position(|&(t, _)| t == 1).unwrap()];
        assert!(close(e01, 1.0 + 0.5 + 0.5), "{e01}");
        // weighted: the long way round carries nothing
        let w = Edges::fold(3, [(0, 1, Some(1.0)), (1, 2, Some(1.0)), (0, 2, Some(5.0))]);
        let eb = edge_betweenness(&w, true).unwrap();
        assert_eq!(eb[0], vec![2.0, 0.0]);
        assert!(edge_betweenness(&Edges::fold(2, [(0, 1, Some(-1.0))]), true).is_err());
    }

    #[test]
    fn routes_are_shortest_and_deterministic() {
        let p = Edges::fold(4, [(0, 1, None), (1, 2, None), (2, 3, None)]);
        let r = route(&p, &[0], false).unwrap();
        assert_eq!(r[0], Some(vec![0]));
        assert_eq!(r[3], Some(vec![0, 1, 2, 3]));
        // unreached: the edges point away from 3
        assert_eq!(route(&p, &[3], false).unwrap()[0], None);
        // two seeds: the nearest wins
        let r = route(&p, &[0, 3], false).unwrap();
        assert_eq!(
            r[2],
            Some(vec![0, 1, 2]),
            "3 reaches nothing along the arrows"
        );
        let both = p.symmetric();
        let r = route(&both, &[0, 3], false).unwrap();
        assert_eq!(r[2], Some(vec![3, 2]));
        assert_eq!(r[1], Some(vec![0, 1]));
        // a tie: 0-1-3 and 0-2-3 are equal, the predecessor with the
        // smaller index (1) wins, whatever the edge order
        for edges in [
            vec![(0, 1, None), (1, 3, None), (0, 2, None), (2, 3, None)],
            vec![(0, 2, None), (2, 3, None), (0, 1, None), (1, 3, None)],
        ] {
            let sq = Edges::fold(4, edges);
            assert_eq!(route(&sq, &[0], false).unwrap()[3], Some(vec![0, 1, 3]));
        }
        // weighted: the light route, not the fewest hops
        let w = Edges::fold(3, [(0, 1, Some(1.0)), (1, 2, Some(1.0)), (0, 2, Some(5.0))]);
        assert_eq!(route(&w, &[0], true).unwrap()[2], Some(vec![0, 1, 2]));
        assert_eq!(route(&w, &[0], false).unwrap()[2], Some(vec![0, 2]));
        assert!(route(&Edges::fold(2, [(0, 1, Some(-1.0))]), &[0], true).is_err());
    }

    #[test]
    fn triangles_and_the_clustering_coefficient() {
        let tri = Edges::fold(3, [(0, 1, None), (1, 2, None), (2, 0, None)]);
        assert_eq!(triangles(&tri), vec![1, 1, 1]);
        assert!(clustering(&tri).iter().all(|&c| close(c, 1.0)));
        let sq = Edges::fold(4, [(0, 1, None), (1, 2, None), (2, 3, None), (3, 0, None)]);
        assert_eq!(triangles(&sq), vec![0, 0, 0, 0]);
        assert!(clustering(&sq).iter().all(|&c| c == 0.0));
        // K4: three triangles through every node
        let k4 = Edges::fold(
            4,
            [
                (0, 1, None),
                (0, 2, None),
                (0, 3, None),
                (1, 2, None),
                (1, 3, None),
                (2, 3, None),
            ],
        );
        assert_eq!(triangles(&k4), vec![3, 3, 3, 3]);
        // a node with three neighbours, one pair of them adjacent
        let g = Edges::fold(4, [(0, 1, None), (0, 2, None), (0, 3, None), (1, 2, None)]);
        assert_eq!(triangles(&g), vec![1, 1, 1, 0]);
        let c = clustering(&g);
        assert!(close(c[0], 1.0 / 3.0), "{c:?}");
        assert!(close(c[1], 1.0) && close(c[2], 1.0) && c[3] == 0.0, "{c:?}");
        // the direction of an edge is no matter
        let back = Edges::fold(3, [(1, 0, None), (2, 1, None), (0, 2, None)]);
        assert_eq!(triangles(&back), vec![1, 1, 1]);
    }

    #[test]
    fn closeness_in_both_forms() {
        let p = Edges::fold(3, [(0, 1, None), (1, 2, None)]).symmetric();
        let c = closeness(&p, false, false).unwrap();
        assert!(
            close(c[1], 1.0) && close(c[0], 2.0 / 3.0) && close(c[2], 2.0 / 3.0),
            "{c:?}"
        );
        let h = closeness(&p, false, true).unwrap();
        assert!(close(h[1], 1.0) && close(h[0], 0.75), "{h:?}");
        let d = Edges::fold(3, [(0, 1, None), (1, 2, None)]);
        let c = closeness(&d, false, false).unwrap();
        assert!(
            close(c[0], 2.0 / 3.0) && close(c[1], 0.5) && c[2] == 0.0,
            "{c:?}"
        );
        let w = Edges::fold(2, [(0, 1, Some(4.0))]);
        assert!(close(closeness(&w, true, false).unwrap()[0], 0.25));
        assert_eq!(
            closeness(&Edges::fold(1, []), false, false).unwrap(),
            vec![0.0]
        );
    }

    fn two_triangles() -> Edges {
        Edges::fold(
            6,
            [
                (0, 1, None),
                (1, 2, None),
                (2, 0, None),
                (3, 4, None),
                (4, 5, None),
                (5, 3, None),
                (2, 3, None),
            ],
        )
        .symmetric()
    }

    #[test]
    fn modularity_of_a_partition() {
        let g = two_triangles();
        let q = modularity(&g, 1.0, &[0, 0, 0, 1, 1, 1]);
        assert!(close(q, 2.0 * (3.0 / 7.0 - 0.25)), "{q}");
        assert!(close(modularity(&g, 1.0, &[0; 6]), 0.0));
        assert!(modularity(&g, 1.0, &[0, 1, 0, 1, 0, 1]) < q);
        assert!(modularity(&g, 2.0, &[0, 0, 0, 1, 1, 1]) < q);
        let d = Edges::fold(
            6,
            [
                (0, 1, None),
                (1, 2, None),
                (2, 0, None),
                (3, 4, None),
                (4, 5, None),
                (5, 3, None),
                (2, 3, None),
            ],
        );
        let qd = modularity(&d, 1.0, &[0, 0, 0, 1, 1, 1]);
        // inside 6 of 7; out-degrees 4 and 3, in-degrees 3 and 4: (4·3 + 3·4)/49
        assert!(close(qd, 6.0 / 7.0 - 24.0 / 49.0), "{qd}");
    }

    #[test]
    fn communities_find_the_two_triangles() {
        let g = two_triangles();
        assert_eq!(communities(&g, 1.0), vec![1, 1, 1, 2, 2, 2]);
        assert_eq!(communities(&Edges::fold(1, []), 1.0), vec![1]);
        assert_eq!(communities(&Edges::fold(0, []), 1.0), Vec::<usize>::new());
        assert_eq!(communities(&Edges::fold(3, []), 1.0), vec![1, 2, 3]);
        let ring = Edges::fold(6, (0..6).map(|i| (i, (i + 1) % 6, None))).symmetric();
        let c = communities(&ring, 1.0);
        assert_eq!(c[0], 1);
        assert!(c.iter().all(|&k| (1..=6).contains(&k)));
        let part: Vec<usize> = c.iter().map(|k| k - 1).collect();
        let singles: Vec<usize> = (0..6).collect();
        assert!(modularity(&ring, 1.0, &part) >= modularity(&ring, 1.0, &singles) - 1e-12);
        let d = Edges::fold(
            6,
            [
                (0, 1, None),
                (1, 2, None),
                (2, 0, None),
                (3, 4, None),
                (4, 5, None),
                (5, 3, None),
                (2, 3, None),
            ],
        );
        assert_eq!(communities(&d, 1.0), vec![1, 1, 1, 2, 2, 2]);
    }
}
