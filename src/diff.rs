//! Which characters of a rename actually changed.
//!
//! A rename prints two long names, and colouring both whole leaves the reader
//! comparing them character by character. This trims the shared start and end,
//! anchors on the longest run the two middles still share, and recurses either
//! side of it, so two edits to one name stay two marks.
//!
//! The anchor is a substring, not a longest common subsequence: an LCS pairs
//! scattered characters, so letters that two unrelated names happen to share
//! read as kept and the result turns to confetti. Requiring `MIN_ANCHOR`
//! contiguous characters means a short coincidence can never become a match.
//! Names with nothing in common need no special case, since no anchor clears
//! the threshold and both print as one solid block.

/// How many characters must agree in a row before a match means anything.
const MIN_ANCHOR: usize = 4;
/// Longest pair worth the quadratic search for an anchor.
const LIMIT: usize = 512;

/// For each character of `from` and of `to`, whether it survived the rename.
///
/// Anything false is what the rename took away (in `from`) or added (in `to`).
pub fn common(from: &str, to: &str) -> (Vec<bool>, Vec<bool>) {
    let old: Vec<char> = from.chars().collect();
    let new: Vec<char> = to.chars().collect();
    let mut kept_old = vec![false; old.len()];
    let mut kept_new = vec![false; new.len()];
    align(&old, &new, 0, 0, &mut kept_old, &mut kept_new);
    (kept_old, kept_new)
}

/// Marks what `old` and `new` share, writing into the masks at the offsets the
/// two slices sit at in the whole name.
fn align(
    old: &[char],
    new: &[char],
    at_old: usize,
    at_new: usize,
    kept_old: &mut [bool],
    kept_new: &mut [bool],
) {
    let (n, m) = (old.len(), new.len());
    let most = n.min(m);

    let head = (0..most).take_while(|&i| old[i] == new[i]).count();
    // The tail may not eat into the head: with nothing between them there is
    // no change left to show.
    let tail = (0..most - head)
        .take_while(|&k| old[n - 1 - k] == new[m - 1 - k])
        .count();

    kept_old[at_old..at_old + head].fill(true);
    kept_new[at_new..at_new + head].fill(true);
    kept_old[at_old + n - tail..at_old + n].fill(true);
    kept_new[at_new + m - tail..at_new + m].fill(true);

    let (old_mid, new_mid) = (&old[head..n - tail], &new[head..m - tail]);
    if old_mid.is_empty() || new_mid.is_empty() {
        return;
    }

    // What is left is a change on both sides unless they still share a run
    // long enough to mean something, in which case it is really two changes
    // with untouched text between them.
    let Some((o, e, len)) = anchor(old_mid, new_mid) else {
        return;
    };

    let (o_at, n_at) = (at_old + head, at_new + head);
    align(&old_mid[..o], &new_mid[..e], o_at, n_at, kept_old, kept_new);
    kept_old[o_at + o..o_at + o + len].fill(true);
    kept_new[n_at + e..n_at + e + len].fill(true);
    align(
        &old_mid[o + len..],
        &new_mid[e + len..],
        o_at + o + len,
        n_at + e + len,
        kept_old,
        kept_new,
    );
}

/// The longest run of characters the two share, if it is long enough to be
/// worth trusting: where it starts in each, and how long it is.
fn anchor(old: &[char], new: &[char]) -> Option<(usize, usize, usize)> {
    if old.len() > LIMIT || new.len() > LIMIT {
        return None;
    }

    let mut prev = vec![0usize; new.len() + 1];
    let mut best = (0, 0, 0);
    for (i, o) in old.iter().enumerate() {
        let mut row = vec![0usize; new.len() + 1];
        for (j, e) in new.iter().enumerate() {
            if o == e {
                row[j + 1] = prev[j] + 1;
                if row[j + 1] > best.2 {
                    best = (i + 1 - row[j + 1], j + 1 - row[j + 1], row[j + 1]);
                }
            }
        }
        prev = row;
    }

    (best.2 >= MIN_ANCHOR).then_some(best)
}

#[cfg(test)]
mod tests {
    use super::common;

    fn marks(kept: &[bool]) -> String {
        kept.iter().map(|&k| if k { '=' } else { 'x' }).collect()
    }

    #[test]
    fn one_edit_leaves_the_rest_untouched() {
        let (from, to) = common("track one.flac", "track two.flac");
        assert_eq!(marks(&from), "======xxx=====");
        assert_eq!(marks(&to), "======xxx=====");
    }

    #[test]
    fn two_edits_stay_two_marks_with_kept_text_between() {
        let (from, _) = common("aaaa 1111 bbbb", "aaaa 2222 bbbb");
        assert_eq!(marks(&from), "=====xxxx=====");
    }

    /// A short coincidence must not count as a match, or unrelated names come
    /// out as confetti instead of one solid block.
    #[test]
    fn names_with_nothing_in_common_print_as_one_block() {
        let (from, to) = common("zzzz", "qqqq");
        assert!(from.iter().all(|&k| !k));
        assert!(to.iter().all(|&k| !k));
    }
}
