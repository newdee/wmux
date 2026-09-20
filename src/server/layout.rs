//! Pane layout tree: n-ary splits with explicit sizes, tmux style.

use crate::command::Dir;

pub type PaneId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl Rect {
    pub fn contains(&self, x: u16, y: u16) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    Leaf(PaneId),
    Split {
        /// Children are laid out left-to-right (true) or top-to-bottom (false).
        horizontal: bool,
        children: Vec<Node>,
        sizes: Vec<u16>,
    },
}

/// The named arrangements of `select-layout`, in the order `-n` cycles them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    EvenHorizontal,
    EvenVertical,
    MainHorizontal,
    MainVertical,
    Tiled,
}

pub const PRESETS: &[(&str, Preset)] = &[
    ("even-horizontal", Preset::EvenHorizontal),
    ("even-vertical", Preset::EvenVertical),
    ("main-horizontal", Preset::MainHorizontal),
    ("main-vertical", Preset::MainVertical),
    ("tiled", Preset::Tiled),
];

impl Preset {
    pub fn parse(name: &str) -> Option<Preset> {
        // tmux takes unambiguous prefixes here too ("even-h", "til").
        let hits: Vec<Preset> = PRESETS.iter().filter(|(n, _)| n.starts_with(name)).map(|(_, p)| *p).collect();
        if hits.len() == 1 { Some(hits[0]) } else { PRESETS.iter().find(|(n, _)| *n == name).map(|(_, p)| *p) }
    }

    pub fn name(self) -> &'static str {
        PRESETS.iter().find(|(_, p)| *p == self).map(|(n, _)| *n).unwrap_or("tiled")
    }

    pub fn next(self) -> Preset {
        let i = PRESETS.iter().position(|(_, p)| *p == self).unwrap_or(0);
        PRESETS[(i + 1) % PRESETS.len()].1
    }

    pub fn prev(self) -> Preset {
        let i = PRESETS.iter().position(|(_, p)| *p == self).unwrap_or(0);
        PRESETS[(i + PRESETS.len() - 1) % PRESETS.len()].1
    }

    /// Rebuild a layout tree holding exactly `panes`, in that order.
    pub fn build(self, panes: &[PaneId]) -> Option<Node> {
        let (&first, rest) = panes.split_first()?;
        if rest.is_empty() {
            return Some(Node::Leaf(first));
        }
        let row = |ids: &[PaneId], horizontal: bool| Node::Split {
            horizontal,
            children: ids.iter().map(|id| Node::Leaf(*id)).collect(),
            sizes: vec![1; ids.len()], // equal weights; `layout` scales them to the rect
        };
        Some(match self {
            Preset::EvenHorizontal => row(panes, true),
            Preset::EvenVertical => row(panes, false),
            // The first pane keeps half; the others share the other half.
            Preset::MainVertical => {
                Node::Split { horizontal: true, children: vec![Node::Leaf(first), row(rest, false)], sizes: vec![1, 1] }
            }
            Preset::MainHorizontal => {
                Node::Split { horizontal: false, children: vec![Node::Leaf(first), row(rest, true)], sizes: vec![1, 1] }
            }
            Preset::Tiled => {
                // Columns first, like tmux: ceil(sqrt(n)) columns of rows.
                let n = panes.len();
                let cols = (n as f64).sqrt().ceil() as usize;
                let rows = n.div_ceil(cols);
                let mut columns: Vec<Node> = Vec::new();
                let mut i = 0;
                for c in 0..cols {
                    // Spread the remainder over the first columns.
                    let take = if c < n % cols || n.is_multiple_of(cols) { rows } else { rows - 1 };
                    let take = take.min(n - i);
                    if take == 0 {
                        break;
                    }
                    let slice = &panes[i..i + take];
                    i += take;
                    columns.push(if slice.len() == 1 { Node::Leaf(slice[0]) } else { row(slice, false) });
                }
                if columns.len() == 1 {
                    columns.pop().unwrap()
                } else {
                    Node::Split { horizontal: true, sizes: vec![1; columns.len()], children: columns }
                }
            }
        })
    }
}

impl Node {
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<PaneId>) {
        match self {
            Node::Leaf(id) => out.push(*id),
            Node::Split { children, .. } => children.iter().for_each(|c| c.collect(out)),
        }
    }

    pub fn contains(&self, id: PaneId) -> bool {
        match self {
            Node::Leaf(i) => *i == id,
            Node::Split { children, .. } => children.iter().any(|c| c.contains(id)),
        }
    }

    /// Compute pane rectangles inside `rect`, rebalancing stored sizes to fit.
    pub fn layout(&mut self, rect: Rect, out: &mut Vec<(PaneId, Rect)>) {
        match self {
            Node::Leaf(id) => out.push((*id, rect)),
            Node::Split { horizontal, children, sizes } => {
                let n = children.len() as u16;
                let total = if *horizontal { rect.w } else { rect.h };
                let avail = total.saturating_sub(n - 1);
                fit_sizes(sizes, avail);
                let (mut pos, end) = if *horizontal { (rect.x, rect.x + rect.w) } else { (rect.y, rect.y + rect.h) };
                for (child, &sz) in children.iter_mut().zip(sizes.iter()) {
                    // In a degenerate area the border column may not exist;
                    // never place a child past the end of the rectangle.
                    let sz = sz.min(end.saturating_sub(pos));
                    let r = if *horizontal {
                        Rect { x: pos, y: rect.y, w: sz, h: rect.h }
                    } else {
                        Rect { x: rect.x, y: pos, w: rect.w, h: sz }
                    };
                    child.layout(r, out);
                    pos = (pos + sz + 1).min(end);
                }
            }
        }
    }

    /// Split pane `target` (currently occupying `target_rect`), placing `new`
    /// after it. Returns false if `target` is not in the tree.
    pub fn split(&mut self, target: PaneId, horizontal: bool, new: PaneId, target_rect: Rect) -> bool {
        self.split_at(target, horizontal, new, target_rect, false)
    }

    /// Like `split`, with `before` placing `new` left of / above `target`.
    pub fn split_at(&mut self, target: PaneId, horizontal: bool, new: PaneId, target_rect: Rect, before: bool) -> bool {
        let total = if horizontal { target_rect.w } else { target_rect.h };
        let avail = total.saturating_sub(1);
        let second = avail / 2;
        let first = avail - second;
        // The target keeps the larger half, wherever it ends up.
        let (a, b) = if before { (second.max(1), first.max(1)) } else { (first.max(1), second.max(1)) };
        match self {
            Node::Leaf(id) if *id == target => {
                let (x, y) = if before { (new, target) } else { (target, new) };
                *self = Node::Split { horizontal, children: vec![Node::Leaf(x), Node::Leaf(y)], sizes: vec![a, b] };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { horizontal: h, children, sizes } => {
                let idx = match children.iter().position(|c| c.contains(target)) {
                    Some(i) => i,
                    None => return false,
                };
                if *h == horizontal && matches!(children[idx], Node::Leaf(_)) {
                    let at = if before { idx } else { idx + 1 };
                    children.insert(at, Node::Leaf(new));
                    sizes[idx] = if before { b } else { a };
                    sizes.insert(at, if before { a } else { b });
                    true
                } else {
                    children[idx].split_at(target, horizontal, new, target_rect, before)
                }
            }
        }
    }

    /// Wrap the whole tree in a new split so that `new` spans the full
    /// width (horizontal) or height of `area`.
    pub fn split_root(&mut self, horizontal: bool, new: PaneId, area: Rect, before: bool) {
        let total = if horizontal { area.w } else { area.h };
        let avail = total.saturating_sub(1);
        let second = (avail / 2).max(1);
        let first = avail.saturating_sub(second).max(1);
        let old = std::mem::replace(self, Node::Leaf(new));
        let (children, sizes) = if before {
            (vec![Node::Leaf(new), old], vec![second, first])
        } else {
            (vec![old, Node::Leaf(new)], vec![first, second])
        };
        *self = Node::Split { horizontal, children, sizes };
    }

    /// Remove a pane, collapsing single-child splits. Returns false if absent.
    /// The freed space goes to the previous sibling (or the next one).
    pub fn remove(&mut self, id: PaneId) -> bool {
        match self {
            Node::Leaf(_) => false,
            Node::Split { children, sizes, .. } => {
                if let Some(idx) = children.iter().position(|c| matches!(c, Node::Leaf(i) if *i == id)) {
                    let freed = sizes[idx] + 1;
                    children.remove(idx);
                    sizes.remove(idx);
                    if !sizes.is_empty() {
                        let give = if idx > 0 { idx - 1 } else { 0 };
                        sizes[give] += freed;
                    }
                    if children.len() == 1 {
                        let only = children.remove(0);
                        *self = only;
                    }
                    return true;
                }
                for (i, c) in children.iter_mut().enumerate() {
                    if c.contains(id) {
                        let ok = c.remove(id);
                        // A child that collapsed to a split in the same direction
                        // can be flattened into us.
                        if let Node::Split { horizontal: ch, children: cc, sizes: cs } = &children[i].clone()
                            && let Node::Split { horizontal, children: pc, sizes: ps, .. } = self
                            && ch == horizontal
                        {
                            pc.remove(i);
                            ps.remove(i);
                            for (k, (c2, s2)) in cc.iter().zip(cs.iter()).enumerate() {
                                pc.insert(i + k, c2.clone());
                                ps.insert(i + k, *s2);
                            }
                        }
                        return ok;
                    }
                }
                false
            }
        }
    }

    /// Move the edge of `id` in direction `dir` by `amount` cells (tmux
    /// `resize-pane` semantics). Returns false if nothing could change.
    pub fn resize(&mut self, id: PaneId, dir: Dir, amount: u16) -> bool {
        let want_h = matches!(dir, Dir::Left | Dir::Right);
        let grow = matches!(dir, Dir::Right | Dir::Down);
        self.resize_inner(id, want_h, grow, amount)
    }

    fn resize_inner(&mut self, id: PaneId, want_h: bool, grow: bool, amount: u16) -> bool {
        match self {
            Node::Leaf(_) => false,
            Node::Split { horizontal, children, sizes } => {
                let idx = match children.iter().position(|c| c.contains(id)) {
                    Some(i) => i,
                    None => return false,
                };
                // Prefer the innermost split of the right orientation.
                if children[idx].resize_inner(id, want_h, grow, amount) {
                    return true;
                }
                if *horizontal != want_h || children.len() < 2 {
                    return false;
                }
                // Like tmux: resize the cell's trailing edge; for the last
                // cell, apply the same change to the previous cell instead.
                let (a, b) = if idx + 1 < children.len() { (idx, idx + 1) } else { (idx - 1, idx) };
                let (from, to) = if grow { (b, a) } else { (a, b) };
                let delta = amount.min(sizes[from].saturating_sub(1));
                if delta == 0 {
                    return false;
                }
                sizes[from] -= delta;
                sizes[to] += delta;
                true
            }
        }
    }

    pub fn swap(&mut self, a: PaneId, b: PaneId) {
        match self {
            Node::Leaf(id) => {
                if *id == a {
                    *id = b;
                } else if *id == b {
                    *id = a;
                }
            }
            Node::Split { children, .. } => children.iter_mut().for_each(|c| c.swap(a, b)),
        }
    }
}

/// Scale `sizes` so they sum to `avail`, keeping every entry >= 1 where possible.
fn fit_sizes(sizes: &mut [u16], avail: u16) {
    if sizes.is_empty() {
        return;
    }
    let sum: u32 = sizes.iter().map(|&s| s as u32).sum();
    if sum == avail as u32 {
        return;
    }
    let n = sizes.len() as u16;
    if avail < n {
        // Not enough room for everyone; give what we can from the left.
        let mut left = avail;
        for s in sizes.iter_mut() {
            *s = left.min(1);
            left -= *s;
        }
        return;
    }
    let sum = sum.max(1);
    let mut acc = 0u32;
    for s in sizes.iter_mut() {
        let v = ((*s as u32) * (avail as u32) / sum).max(1) as u16;
        *s = v;
        acc += v as u32;
    }
    // Fix rounding drift on the largest entry.
    let biggest = (0..sizes.len()).max_by_key(|&i| sizes[i]).unwrap();
    if acc > avail as u32 {
        let over = (acc - avail as u32) as u16;
        // Take from entries larger than 1, biggest first.
        let mut over = over;
        let mut order: Vec<usize> = (0..sizes.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(sizes[i]));
        for i in order {
            let can = sizes[i].saturating_sub(1).min(over);
            sizes[i] -= can;
            over -= can;
            if over == 0 {
                break;
            }
        }
    } else {
        sizes[biggest] += (avail as u32 - acc) as u16;
    }
}

/// Pick the neighbouring pane of `from` in direction `dir` among `rects`.
pub fn neighbour(rects: &[(PaneId, Rect)], from: PaneId, dir: Dir) -> Option<PaneId> {
    let (_, a) = rects.iter().find(|(id, _)| *id == from)?;
    let overlap = |a0: u16, a1: u16, b0: u16, b1: u16| -> i32 { (a1.min(b1) as i32) - (a0.max(b0) as i32) };
    let mut best: Option<(i32, i32, PaneId)> = None; // (distance, -overlap, id)
    for (id, r) in rects {
        if *id == from {
            continue;
        }
        let (dist, ov) = match dir {
            Dir::Left => {
                if r.x + r.w > a.x {
                    continue;
                }
                (a.x as i32 - (r.x + r.w) as i32, overlap(a.y, a.y + a.h, r.y, r.y + r.h))
            }
            Dir::Right => {
                if r.x < a.x + a.w {
                    continue;
                }
                (r.x as i32 - (a.x + a.w) as i32, overlap(a.y, a.y + a.h, r.y, r.y + r.h))
            }
            Dir::Up => {
                if r.y + r.h > a.y {
                    continue;
                }
                (a.y as i32 - (r.y + r.h) as i32, overlap(a.x, a.x + a.w, r.x, r.x + r.w))
            }
            Dir::Down => {
                if r.y < a.y + a.h {
                    continue;
                }
                (r.y as i32 - (a.y + a.h) as i32, overlap(a.x, a.x + a.w, r.x, r.x + r.w))
            }
        };
        if ov <= 0 {
            continue;
        }
        let key = (dist, -ov, *id);
        if best.is_none_or(|b| key < b) {
            best = Some(key);
        }
    }
    best.map(|b| b.2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rects(n: &mut Node, w: u16, h: u16) -> Vec<(PaneId, Rect)> {
        let mut out = Vec::new();
        n.layout(Rect { x: 0, y: 0, w, h }, &mut out);
        out
    }

    fn rect_of(r: &[(PaneId, Rect)], id: PaneId) -> Rect {
        r.iter().find(|(i, _)| *i == id).unwrap().1
    }

    #[test]
    fn presets_arrange_every_pane() {
        let ids = [1, 2, 3, 4];
        let sizes = |p: Preset| -> Vec<(u16, u16)> {
            let mut n = p.build(&ids).unwrap();
            assert_eq!(n.panes(), ids, "{}: keeps every pane, in order", p.name());
            rects(&mut n, 80, 24).iter().map(|(_, r)| (r.w, r.h)).collect()
        };
        // Four rows and four columns, the leftover cell going to the last one
        // (24 rows minus 3 separators splits 5/5/5/6).
        assert_eq!(sizes(Preset::EvenVertical), [(80, 5), (80, 5), (80, 5), (80, 6)]);
        assert_eq!(sizes(Preset::EvenHorizontal), [(19, 24), (19, 24), (19, 24), (20, 24)]);
        // One big pane plus a stack of the rest.
        let main_v = sizes(Preset::MainVertical);
        assert_eq!(main_v[0], (39, 24), "the main pane keeps half the width");
        assert!(main_v[1..].iter().all(|(w, h)| *w == 40 && (7..=8).contains(h)), "{main_v:?}");
        let main_h = sizes(Preset::MainHorizontal);
        assert_eq!(main_h[0].0, 80, "the main pane spans the width: {main_h:?}");
        assert!(main_h[1..].iter().all(|(w, _)| *w == 26), "{main_h:?}");
        // A 2x2 grid.
        let tiled = sizes(Preset::Tiled);
        assert!(tiled.iter().all(|(w, h)| (39..=40).contains(w) && (11..=12).contains(h)), "{tiled:?}");

        // Degenerate counts still produce a tree with exactly those panes.
        for p in PRESETS.iter().map(|(_, p)| *p) {
            assert!(p.build(&[]).is_none(), "{}: nothing to arrange", p.name());
            assert_eq!(p.build(&[7]).unwrap(), Node::Leaf(7));
            let mut many: Node = p.build(&(1..=9).collect::<Vec<PaneId>>()).unwrap();
            assert_eq!(many.panes().len(), 9);
            assert_eq!(rects(&mut many, 80, 24).len(), 9, "{}: nine panes get nine rects", p.name());
        }
    }

    #[test]
    fn preset_names_take_prefixes_and_cycle() {
        assert_eq!(Preset::parse("tiled"), Some(Preset::Tiled));
        assert_eq!(Preset::parse("til"), Some(Preset::Tiled));
        assert_eq!(Preset::parse("even-h"), Some(Preset::EvenHorizontal));
        assert_eq!(Preset::parse("main"), None, "main-horizontal and main-vertical both match");
        assert_eq!(Preset::parse("nope"), None);
        assert_eq!(Preset::EvenHorizontal.next(), Preset::EvenVertical);
        assert_eq!(Preset::Tiled.next(), Preset::EvenHorizontal, "the cycle wraps");
        assert_eq!(Preset::EvenHorizontal.prev(), Preset::Tiled);
        for (name, p) in PRESETS {
            assert_eq!(p.name(), *name);
        }
    }

    fn assert_tiling(r: &[(PaneId, Rect)], w: u16, h: u16) {
        // No overlaps, everything inside, and every pane has size >= 1.
        for (i, (_, a)) in r.iter().enumerate() {
            assert!(a.w >= 1 && a.h >= 1, "{a:?}");
            assert!(a.x + a.w <= w && a.y + a.h <= h, "{a:?} outside {w}x{h}");
            for (_, b) in &r[i + 1..] {
                let sep = a.x + a.w < b.x || b.x + b.w < a.x || a.y + a.h < b.y || b.y + b.h < a.y;
                assert!(sep, "overlap/adjacent without border: {a:?} {b:?}");
            }
        }
    }

    #[test]
    fn single_leaf_fills() {
        let mut n = Node::Leaf(1);
        let r = rects(&mut n, 80, 24);
        assert_eq!(r, vec![(1, Rect { x: 0, y: 0, w: 80, h: 24 })]);
    }

    #[test]
    fn split_horizontal_and_vertical() {
        let mut n = Node::Leaf(1);
        let r = rects(&mut n, 80, 24);
        assert!(n.split(1, true, 2, rect_of(&r, 1)));
        let r = rects(&mut n, 80, 24);
        assert_eq!(rect_of(&r, 1), Rect { x: 0, y: 0, w: 40, h: 24 });
        assert_eq!(rect_of(&r, 2), Rect { x: 41, y: 0, w: 39, h: 24 });
        assert_tiling(&r, 80, 24);
        assert!(n.split(2, false, 3, rect_of(&r, 2)));
        let r = rects(&mut n, 80, 24);
        assert_eq!(rect_of(&r, 2), Rect { x: 41, y: 0, w: 39, h: 12 });
        assert_eq!(rect_of(&r, 3), Rect { x: 41, y: 13, w: 39, h: 11 });
        assert_tiling(&r, 80, 24);
        assert_eq!(n.panes(), vec![1, 2, 3]);
        assert!(!n.split(99, true, 4, Rect::default()));
    }

    #[test]
    fn split_before_and_root() {
        let mut n = Node::Leaf(1);
        let r = rects(&mut n, 80, 24);
        assert!(n.split_at(1, true, 2, rect_of(&r, 1), true));
        assert_eq!(n.panes(), vec![2, 1]);
        let r = rects(&mut n, 80, 24);
        assert_eq!(rect_of(&r, 2), Rect { x: 0, y: 0, w: 39, h: 24 });
        assert_eq!(rect_of(&r, 1), Rect { x: 40, y: 0, w: 40, h: 24 });
        // Flat insert before inside an existing same-direction split.
        assert!(n.split_at(1, true, 3, rect_of(&r, 1), true));
        assert_eq!(n.panes(), vec![2, 3, 1]);
        assert_tiling(&rects(&mut n, 80, 24), 80, 24);
        // Full-width split below everything.
        n.split_root(false, 4, Rect { x: 0, y: 0, w: 80, h: 24 }, false);
        let r = rects(&mut n, 80, 24);
        assert_eq!(rect_of(&r, 4), Rect { x: 0, y: 13, w: 80, h: 11 });
        assert_tiling(&r, 80, 24);
        n.split_root(true, 5, Rect { x: 0, y: 0, w: 80, h: 24 }, true);
        let r = rects(&mut n, 80, 24);
        assert_eq!(rect_of(&r, 5), Rect { x: 0, y: 0, w: 39, h: 24 });
        assert_tiling(&r, 80, 24);
    }

    #[test]
    fn same_direction_split_is_flat() {
        let mut n = Node::Leaf(1);
        let r = rects(&mut n, 80, 24);
        n.split(1, true, 2, rect_of(&r, 1));
        let r = rects(&mut n, 80, 24);
        n.split(1, true, 3, rect_of(&r, 1));
        match &n {
            Node::Split { children, .. } => assert_eq!(children.len(), 3),
            _ => panic!(),
        }
        assert_eq!(n.panes(), vec![1, 3, 2]);
    }

    #[test]
    fn remove_collapses() {
        let mut n = Node::Leaf(1);
        let r = rects(&mut n, 80, 24);
        n.split(1, true, 2, rect_of(&r, 1));
        let r = rects(&mut n, 80, 24);
        n.split(2, false, 3, rect_of(&r, 2));
        assert!(n.remove(3));
        let r = rects(&mut n, 80, 24);
        assert_eq!(rect_of(&r, 2), Rect { x: 41, y: 0, w: 39, h: 24 });
        assert!(n.remove(1));
        assert_eq!(n, Node::Leaf(2));
        assert!(!n.remove(1));
    }

    #[test]
    fn remove_flattens_nested_same_direction() {
        // [1 | [2 / 3]] then split 3 horizontally -> [1 | [2 / [3 | 4]]]; remove 2
        // -> [1 | [3 | 4]] which flattens to [1 | 3 | 4].
        let mut n = Node::Leaf(1);
        let r = rects(&mut n, 80, 24);
        n.split(1, true, 2, rect_of(&r, 1));
        let r = rects(&mut n, 80, 24);
        n.split(2, false, 3, rect_of(&r, 2));
        let r = rects(&mut n, 80, 24);
        n.split(3, true, 4, rect_of(&r, 3));
        assert!(n.remove(2));
        match &n {
            Node::Split { children, horizontal, .. } => {
                assert!(*horizontal);
                assert_eq!(children.len(), 3, "{n:?}");
            }
            _ => panic!("{n:?}"),
        }
        assert_tiling(&rects(&mut n, 80, 24), 80, 24);
    }

    #[test]
    fn resize_moves_edges() {
        let mut n = Node::Leaf(1);
        let r = rects(&mut n, 80, 24);
        n.split(1, true, 2, rect_of(&r, 1));
        let _ = rects(&mut n, 80, 24);
        assert!(n.resize(1, Dir::Right, 5));
        let r = rects(&mut n, 80, 24);
        assert_eq!(rect_of(&r, 1).w, 45);
        assert_eq!(rect_of(&r, 2).w, 34);
        // Rightmost pane resized -R moves its left edge right (shrinks).
        assert!(n.resize(2, Dir::Right, 4));
        let r = rects(&mut n, 80, 24);
        assert_eq!(rect_of(&r, 2).w, 30);
        assert_eq!(rect_of(&r, 1).w, 49);
        // Cannot shrink below 1.
        assert!(n.resize(2, Dir::Right, 100));
        let r = rects(&mut n, 80, 24);
        assert_eq!(rect_of(&r, 2).w, 1);
        assert!(!n.resize(2, Dir::Right, 1));
        // Vertical resize on a horizontal-only tree does nothing.
        assert!(!n.resize(1, Dir::Down, 3));
        assert_tiling(&r, 80, 24);
    }

    #[test]
    fn window_resize_rescales() {
        let mut n = Node::Leaf(1);
        let r = rects(&mut n, 80, 24);
        n.split(1, true, 2, rect_of(&r, 1));
        let _ = rects(&mut n, 80, 24);
        let r = rects(&mut n, 120, 40);
        assert_tiling(&r, 120, 40);
        assert_eq!(rect_of(&r, 1).w + rect_of(&r, 2).w + 1, 120);
        let r = rects(&mut n, 3, 2);
        assert_tiling(&r, 3, 2);
        // Degenerate: narrower than the number of panes.
        let mut out = Vec::new();
        n.layout(Rect { x: 0, y: 0, w: 1, h: 1 }, &mut out);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn degenerate_areas_never_panic() {
        // Three panes in a 1x1 (and 0x0) area: everyone gets a rectangle,
        // some of them empty, and nothing overflows.
        let mut n = Node::Leaf(1);
        let r = rects(&mut n, 80, 24);
        n.split(1, true, 2, rect_of(&r, 1));
        let r = rects(&mut n, 80, 24);
        n.split(2, false, 3, rect_of(&r, 2));
        for (w, h) in [(1, 1), (0, 0), (2, 1), (1, 3), (3, 3)] {
            let mut out = Vec::new();
            n.layout(Rect { x: 0, y: 0, w, h }, &mut out);
            assert_eq!(out.len(), 3, "{w}x{h}");
            for (_, r) in &out {
                assert!(r.x + r.w <= w && r.y + r.h <= h, "{w}x{h}: {r:?}");
            }
            // Neighbour lookup and resize must not panic either.
            for id in [1, 2, 3] {
                for d in [Dir::Left, Dir::Right, Dir::Up, Dir::Down] {
                    let _ = neighbour(&out, id, d);
                    let _ = n.resize(id, d, 1);
                }
            }
        }
        // Growing back restores a proper tiling.
        assert_tiling(&rects(&mut n, 80, 24), 80, 24);
    }

    #[test]
    fn many_splits_random_walk_stays_tiled() {
        let mut n = Node::Leaf(0);
        let mut next = 1;
        let mut seed = 12345u32;
        for _ in 0..40 {
            seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
            let ids = n.panes();
            let target = ids[(seed >> 8) as usize % ids.len()];
            let r = rects(&mut n, 200, 60);
            let horizontal = (seed >> 3) & 1 == 0;
            let tr = rect_of(&r, target);
            if (horizontal && tr.w < 3) || (!horizontal && tr.h < 3) {
                continue;
            }
            n.split(target, horizontal, next, tr);
            next += 1;
            let r = rects(&mut n, 200, 60);
            assert_tiling(&r, 200, 60);
            assert_eq!(r.len(), n.panes().len());
        }
        while n.panes().len() > 1 {
            let ids = n.panes();
            seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
            n.remove(ids[(seed >> 8) as usize % ids.len()]);
            assert_tiling(&rects(&mut n, 200, 60), 200, 60);
        }
    }

    #[test]
    fn neighbours() {
        let mut n = Node::Leaf(1);
        let r = rects(&mut n, 80, 24);
        n.split(1, true, 2, rect_of(&r, 1));
        let r = rects(&mut n, 80, 24);
        n.split(2, false, 3, rect_of(&r, 2));
        let r = rects(&mut n, 80, 24);
        assert_eq!(neighbour(&r, 1, Dir::Right), Some(2));
        assert_eq!(neighbour(&r, 3, Dir::Left), Some(1));
        assert_eq!(neighbour(&r, 2, Dir::Down), Some(3));
        assert_eq!(neighbour(&r, 3, Dir::Up), Some(2));
        assert_eq!(neighbour(&r, 1, Dir::Left), None);
        assert_eq!(neighbour(&r, 1, Dir::Up), None);
    }

    #[test]
    fn swap_ids() {
        let mut n = Node::Leaf(1);
        let r = rects(&mut n, 80, 24);
        n.split(1, true, 2, rect_of(&r, 1));
        n.swap(1, 2);
        assert_eq!(n.panes(), vec![2, 1]);
    }
}
