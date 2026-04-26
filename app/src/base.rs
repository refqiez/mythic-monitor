pub mod logger;
pub use logger::{init_logger, WriterWrapper};
pub(crate) use logger::log_user;

pub mod path;
pub use path::{app_paths, init_paths, analize_path, AppPath, PathInitError, AppPathAnalisys};


#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutoSize {
    w: Option<usize>,
    h: Option<usize>,
    scale: f64,
}

impl AutoSize {
    pub fn new(width: Option<usize>, height: Option<usize>, scale: f64) -> Self {
        Self { w: width, h: height, scale }
    }

    pub fn width(&self) -> Option<usize> {
        self.w.map(|w| (w as f64 * self.scale) as usize)
    }

    pub fn height(&self) -> Option<usize> {
        self.h.map(|h| (h as f64 * self.scale) as usize)
    }

    pub fn complete(&self, width: usize, height: usize) -> (usize, usize) {
        let (w,h) = match (self.w, self.h) {
            (None, None) =>
                (width, height),
            (Some(w), None) =>
                (w, w * height / width),
            (None, Some(h)) =>
                (h * width / height, h),
            (Some(w), Some(h)) =>
                (w, h)
        };

        ((w as f64 * self.scale) as usize, (h as f64 * self.scale) as usize)
    }

    pub fn is_complete(&self) -> bool {
        self.w.is_some() && self.h.is_some()
    }
}


// #[derive(Debug, Clone, Copy, PartialEq, Eq)]
// pub enum Align {
//     Start,
//     Center,
//     End,
// }


pub fn o3_hungarian(n: usize, m: usize, cost: impl Fn (usize, usize) -> i32) -> (i32, Vec<usize>) {
    assert!(n <= m);
    const MAX: i32 = 1_000_000_000;
    let mut price_l = vec![0; n+1];
    let mut price_r = vec![0; m+1];
    // let mut match_r2l = vec![None; m+1];
    // let mut way = vec![None; m+1];
    let mut match_r2l = vec![0; m+1];
    let mut way = vec![0; m+1];

    for i in 1 ..= n {
        match_r2l[0] = i;
        let mut j0 = 0;
        let mut min_r = vec![MAX; m+1];
        let mut used = vec![false; m+1];
        loop {
            used[j0] = true;
            let i0 = match_r2l[j0];
            let mut delta = MAX;
            let mut j1 = 0;
            for j in 1 ..= m {
                if !used[j] {
                    let cur = (cost(i0, j)) - price_l[i0] - price_r[j];
                    if cur < min_r[j] {
                        min_r[j] = cur; way[j] = j0;
                    }
                    if min_r[j] < delta {
                        delta = min_r[j];  j1 = j;
                    }
                }
            }
            for j in 0 ..= m {
                if used[j] {
                    price_l[match_r2l[j]] += delta; price_r[j] -= delta;
                } else {
                    min_r[j] -= delta;
                }
            }
            j0 = j1;
            if match_r2l[j0] == 0 { break; }
        }

        loop {
            let j1 = way[j0];
            match_r2l[j0] = match_r2l[j1];
            j0 = j1;
            if j0 == 0 { break; }
        }
    }

  // let mut ans = vec![0; n+1];
  // for j in 1 ..= m {
  //     ans[match_r2l[j]] = j;
  // }

  (-price_r[0], match_r2l)
}

#[derive(Debug)]
pub enum EditOp<T,U> {
    KeepIt(usize),
    Update(usize, U),
    Insert(usize, T),
    Remove(usize),
}

#[derive(Debug)]
pub struct EditOps<T,U>(pub Vec<EditOp<T,U>>);

impl<T,U> EditOps<T,U> {
    pub fn no_changes(size: usize) -> Self {
        Self((0..size).map(EditOp::KeepIt).collect())
    }
}

#[derive(PartialEq, Eq, Clone, PartialOrd, Ord)]
enum EditOpDecision {
    KeepIt,
    Update,
    Insert,
    Remove,
    Noop,
}

// Cost should return 0 if identical. Having cost always less than 200 will prevent insert-delete pattern.
// Use cost u32::MAX if you want the element be always deleted.
// Returned edit sequence will have semi-strictly decreaseing index, which enables inplace application of the seq.
// More specifically, for each element in 'a', it will generate one of KeepIt, Update and Remove.
// For each element missing from A, it will produce Insert at the needed position before progressing to next element on 'a'.
// KeepIt (and Update) is a adition on the conventional edit script algorithms, being a main reason of this regularity.
// With that, it seems we don't need to keep indexes in EditOp, but keeping it for future uses.
pub fn generate_edit_script<S,T>(
    a: &[S],
    mut b: Vec<T>,
    cost: impl Fn(&S, &T) -> u32,
) -> EditOps<T,T> {
    use EditOpDecision::*;

    let m = a.len();
    let n = b.len();

    let del_cost = 100;
    let ins_cost = 100;

    // dp[i][j] = (minimum cost of a[..i] -> b[..j], decision)
    let mut dp = vec![vec![(0, Noop); n + 1]; m + 1];

    // initialize
    for i in 0..=m {
        dp[i][0] = (del_cost * i as u32, Remove);
    }
    for j in 0..=n {
        dp[0][j] = (ins_cost * j as u32, Insert);
    }
    dp[0][0] = (0, Noop);

    // fill DP
    for i in 1..=m {
        for j in 1..=n {
            let del = (dp[i - 1][j].0 + del_cost, Remove);
            let ins = (dp[i][j - 1].0 + ins_cost, Insert);
            let subst_cost = cost(&a[i - 1], &b[j - 1]);
            if subst_cost == u32::MAX {
                dp[i][j] = del;
            } else {
                let subst = (dp[i - 1][j - 1].0 + subst_cost, if subst_cost == 0 { KeepIt } else { Update });
                dp[i][j] = subst.min(del).min(ins);
            }
        }
    }

    // backtrack
    let mut ops = Vec::new();
    let (mut i, mut j) = (m, n);

    loop {
        match dp[i][j].1 {
            Remove => {
                ops.push(EditOp::Remove(i - 1));
                i -= 1;
            }
            Insert => {
                // let item = b[j - 1].clone();
                b.truncate(j); let item = b.pop().unwrap();
                ops.push(EditOp::Insert(i, item));
                j -= 1;
            }
            Update => {
                // let item = b[j - 1].clone();
                b.truncate(j); let item = b.pop().unwrap();
                ops.push(EditOp::Update(i - 1, item));
                i -= 1; j -= 1;
            }
            KeepIt => {
                ops.push(EditOp::KeepIt(i - 1));
                i -= 1; j -= 1;
            }
            Noop => break,
        } // INVARIANT: ops.last.idx == i
    } // INVARIANT: i non-stricly decreases (non-stricity only happens for Insert)
    debug_assert!(i == 0 && j == 0);

    ops.reverse();
    // INVARIANT: op.idx increases by 0 or 1; 0 only if op is Insert (in which case j decreases)
    EditOps(ops)
}

// example of sequence application
#[allow(unused)]
fn apply_edit_script_example<T: Clone>(
    base: &mut Vec<T>,
    ops: EditOps<T,T>,
) {
    let mut cursor = 0;

    for op in ops.0 {
        // INVARIANT: base[0..cursor] is fixed
        match op {
            EditOp::KeepIt(index) => {
                cursor += 1;
            }
            EditOp::Update(index, value) => {
                base[cursor] = value;
                cursor += 1;
            }
            EditOp::Remove(index) => {
                base.remove(cursor);
            }
            EditOp::Insert(index, value) => {
                base.insert(cursor, value);
                cursor += 1;
            }
        }
    }
}

// example of sequence application
#[allow(unused)]
fn apply_edit_script<T,U>(
    mut base: impl Iterator<Item=T>,
    ops: EditOps<T,U>,
    mut apply: impl FnMut(T, U) -> T,
) -> Vec<T> {
    let mut ret = vec![];

    for op in ops.0 {
        // INVARIANT: base[0..cursor] is fixed
        match op {
            EditOp::KeepIt(index) => {
                ret.push(base.next().unwrap())
            }
            EditOp::Update(index, u) => {
                let t = base.next().unwrap();
                ret.push(apply(t, u));
            }
            EditOp::Remove(index) => {
                base.next();
            }
            EditOp::Insert(index, t) => {
                ret.push(t);
            }
        }
    }

    ret
}

#[cfg(test)]
mod test {
    use crate::base::apply_edit_script_example;

    use super::generate_edit_script;

    fn test_round_trip_str(a: &str, b: &str) {
        let mut base: Vec<_> = a.chars().collect();
        let new: Vec<_> = b.chars().collect();

        let seq = generate_edit_script(&base, new.clone(), |a,b| if a==b {0} else {1});
        apply_edit_script_example(&mut base, seq);
        assert_eq!(base, new);
    }

    #[test]
    fn test_round_trip_strs() {
        test_round_trip_str("", "sadfbid");
        test_round_trip_str("sadfbid", "");
        test_round_trip_str("sadfbid", "sadfbid");
        test_round_trip_str("abcdefg", "gbcdfe");
        test_round_trip_str("abcdefg", "gbe");
        test_round_trip_str("bde", "gbcdfe");
    }
}


// Slightly worse than Damerau-Levelstein but cheap and good enough.
// See https://en.wikipedia.org/wiki/Damerau%E2%80%93Levenshtein_distance for more info.
// Uses max_dist bound for early exit.
// 'buffer' should be larger than 3*(b.len()+1).
pub fn bounded_optimal_string_alignment_distance(a: &str, b: &str, max_dist: usize, buffer: &mut [usize]) -> Option<usize> {
    let a = a.as_bytes();
    let b = b.as_bytes();

    let n = a.len();
    let m = b.len();

    // early exit
    if (n as isize - m as isize).abs() as usize > max_dist {
        return None;
    }

    let (mut prev2, buffer) = buffer.split_at_mut(m+1);
    let (mut prev1, buffer) = buffer.split_at_mut(m+1);
    let (mut curr, _buffer) = buffer.split_at_mut(m+1);

    // Initialize first row
    for j in 0..=m {
        prev1[j] = j;
    }

    for i in 1..=n {
        curr[0] = i;
        let mut row_min = curr[0];

        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };

            let mut val =
                (prev1[j] + 1) // deletion
                .min(curr[j - 1] + 1) // insertion
                .min(prev1[j - 1] + cost); // substitution

            // transposition
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                val = val.min(prev2[j - 2] + 1);
            }

            curr[j] = val;
            row_min = row_min.min(val);
        }

        // early exit
        if row_min > max_dist {
            return None;
        }

        // rotate rows
        std::mem::swap(&mut prev2, &mut prev1);
        std::mem::swap(&mut prev1, &mut curr);
    }

    let dist = prev1[m];
    if dist <= max_dist {
        Some(dist)
    } else {
        None
    }
}

/// Gets utf8 sequence of digits, parse u8 number.
/// This code rejects any non-digit character.
/// Numeric digits are ascii characters to be represented with single byte < 0xC0, so it's fine.
pub const fn parse_simple_u8(s: &[u8]) -> Option<u8> {
    const fn upd(r: &mut u8, d: u8) -> bool {
        if d > b'9' { return false; }
        let Some(d) = d.checked_sub(b'0') else {return false};
        let Some(nr) = r.checked_mul(10) else {return false};
        let Some(nr) = nr.checked_add(d) else {return false};
        *r = nr; true
    }
    let mut r = 0u8;
    if s.len() > 0 { if ! upd(&mut r, s[0]) { return None; } }
    if s.len() > 1 { if ! upd(&mut r, s[1]) { return None; } }
    if s.len() > 2 { if ! upd(&mut r, s[2]) { return None; } }
    if s.len() > 3 { return None }
    Some(r)
}

/// Gets utf16 sequence of digits, parse u8 number.
/// This code rejects any non-digit character.
/// Numeric digits are ascii characters to be represented with single byte < 0xC0, so it's fine.
pub const fn parse_simple_u8w(s: &[u16]) -> Option<u8> {
    const fn upd(r: &mut u8, d: u16) -> bool {
        let d = if d <= b'9' as u16 {d as u8} else {return false};
        let Some(d) = d.checked_sub(b'0') else {return false};
        let Some(nr) = r.checked_mul(10) else {return false};
        let Some(nr) = nr.checked_add(d) else {return false};
        *r = nr; true
    }
    let mut r = 0u8;
    if s.len() > 0 { if ! upd(&mut r, s[0]) { return None; } }
    if s.len() > 1 { if ! upd(&mut r, s[1]) { return None; } }
    if s.len() > 2 { if ! upd(&mut r, s[2]) { return None; } }
    if s.len() > 3 { return None }
    Some(r)
}


pub struct Version {
    pub major: u8,
    pub minor: u8,
}

pub const MYTHIC_VERSION: Version = Version {
    major: parse_simple_u8(env!("CARGO_PKG_VERSION_MAJOR").as_bytes()).unwrap(),
    minor: parse_simple_u8(env!("CARGO_PKG_VERSION_MINOR").as_bytes()).unwrap(),
};


pub fn is_version_compatible(spec: &str) -> Option<bool> {

    fn parse_version(input: &str) -> Option<(u64, Option<u64>)> {
        let mut parts = input.trim().split('.');
        let major = parts.next()?.trim().parse::<u64>().ok()?;
        let minor_opt = if let Some(m) = parts.next() {
            let m = m.trim();
            if m.is_empty() { return None; }
            Some(m.parse::<u64>().ok()?)
        } else { None };
        if parts.next().is_some() { return None; }
        Some((major, minor_opt))
    }

    let cur_major = MYTHIC_VERSION.major as u64;
    let cur_minor = MYTHIC_VERSION.minor as u64;

    let spec = spec.trim();
    if spec.is_empty() { return Some(true); }

    let (op, rest) =
    if let Some(s) = spec.strip_prefix('=') {
        ('=', s.trim())
    } else if let Some(s) = spec.strip_prefix('^') {
        ('^', s.trim())
    } else {
        ('^', spec)
    };

    let (req_major, req_minor_opt) = parse_version(rest)?;

    match op {
        '=' => {
            if let Some(req_minor) = req_minor_opt {
                Some(cur_major == req_major && cur_minor == req_minor)
            } else {
                Some(cur_major == req_major)
            }
        }
        '^' => {
            if cur_major != req_major {
                return Some(false);
            }

            if let Some(req_minor) = req_minor_opt {
                Some(cur_minor >= req_minor)
            } else {
                Some(true)
            }
        }
        _ => unreachable!(),
    }
}

/// An Iterator wrapper that will enable processing a task on-the-go while report the the caller right away.
/// If dropped, the iterator will consume itself guraranteeing the task finishes to the end unless halted.
/// Note that halted CoroutineIter would still generate next item. 'halt' only stops automatic consumption.
#[must_use]
pub struct CoroutineIter<T, I> where I: Iterator<Item=T> {
    iter: I,
    halted: bool,
}

impl<T, I> CoroutineIter<T, I> where I: Iterator<Item=T> {
    pub fn new(iter: I) -> Self {
        Self { iter, halted: false }
    }

    pub fn halt(&mut self) {
        self.halted = true;
    }
}

impl<T, I> Iterator for CoroutineIter<T, I> where I: Iterator<Item=T> {
    type Item = T;
    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next()
    }
}

impl<T, I> Drop for CoroutineIter<T, I> where I: Iterator<Item=T> {
    fn drop(&mut self) {
        if self.halted {
            while let Some(_x) = self.next() { }
        }
    }
}