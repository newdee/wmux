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

    /// Put `ids` into the leaves, in order, keeping the shape of the tree.
    /// Used by `rotate-window`, which moves panes but not the layout.
    pub fn set_panes(&mut self, ids: &[PaneId]) {
        let mut it = ids.iter().copied();
        self.assign(&mut it);
    }

    fn assign(&mut self, it: &mut impl Iterator<Item = PaneId>) {
        match self {
            Node::Leaf(id) => {
                if let Some(next) = it.next() {
                    *id = next;
                }
            }
            Node::Split { children, .. } => children.iter_mut().for_each(|c| c.assign(it)),
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
                let mut fitted = sizes.clone();
                fit_sizes(&mut fitted, avail);
                // The fitted sizes become the stored ones (so resize-pane
                // works in the cells on screen), except when the window is
                // squeezed so far that a pane is pinned to one cell: then the
                // proportions could not survive, so the stored sizes are kept
                // and growing the window back brings the old layout back.
                let shrinking = sizes.iter().map(|&s| s as u32).sum::<u32>() > avail as u32;
                let pinned = fitted.iter().zip(sizes.iter()).any(|(f, s)| *f <= 1 && *s > 1);
                if !(shrinking && pinned) {
                    sizes.copy_from_slice(&fitted);
                }
                let (mut pos, end) = if *horizontal { (rect.x, rect.x + rect.w) } else { (rect.y, rect.y + rect.h) };
                for (child, &sz) in children.iter_mut().zip(fitted.iter()) {
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

    /// Give every pane beside `id` the same size, keeping the rest of the
    /// layout (tmux `select-layout -E`). Returns false if the pane is alone.
    pub fn spread(&mut self, id: PaneId) -> bool {
        match self {
            Node::Leaf(_) => false,
            Node::Split { children, sizes, .. } => {
                if children.iter().any(|c| matches!(c, Node::Leaf(i) if *i == id)) {
                    let total: u32 = sizes.iter().map(|s| *s as u32).sum();
                    let each = (total / sizes.len() as u32).max(1) as u16;
                    let last = sizes.len() - 1;
                    for s in sizes.iter_mut() {
                        *s = each;
                    }
                    // Rounding goes to the last pane, so the total is kept.
                    sizes[last] = (total.saturating_sub(each as u32 * last as u32)).max(1) as u16;
                    return true;
                }
                children.iter_mut().find(|c| c.contains(id)).is_some_and(|c| c.spread(id))
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
                // Like tmux: move the cell's trailing edge; for the last cell,
                // the edge before it. The edge pushes: moving it forward takes
                // from the cells after it, the nearest first, and moving it
                // back from the cells before it, so a pane can grow past a
                // neighbour already at its smallest. A cell that grows and
                // finds nothing left after it takes from the cells before it
                // (tmux's "opposite" search), which is how a middle pane grows
                // when everything below it is already as small as it goes.
                let (a, b) = if idx + 1 < children.len() { (idx, idx + 1) } else { (idx - 1, idx) };
                let (gains, losers): (usize, Vec<usize>) = if grow {
                    (a, (b..children.len()).chain((0..a).rev()).collect())
                } else {
                    (b, (0..=a).rev().collect())
                };
                let mut left = amount;
                let mut moved = 0;
                for i in losers {
                    let can = sizes[i].saturating_sub(1).min(left);
                    sizes[i] -= can;
                    left -= can;
                    moved += can;
                    if left == 0 {
                        break;
                    }
                }
                if moved == 0 {
                    return false;
                }
                sizes[gains] += moved;
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

/// tmux's checksum over a layout string (the four hex digits in front).
fn layout_checksum(s: &str) -> u16 {
    let mut csum: u16 = 0;
    for b in s.bytes() {
        csum = (csum >> 1).wrapping_add((csum & 1) << 15);
        csum = csum.wrapping_add(b as u16);
    }
    csum
}

/// A tmux layout string for this tree: `csum,WxH,X,Y` per cell, a leaf
/// ending in `,paneid`, a left-to-right split in `{}` and a top-to-bottom
/// one in `[]`. `rects` are the panes' rectangles; a split's is the box
/// around its children. What tmux's `#{window_layout}` shows and its
/// `select-layout` takes back.
pub fn layout_string(node: &Node, rects: &[(PaneId, Rect)]) -> String {
    fn bounds(node: &Node, rects: &[(PaneId, Rect)]) -> Rect {
        match node {
            Node::Leaf(id) => rects.iter().find(|(i, _)| i == id).map(|(_, r)| *r).unwrap_or_default(),
            Node::Split { children, .. } => {
                let bs: Vec<Rect> = children.iter().map(|c| bounds(c, rects)).collect();
                let x = bs.iter().map(|r| r.x).min().unwrap_or(0);
                let y = bs.iter().map(|r| r.y).min().unwrap_or(0);
                let x2 = bs.iter().map(|r| r.x + r.w).max().unwrap_or(0);
                let y2 = bs.iter().map(|r| r.y + r.h).max().unwrap_or(0);
                Rect { x, y, w: x2 - x, h: y2 - y }
            }
        }
    }
    fn dump(node: &Node, rects: &[(PaneId, Rect)], out: &mut String) {
        let r = bounds(node, rects);
        out.push_str(&format!("{}x{},{},{}", r.w, r.h, r.x, r.y));
        match node {
            Node::Leaf(id) => out.push_str(&format!(",{id}")),
            Node::Split { horizontal, children, .. } => {
                let (open, close) = if *horizontal { ('{', '}') } else { ('[', ']') };
                out.push(open);
                for (i, c) in children.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    dump(c, rects, out);
                }
                out.push(close);
            }
        }
    }
    let mut body = String::new();
    dump(node, rects, &mut body);
    format!("{:04x},{body}", layout_checksum(&body))
}

/// Read a tmux layout string back into a tree. The checksum, when there is
/// one, must match; the pane ids in it are ignored (the window's panes are
/// put in, in order) but their number must be the window's. Sizes are the
/// cells' widths or heights, which `layout` scales to whatever the window
/// is now.
pub fn parse_layout(s: &str) -> Result<Node, String> {
    let s = s.trim();
    let body = match s.split_once(',') {
        Some((sum, rest)) if sum.len() == 4 && sum.chars().all(|c| c.is_ascii_hexdigit()) => {
            let want = u16::from_str_radix(sum, 16).map_err(|e| e.to_string())?;
            if layout_checksum(rest) != want {
                return Err("layout checksum does not match".into());
            }
            rest
        }
        _ => s,
    };
    struct P<'a> {
        s: &'a [u8],
        i: usize,
    }
    impl P<'_> {
        fn peek(&self) -> Option<u8> {
            self.s.get(self.i).copied()
        }
        fn eat(&mut self, c: u8) -> Result<(), String> {
            if self.peek() == Some(c) {
                self.i += 1;
                Ok(())
            } else {
                Err(format!("expected '{}' at {} in layout", c as char, self.i))
            }
        }
        fn number(&mut self) -> Result<u32, String> {
            let start = self.i;
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.i += 1;
            }
            std::str::from_utf8(&self.s[start..self.i])
                .ok()
                .and_then(|t| t.parse().ok())
                .ok_or_else(|| format!("expected a number at {start} in layout"))
        }
        /// One cell: its size and the tree under it.
        fn cell(&mut self, depth: usize) -> Result<(Node, u16, u16), String> {
            if depth > 64 {
                return Err("layout is nested too deep".into());
            }
            let w = self.number()? as u16;
            self.eat(b'x')?;
            let h = self.number()? as u16;
            self.eat(b',')?;
            self.number()?; // x
            self.eat(b',')?;
            self.number()?; // y
            match self.peek() {
                Some(b',') => {
                    self.i += 1;
                    self.number()?; // the pane id, which is not ours to keep
                    Ok((Node::Leaf(0), w, h))
                }
                Some(open @ (b'{' | b'[')) => {
                    self.i += 1;
                    let horizontal = open == b'{';
                    let close = if horizontal { b'}' } else { b']' };
                    let (mut children, mut sizes) = (Vec::new(), Vec::new());
                    loop {
                        let (child, cw, ch) = self.cell(depth + 1)?;
                        children.push(child);
                        sizes.push(if horizontal { cw } else { ch }.max(1));
                        match self.peek() {
                            Some(b',') => self.i += 1,
                            Some(c) if c == close => {
                                self.i += 1;
                                break;
                            }
                            _ => return Err(format!("expected ',' or '{}' at {} in layout", close as char, self.i)),
                        }
                    }
                    if children.len() < 2 {
                        return Err("a split in the layout has fewer than two cells".into());
                    }
                    Ok((Node::Split { horizontal, children, sizes }, w, h))
                }
                _ => Err(format!("expected ',' '{{' or '[' at {} in layout", self.i)),
            }
        }
    }
    let mut p = P { s: body.as_bytes(), i: 0 };
    let (node, _, _) = p.cell(0)?;
    if p.i != body.len() {
        return Err(format!("trailing text at {} in layout", p.i));
    }
    Ok(node)
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
    // Each entry's share rounded down, and what the rounding dropped.
    let mut dropped: Vec<(u32, usize)> = Vec::with_capacity(sizes.len());
    for (i, s) in sizes.iter_mut().enumerate() {
        let exact = (*s as u32) * (avail as u32);
        let v = (exact / sum).max(1);
        dropped.push((if exact / sum == 0 { 0 } else { exact % sum }, i));
        *s = v as u16;
        acc += v;
    }
    if acc < avail as u32 {
        // The cells rounding left over go one each to the entries that lost
        // the most, the earlier first on a tie (as tmux spreads an even
        // layout): 158 columns in three are 53/53/52, not 52/52/54. Fewer
        // cells are left over than there are entries.
        dropped.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        let left = (avail as u32 - acc) as usize;
        for &(_, i) in dropped.iter().take(left) {
            sizes[i] += 1;
        }
    } else if acc > avail as u32 {
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
    fn spread_evens_out_the_panes_beside_one() {
        // Three panes side by side, dragged out of shape.
        let mut n = Node::Leaf(1);
        let r = rects(&mut n, 80, 24);
        n.split(1, true, 2, rect_of(&r, 1));
        let r = rects(&mut n, 80, 24);
        n.split(2, true, 3, rect_of(&r, 2));
        let _ = rects(&mut n, 80, 24);
        assert!(n.resize(1, Dir::Right, 12));
        let before: Vec<u16> = rects(&mut n, 80, 24).iter().map(|(_, r)| r.w).collect();
        assert!(before[0] > before[1] + 10, "lopsided to start with: {before:?}");
        assert!(n.spread(2), "the pane has panes beside it");
        let after: Vec<u16> = rects(&mut n, 80, 24).iter().map(|(_, r)| r.w).collect();
        assert!(after.iter().max().unwrap() - after.iter().min().unwrap() <= 1, "evened out: {after:?}");
        assert_eq!(after.iter().sum::<u16>(), before.iter().sum::<u16>(), "the same total width");

        // Only the split the pane is in changes; the rest of the tree stands.
        let mut nested = Node::Split {
            horizontal: true,
            sizes: vec![60, 19],
            children: vec![
                Node::Leaf(1),
                Node::Split { horizontal: false, sizes: vec![20, 3], children: vec![Node::Leaf(2), Node::Leaf(3)] },
            ],
        };
        assert!(nested.spread(3));
        let r = rects(&mut nested, 80, 24);
        assert_eq!(rect_of(&r, 1).w, 60, "the outer split is untouched");
        assert!(rect_of(&r, 2).h.abs_diff(rect_of(&r, 3).h) <= 1, "{r:?}");
        // A pane with nothing beside it says so.
        assert!(!Node::Leaf(1).spread(1));
        assert!(!nested.spread(99), "a pane that is not there");
    }

    /// A window squeezed until a pane is down to one row, then grown back,
    /// gets its old proportions back; a moderate squeeze keeps them to a
    /// cell; resize-pane on a smaller window moves the edge by whole cells;
    /// and rounding leftovers go to the first entries.
    #[test]
    fn proportions_survive_a_squeeze() {
        let heights = |n: &mut Node, h: u16| -> Vec<u16> { rects(n, 80, h).iter().map(|(_, r)| r.h).collect() };
        let mut n =
            Node::Split { horizontal: false, children: vec![Node::Leaf(1), Node::Leaf(2)], sizes: vec![30, 10] };
        assert_eq!(heights(&mut n, 41), [30, 10]);
        let squeezed = heights(&mut n, 4);
        assert_eq!(squeezed.iter().sum::<u16>(), 3, "{squeezed:?}");
        assert_eq!(heights(&mut n, 41), [30, 10], "back to the old split");
        // Halving and back: within a cell of where it was.
        let half = heights(&mut n, 21);
        assert_eq!(half, [15, 5]);
        let back = heights(&mut n, 41);
        assert!(back[0].abs_diff(30) <= 1 && back.iter().sum::<u16>() == 40, "{back:?}");
        // On the smaller window, resize moves the edge by the cells asked.
        heights(&mut n, 21);
        assert!(n.resize(1, Dir::Down, 2));
        assert_eq!(heights(&mut n, 21), [17, 3]);
        // Three columns in 158 cells: the two cells of rounding go first.
        let mut cols = Preset::EvenHorizontal.build(&[1, 2, 3]).unwrap();
        let w: Vec<u16> = rects(&mut cols, 160, 24).iter().map(|(_, r)| r.w).collect();
        assert_eq!(w, [53, 53, 52]);
    }

    #[test]
    fn presets_arrange_every_pane() {
        let ids = [1, 2, 3, 4];
        let sizes = |p: Preset| -> Vec<(u16, u16)> {
            let mut n = p.build(&ids).unwrap();
            assert_eq!(n.panes(), ids, "{}: keeps every pane, in order", p.name());
            rects(&mut n, 80, 24).iter().map(|(_, r)| (r.w, r.h)).collect()
        };
        // Four rows and four columns, the leftover cell going to the first
        // one, as tmux spreads them (24 rows minus 3 separators is 6/5/5/5).
        assert_eq!(sizes(Preset::EvenVertical), [(80, 6), (80, 5), (80, 5), (80, 5)]);
        assert_eq!(sizes(Preset::EvenHorizontal), [(20, 24), (19, 24), (19, 24), (19, 24)]);
        // One big pane plus a stack of the rest.
        let main_v = sizes(Preset::MainVertical);
        assert_eq!(main_v[0], (40, 24), "the main pane keeps half the width, and the odd cell");
        assert!(main_v[1..].iter().all(|(w, h)| *w == 39 && (7..=8).contains(h)), "{main_v:?}");
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
    fn layout_strings_round_trip_and_read_tmux_ones() {
        // [1 | [2 / 3]] in 80x24, dumped the way tmux writes it.
        let mut n = Node::Leaf(1);
        let r = rects(&mut n, 80, 24);
        n.split(1, true, 2, rect_of(&r, 1));
        let r = rects(&mut n, 80, 24);
        n.split(2, false, 3, rect_of(&r, 2));
        let r = rects(&mut n, 80, 24);
        let s = layout_string(&n, &r);
        let body = &s[5..];
        assert_eq!(body, "80x24,0,0{40x24,0,0,1,39x24,41,0[39x12,41,0,2,39x11,41,13,3]}");
        assert_eq!(&s[..5], &format!("{:04x},", layout_checksum(body)));
        // Back in: the same shape and sizes, the pane ids from the window.
        let mut back = parse_layout(&s).unwrap();
        back.set_panes(&[1, 2, 3]);
        assert_eq!(rects(&mut back, 80, 24), r, "the same rectangles come out");
        // ...and the shape survives a different window size.
        assert_tiling(&rects(&mut back, 120, 40), 120, 40);
        // A string tmux itself produced (checksum included), and one without.
        let body = "159x48,0,0{79x48,0,0,0,79x48,80,0[79x24,80,0,1,79x23,80,25,2]}";
        let tmux = format!("{:04x},{body}", layout_checksum(body));
        let mut t = parse_layout(&tmux).unwrap();
        assert_eq!(t.panes().len(), 3);
        t.set_panes(&[7, 8, 9]);
        assert_tiling(&rects(&mut t, 80, 24), 80, 24);
        assert!(parse_layout("159x48,0,0{79x48,0,0,0,79x48,80,0,1}").is_ok(), "no checksum is fine");
        // Bad ones say what is wrong instead of panicking.
        assert!(parse_layout("0000,159x48,0,0,0").unwrap_err().contains("checksum"));
        assert!(parse_layout("159x48,0,0{79x48,0,0,0}").unwrap_err().contains("fewer than two"));
        assert!(parse_layout("159x48,0,0{79x48,0,0,0,79x48,80,0,1").unwrap_err().contains("expected"));
        assert!(parse_layout("tiled").unwrap_err().contains("expected"));
        assert!(parse_layout("").is_err());
        let deep = "{".repeat(100);
        assert!(parse_layout(&format!("1x1,0,0{deep}")).is_err());
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
