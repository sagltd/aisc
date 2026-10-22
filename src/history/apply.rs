//! Pure line-level application of blocks: exact `splice`/`unsplice` and fuzzy `transplant`.
use super::model::{Block, Line};
use std::fmt;

pub(crate) fn split_lines(bytes: &[u8]) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            lines.push(bytes[start..=index].to_vec());
            start = index + 1;
        }
    }
    if start < bytes.len() {
        lines.push(bytes[start..].to_vec());
    }
    lines
}

pub(crate) fn join_lines(lines: &[Line]) -> Vec<u8> {
    lines.concat()
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ApplyError {
    /// The recorded lines are not at the recorded position.
    Mismatch { block: usize },
    /// Blocks are unsorted or overlap.
    Overlap { block: usize },
    /// No significant place for the block in the target.
    Missing { block: usize },
    /// Several equally good places.
    Ambiguous { block: usize },
}

impl fmt::Display for ApplyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mismatch { block } => write!(formatter, "block {block} does not match its base"),
            Self::Overlap { block } => write!(formatter, "block {block} overlaps another"),
            Self::Missing { block } => write!(formatter, "block {block} has no matching place"),
            Self::Ambiguous { block } => {
                write!(
                    formatter,
                    "block {block} matches several places equally well"
                )
            }
        }
    }
}
impl std::error::Error for ApplyError {}

/// Applies `blocks` (sorted by `old_pos`) to `old`, checking that each removed run is present.
pub(crate) fn splice(old: &[Line], blocks: &[&Block]) -> Result<Vec<Line>, ApplyError> {
    let mut output = Vec::with_capacity(old.len());
    let mut cursor = 0;
    for (index, block) in blocks.iter().enumerate() {
        let end = block.old_pos + block.removed.len();
        if block.old_pos < cursor {
            return Err(ApplyError::Overlap { block: index });
        }
        if end > old.len() || old[block.old_pos..end] != block.removed[..] {
            return Err(ApplyError::Mismatch { block: index });
        }
        output.extend_from_slice(&old[cursor..block.old_pos]);
        output.extend(block.added.iter().cloned());
        cursor = end;
    }
    output.extend_from_slice(&old[cursor..]);
    Ok(output)
}

/// Exact inverse of `splice`: removes the added runs found at `new_pos` and restores removed runs.
pub(crate) fn unsplice(new: &[Line], blocks: &[&Block]) -> Result<Vec<Line>, ApplyError> {
    let mut output = Vec::with_capacity(new.len());
    let mut cursor = 0;
    for (index, block) in blocks.iter().enumerate() {
        let end = block.new_pos + block.added.len();
        if block.new_pos < cursor {
            return Err(ApplyError::Overlap { block: index });
        }
        if end > new.len() || new[block.new_pos..end] != block.added[..] {
            return Err(ApplyError::Mismatch { block: index });
        }
        output.extend_from_slice(&new[cursor..block.new_pos]);
        output.extend(block.removed.iter().cloned());
        cursor = end;
    }
    output.extend_from_slice(&new[cursor..]);
    Ok(output)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    Forward,
    Reverse,
}

fn significant(line: &[u8]) -> bool {
    line.iter()
        .filter(|byte| byte.is_ascii_alphanumeric())
        .count()
        >= 3
}

struct Placement {
    index: usize,
    length: usize,
    added: Vec<Line>,
    order: usize,
}

/// Re-applies each block to a different `target`, locating it by its removed lines and context.
/// Every block is located on the original target; the best place must be unique and significant.
pub(crate) fn transplant(
    target: &[Line],
    blocks: &[&Block],
    direction: Direction,
) -> Result<Vec<Line>, ApplyError> {
    let mut placements = Vec::with_capacity(blocks.len());
    for (order, block) in blocks.iter().enumerate() {
        let (removed, added) = match direction {
            Direction::Forward => (&block.removed, &block.added),
            Direction::Reverse => (&block.added, &block.removed),
        };
        let mut best: Option<(usize, usize)> = None; // (score, index)
        let mut tie = false;
        let candidates: Vec<usize> = if removed.is_empty() {
            (0..=target.len()).collect()
        } else {
            (0..target.len().saturating_sub(removed.len() - 1))
                .filter(|index| target[*index..*index + removed.len()] == removed[..])
                .collect()
        };
        for index in candidates {
            let end = index + removed.len();
            let mut matched_before = 0;
            while matched_before < block.before.len()
                && index > matched_before
                && target[index - 1 - matched_before]
                    == block.before[block.before.len() - 1 - matched_before]
            {
                matched_before += 1;
            }
            let mut matched_after = 0;
            while matched_after < block.after.len()
                && end + matched_after < target.len()
                && target[end + matched_after] == block.after[matched_after]
            {
                matched_after += 1;
            }
            let anchor = (index == 0 && block.at_start) || (end == target.len() && block.at_eof);
            let score = matched_before + matched_after + usize::from(anchor);
            let strong = removed.iter().any(|line| significant(line))
                || block.before[block.before.len() - matched_before..]
                    .iter()
                    .chain(&block.after[..matched_after])
                    .any(|line| significant(line))
                || anchor;
            if !strong || (removed.is_empty() && score == 0) {
                continue;
            }
            match best {
                Some((top, _)) if score < top => {}
                Some((top, _)) if score == top => tie = true,
                _ => {
                    best = Some((score, index));
                    tie = false;
                }
            }
        }
        let Some((_, index)) = best else {
            return Err(ApplyError::Missing { block: order });
        };
        if tie {
            return Err(ApplyError::Ambiguous { block: order });
        }
        placements.push(Placement {
            index,
            length: removed.len(),
            added: added.clone(),
            order,
        });
    }
    placements.sort_by_key(|placement| (placement.index, placement.order));
    let mut output = Vec::with_capacity(target.len());
    let mut cursor = 0;
    for placement in &placements {
        if placement.index < cursor {
            return Err(ApplyError::Overlap {
                block: placement.order,
            });
        }
        output.extend_from_slice(&target[cursor..placement.index]);
        output.extend(placement.added.iter().cloned());
        cursor = placement.index + placement.length;
    }
    output.extend_from_slice(&target[cursor..]);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<Line> {
        split_lines(text.as_bytes())
    }

    fn block(
        old_pos: usize,
        removed: &str,
        added: &str,
        before: &str,
        after: &str,
        at_start: bool,
        at_eof: bool,
    ) -> Block {
        Block {
            old_pos,
            new_pos: old_pos,
            removed: lines(removed),
            added: lines(added),
            before: lines(before),
            after: lines(after),
            at_start,
            at_eof,
        }
    }

    #[test]
    fn splice_and_unsplice_round_trip_with_missing_final_newline() {
        let old = lines("a\nb\nc");
        let change = block(1, "b\n", "B1\nB2\n", "a\n", "c", false, true);
        let new = splice(&old, &[&change]).unwrap();
        assert_eq!(join_lines(&new), b"a\nB1\nB2\nc");
        let back = unsplice(&new, &[&change]).unwrap();
        assert_eq!(back, old);
        let last = block(2, "c", "c\n", "b\n", "", false, true);
        let appended = splice(&old, &[&last]).unwrap();
        assert_eq!(join_lines(&appended), b"a\nb\nc\n");
    }

    #[test]
    fn splice_rejects_mismatch_and_overlap() {
        let old = lines("a\nb\n");
        let wrong = block(0, "x\n", "y\n", "", "", true, false);
        assert_eq!(
            splice(&old, &[&wrong]),
            Err(ApplyError::Mismatch { block: 0 })
        );
        let first = block(0, "a\nb\n", "", "", "", true, true);
        let second = block(1, "b\n", "", "", "", false, true);
        assert_eq!(
            splice(&old, &[&first, &second]),
            Err(ApplyError::Overlap { block: 1 })
        );
    }

    #[test]
    fn transplant_follows_shifted_context() {
        let target = lines("new first\nintro\nalpha\nbeta\ngamma\ndelta\n");
        let moved = block(
            1,
            "beta\n",
            "BETA\n",
            "intro\nalpha\n",
            "gamma\ndelta\n",
            false,
            false,
        );
        let result = transplant(&target, &[&moved], Direction::Forward).unwrap();
        assert_eq!(
            join_lines(&result),
            b"new first\nintro\nalpha\nBETA\ngamma\ndelta\n"
        );
    }

    #[test]
    fn transplant_scores_context_to_break_ties() {
        let target = lines("one\nsame\ntwo\nsame\nthree\n");
        let second = block(3, "same\n", "SAME\n", "two\n", "three\n", false, false);
        let result = transplant(&target, &[&second], Direction::Forward).unwrap();
        assert_eq!(join_lines(&result), b"one\nsame\ntwo\nSAME\nthree\n");
    }

    #[test]
    fn transplant_reports_ambiguity_and_missing() {
        let target = lines("same\nsame\n");
        let tied = block(0, "same\n", "SAME\n", "", "", false, false);
        assert_eq!(
            transplant(&target, &[&tied], Direction::Forward),
            Err(ApplyError::Ambiguous { block: 0 })
        );
        let absent = block(0, "other\n", "x\n", "", "", false, false);
        assert_eq!(
            transplant(&target, &[&absent], Direction::Forward),
            Err(ApplyError::Missing { block: 0 })
        );
    }

    #[test]
    fn transplant_rejects_insignificant_matches_but_accepts_anchors() {
        let target = lines("}\n}\n");
        let brace = block(0, "}\n", "};\n", "", "", false, false);
        assert_eq!(
            transplant(&target, &[&brace], Direction::Forward),
            Err(ApplyError::Missing { block: 0 })
        );
        let anchored_insert = block(0, "", "// header\n", "", "", true, false);
        let result = transplant(&target, &[&anchored_insert], Direction::Forward).unwrap();
        assert_eq!(join_lines(&result), b"// header\n}\n}\n");
    }

    #[test]
    fn transplant_pure_insertion_needs_context_and_keeps_order_at_same_point() {
        let target = lines("head line\ntail line\n");
        let first = block(1, "", "one\n", "head line\n", "tail line\n", false, false);
        let second = block(1, "", "two\n", "head line\n", "tail line\n", false, false);
        let result = transplant(&target, &[&first, &second], Direction::Forward).unwrap();
        assert_eq!(join_lines(&result), b"head line\none\ntwo\ntail line\n");
        let bare = block(1, "", "x\n", "", "", false, false);
        assert_eq!(
            transplant(&target, &[&bare], Direction::Forward),
            Err(ApplyError::Missing { block: 0 })
        );
    }

    #[test]
    fn reverse_transplant_removes_added_lines() {
        let target = lines("keep one\nadded line\nkeep two\n");
        let change = block(
            1,
            "",
            "added line\n",
            "keep one\n",
            "keep two\n",
            false,
            false,
        );
        let result = transplant(&target, &[&change], Direction::Reverse).unwrap();
        assert_eq!(join_lines(&result), b"keep one\nkeep two\n");
    }

    #[test]
    fn crlf_lines_are_matched_byte_for_byte() {
        let old = lines("a\r\nb\r\n");
        let change = block(1, "b\r\n", "B\r\n", "a\r\n", "", false, true);
        assert_eq!(
            join_lines(&splice(&old, &[&change]).unwrap()),
            b"a\r\nB\r\n"
        );
        let lf_target = lines("a\nb\n");
        assert!(transplant(&lf_target, &[&change], Direction::Forward).is_err());
    }
}
