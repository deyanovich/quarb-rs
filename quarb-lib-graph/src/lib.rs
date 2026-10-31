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
            o.0.partial_cmp(&self.0)
                .unwrap_or(Ordering::Equal)
                .then_with(|| o.1.cmp(&self.1))
        }
    }
    for s in 0..n {
        // single-source shortest paths: the stack of nodes in order
        // of non-decreasing distance, the predecessors on shortest
        // paths, the path counts
        let mut stack: Vec<usize> = Vec::with_capacity(n);
        let mut pred: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut sigma = vec![0.0f64; n];
        let mut dist: Vec<Option<f64>> = vec![None; n];
        sigma[s] = 1.0;
        dist[s] = Some(0.0);
        if weighted {
            let mut heap = BinaryHeap::new();
            heap.push(At(0.0, s));
            let mut settled = vec![false; n];
            while let Some(At(d, v)) = heap.pop() {
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
                            heap.push(At(nd, w));
                        }
                        Some(_) => {}
                        None => {
                            dist[w] = Some(nd);
                            sigma[w] = sigma[v];
                            pred[w] = vec![v];
                            heap.push(At(nd, w));
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
}
