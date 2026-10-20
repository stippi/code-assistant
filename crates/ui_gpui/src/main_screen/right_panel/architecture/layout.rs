//! Places the boxes of one level: components that use others above the
//! ones they use, so dependency arrows point down.
//!
//! A component's rank is the length of its longest dependency chain, with
//! components that depend on each other in a cycle sharing one rank. Ranks become rows, top-down from the
//! highest; a rank wider than the panel wraps onto further rows. Within a
//! rank, components sit near the average column of their dependents
//! (barycenter ordering), which keeps arrows short. Components without any
//! dependency either way sit in rows of their own at the bottom.

use std::collections::HashMap;

/// A box's place on the grid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Slot {
    pub row: usize,
    /// The column; fractional when a row is centered.
    pub column: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    /// One slot per node, in node order.
    pub slots: Vec<Slot>,
    pub rows: usize,
    pub columns: usize,
    /// The first row of components without dependencies, if any.
    pub isolated_from: Option<usize>,
}

/// Lay out `count` nodes with `edges` (`from` uses `to`) on at most
/// `columns` columns.
pub fn layered(count: usize, edges: &[(usize, usize)], columns: usize) -> Layout {
    let columns = columns.max(1);
    let mut uses = vec![Vec::new(); count];
    let mut used_by = vec![Vec::new(); count];
    for &(from, to) in edges {
        if from < count && to < count && from != to {
            uses[from].push(to);
            used_by[to].push(from);
        }
    }

    // Longest dependency chain between groups of mutually dependent
    // components (strongly connected), so a cycle shares one rank instead
    // of stretching into a column.
    let group = strongly_connected(count, &uses);
    let groups = group.iter().max().map_or(0, |g| g + 1);
    let mut group_uses = vec![Vec::new(); groups];
    for (from, deps) in uses.iter().enumerate() {
        for &to in deps {
            if group[from] != group[to] {
                group_uses[group[from]].push(group[to]);
            }
        }
    }
    fn visit(g: usize, uses: &[Vec<usize>], rank: &mut [Option<usize>]) -> usize {
        if let Some(r) = rank[g] {
            return r;
        }
        let r = uses[g]
            .iter()
            .map(|&dep| visit(dep, uses, rank) + 1)
            .max()
            .unwrap_or(0);
        rank[g] = Some(r);
        r
    }
    let mut group_rank: Vec<Option<usize>> = vec![None; groups];
    for g in 0..groups {
        visit(g, &group_uses, &mut group_rank);
    }
    let rank: Vec<Option<usize>> = (0..count).map(|n| group_rank[group[n]]).collect();
    let isolated: Vec<usize> = (0..count)
        .filter(|&n| uses[n].is_empty() && used_by[n].is_empty())
        .collect();
    let max_rank = (0..count)
        .filter(|n| !isolated.contains(n))
        .filter_map(|n| rank[n])
        .max();

    // Ranks top-down, each in node order to start with.
    let mut ranks: Vec<Vec<usize>> = Vec::new();
    if let Some(max_rank) = max_rank {
        for r in (0..=max_rank).rev() {
            let nodes: Vec<usize> = (0..count)
                .filter(|n| rank[*n] == Some(r) && !isolated.contains(n))
                .collect();
            if !nodes.is_empty() {
                ranks.push(nodes);
            }
        }
    }

    // Barycenter sweeps: down by dependents, then up by dependencies.
    let position = |ranks: &[Vec<usize>]| {
        let mut pos = HashMap::new();
        for nodes in ranks {
            let width = nodes.len().min(columns) as f32;
            for (ix, &n) in nodes.iter().enumerate() {
                let col = (ix % columns) as f32 + (columns as f32 - width) / 2.;
                pos.insert(n, col);
            }
        }
        pos
    };
    for (neighbors, downward) in [(&used_by, true), (&uses, false), (&used_by, true)] {
        let order: Vec<usize> = if downward {
            (1..ranks.len()).collect()
        } else {
            (0..ranks.len().saturating_sub(1)).rev().collect()
        };
        for r in order {
            let pos = position(&ranks);
            let nodes = &mut ranks[r];
            let mut keyed: Vec<(f32, usize, usize)> = nodes
                .iter()
                .enumerate()
                .map(|(ix, &n)| {
                    let near: Vec<f32> = neighbors[n]
                        .iter()
                        .filter_map(|m| pos.get(m).copied())
                        .collect();
                    let key = if near.is_empty() {
                        pos[&n]
                    } else {
                        near.iter().sum::<f32>() / near.len() as f32
                    };
                    (key, ix, n)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            *nodes = keyed.into_iter().map(|(_, _, n)| n).collect();
        }
    }

    let mut slots = vec![Slot { row: 0, column: 0. }; count];
    let mut row = 0;
    let mut place = |nodes: &[usize], row: &mut usize| {
        for chunk in nodes.chunks(columns) {
            let offset = (columns - chunk.len()) as f32 / 2.;
            for (ix, &n) in chunk.iter().enumerate() {
                slots[n] = Slot {
                    row: *row,
                    column: ix as f32 + offset,
                };
            }
            *row += 1;
        }
    };
    for nodes in &ranks {
        place(nodes, &mut row);
    }
    let isolated_from = (!isolated.is_empty()).then_some(row);
    place(&isolated, &mut row);
    Layout {
        slots,
        rows: row,
        columns: columns.min(count.max(1)),
        isolated_from,
    }
}

/// Tarjan's strongly connected components: the group index of each node.
/// Groups are numbered so that a group's dependencies come first.
fn strongly_connected(count: usize, uses: &[Vec<usize>]) -> Vec<usize> {
    struct State<'a> {
        uses: &'a [Vec<usize>],
        index: Vec<Option<usize>>,
        low: Vec<usize>,
        on_stack: Vec<bool>,
        stack: Vec<usize>,
        next: usize,
        group: Vec<usize>,
        groups: usize,
    }
    fn connect(n: usize, s: &mut State) {
        s.index[n] = Some(s.next);
        s.low[n] = s.next;
        s.next += 1;
        s.stack.push(n);
        s.on_stack[n] = true;
        for &m in s.uses[n].iter() {
            match s.index[m] {
                None => {
                    connect(m, s);
                    s.low[n] = s.low[n].min(s.low[m]);
                }
                Some(ix) if s.on_stack[m] => s.low[n] = s.low[n].min(ix),
                Some(_) => {}
            }
        }
        if Some(s.low[n]) == s.index[n] {
            while let Some(m) = s.stack.pop() {
                s.on_stack[m] = false;
                s.group[m] = s.groups;
                if m == n {
                    break;
                }
            }
            s.groups += 1;
        }
    }
    let mut state = State {
        uses,
        index: vec![None; count],
        low: vec![0; count],
        on_stack: vec![false; count],
        stack: Vec::new(),
        next: 0,
        group: vec![0; count],
        groups: 0,
    };
    for n in 0..count {
        if state.index[n].is_none() {
            connect(n, &mut state);
        }
    }
    state.group
}

/// The edges not implied by a longer path: `a → c` goes when `a → b → c`
/// exists. Drawing only these keeps a layered graph readable; the direct
/// edges of a hovered box are drawn on top.
pub fn transitive_reduction(count: usize, edges: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut uses = vec![Vec::new(); count];
    for &(from, to) in edges {
        if from < count && to < count && from != to {
            uses[from].push(to);
        }
    }
    edges
        .iter()
        .copied()
        .filter(|&(from, to)| {
            // Is `to` reachable from `from` without the direct edge?
            let mut seen = vec![false; count];
            let mut stack: Vec<usize> = uses[from].iter().copied().filter(|&n| n != to).collect();
            while let Some(n) = stack.pop() {
                if n == to {
                    return false;
                }
                if n == from || std::mem::replace(&mut seen[n], true) {
                    continue;
                }
                stack.extend(uses[n].iter().copied());
            }
            true
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn users_sit_above_what_they_use() {
        // 0 uses 1 uses 2; 3 uses 2; 4 is alone.
        let layout = layered(5, &[(0, 1), (1, 2), (3, 2)], 4);
        let row = |n: usize| layout.slots[n].row;
        assert!(row(0) < row(1));
        assert!(row(1) < row(2));
        assert!(row(3) < row(2));
        assert_eq!(layout.isolated_from, Some(3));
        assert_eq!(row(4), 3);
        assert_eq!(layout.rows, 4);
    }

    #[test]
    fn cycles_share_a_rank_and_wide_ranks_wrap() {
        // 0, 1, 2 form a cycle and use 3.
        let layout = layered(4, &[(0, 1), (1, 2), (2, 0), (1, 3)], 4);
        let rows: Vec<usize> = layout.slots.iter().map(|s| s.row).collect();
        assert_eq!(rows, vec![0, 0, 0, 1]);

        // Five nodes using one: the five wrap onto rows of two.
        let edges: Vec<(usize, usize)> = (1..6).map(|n| (n, 0)).collect();
        let layout = layered(6, &edges, 2);
        assert_eq!(layout.rows, 4);
        assert_eq!(layout.slots[0].row, 3);
        assert!(layout.slots[1..].iter().all(|s| s.row < 3));
        // The single node is centered.
        assert_eq!(layout.slots[0].column, 0.5);
    }

    #[test]
    fn implied_edges_are_reduced() {
        let edges = [(0, 1), (1, 2), (0, 2), (3, 2)];
        assert_eq!(
            transitive_reduction(4, &edges),
            vec![(0, 1), (1, 2), (3, 2)]
        );
        // A cycle keeps its edges.
        assert_eq!(
            transitive_reduction(2, &[(0, 1), (1, 0)]),
            vec![(0, 1), (1, 0)]
        );
    }

    #[test]
    fn dependencies_sit_below_their_users() {
        // 0 → 2 and 1 → 3: 2 under 0, 3 under 1 even if listed swapped.
        let layout = layered(4, &[(0, 3), (1, 2)], 2);
        assert!(layout.slots[3].column < layout.slots[2].column);
    }
}
