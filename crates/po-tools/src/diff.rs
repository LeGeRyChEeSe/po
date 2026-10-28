//! Line diff of a file change, for display: what an `edit` or a `write` added and removed.
//!
//! The format is a small unified diff: a header `@@ <first line> +<added> -<removed>`, then the
//! lines, each prefixed by `+`, `-` or a space (context). The counts cover the whole change even
//! when the listing is cut, so a folded call can show them alone.

/// Context lines kept around each change.
const CONTEXT: usize = 2;
/// Above this many lines on each side, the middle is not aligned line by line: it is shown as
/// removed then added (the counts stay exact for what differs).
const ALIGN_LIMIT: usize = 400;

/// Diff between two versions of a file; `None` when nothing changed.
pub fn line_diff(before: &str, after: &str, max_lines: usize) -> Option<String> {
    let a: Vec<&str> = before.lines().collect();
    let b: Vec<&str> = after.lines().collect();
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..].iter().rev().zip(b[prefix..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (mid_a, mid_b) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);
    if mid_a.is_empty() && mid_b.is_empty() {
        return None;
    }

    // Operations over the middle: ' ' kept, '-' removed, '+' added.
    let ops: Vec<(char, &str)> = if mid_a.len() <= ALIGN_LIMIT && mid_b.len() <= ALIGN_LIMIT {
        align(mid_a, mid_b)
    } else {
        mid_a.iter().map(|l| ('-', *l)).chain(mid_b.iter().map(|l| ('+', *l))).collect()
    };
    let added = ops.iter().filter(|(op, _)| *op == '+').count();
    let removed = ops.iter().filter(|(op, _)| *op == '-').count();

    let mut lines = Vec::new();
    let lead = prefix.saturating_sub(CONTEXT);
    for line in &a[lead..prefix] {
        lines.push(format!(" {line}"));
    }
    // Long runs of kept lines inside the middle shrink to their context.
    let mut i = 0;
    while i < ops.len() {
        let run = ops[i..].iter().take_while(|(op, _)| *op == ' ').count();
        if run > 2 * CONTEXT + 1 {
            for (_, line) in &ops[i..i + CONTEXT] {
                lines.push(format!(" {line}"));
            }
            lines.push("…".to_owned());
            for (_, line) in &ops[i + run - CONTEXT..i + run] {
                lines.push(format!(" {line}"));
            }
            i += run;
        } else if run > 0 {
            for (_, line) in &ops[i..i + run] {
                lines.push(format!(" {line}"));
            }
            i += run;
        } else {
            let (op, line) = ops[i];
            lines.push(format!("{op}{line}"));
            i += 1;
        }
    }
    for line in a[a.len() - suffix..].iter().take(CONTEXT) {
        lines.push(format!(" {line}"));
    }
    if lines.len() > max_lines {
        let hidden = lines.len() - max_lines;
        lines.truncate(max_lines);
        lines.push(format!("… +{hidden}"));
    }
    Some(format!("@@ {} +{added} -{removed}\n{}", lead + 1, lines.join("\n")))
}

/// Longest common subsequence, as a sequence of operations.
fn align<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<(char, &'a str)> {
    let (n, m) = (a.len(), b.len());
    let mut table = vec![vec![0u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if a[i] == b[j] { table[i + 1][j + 1] + 1 } else { table[i + 1][j].max(table[i][j + 1]) };
        }
    }
    let (mut i, mut j, mut ops) = (0, 0, Vec::with_capacity(n + m));
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            ops.push((' ', a[i]));
            i += 1;
            j += 1;
        } else if j < m && (i == n || table[i][j + 1] >= table[i + 1][j]) {
            ops.push(('+', b[j]));
            j += 1;
        } else {
            ops.push(('-', a[i]));
            i += 1;
        }
    }
    // Removals before additions within a change read more naturally.
    let mut k = 0;
    while k < ops.len() {
        let end = k + ops[k..].iter().take_while(|(op, _)| *op != ' ').count();
        ops[k..end].sort_by_key(|(op, _)| if *op == '-' { 0 } else { 1 });
        k = end.max(k + 1);
    }
    ops
}

/// The counts of a diff made by [`line_diff`]: `(added, removed)`.
pub fn counts(diff: &str) -> Option<(usize, usize)> {
    let header = diff.lines().next()?.strip_prefix("@@ ")?;
    let mut parts = header.split(' ').skip(1);
    let added = parts.next()?.strip_prefix('+')?.parse().ok()?;
    let removed = parts.next()?.strip_prefix('-')?.parse().ok()?;
    Some((added, removed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_replaced_line_shows_as_removed_then_added_with_context() {
        let before = "a\nb\nc\nd\ne\nf\n";
        let after = "a\nb\nc\nD\ne\nf\n";
        let diff = line_diff(before, after, 50).unwrap();
        assert_eq!(diff, "@@ 2 +1 -1\n b\n c\n-d\n+D\n e\n f");
        assert_eq!(counts(&diff), Some((1, 1)));
    }

    #[test]
    fn a_new_file_is_all_added_and_a_long_listing_is_cut() {
        let after: String = (0..30).map(|i| format!("line {i}\n")).collect();
        let diff = line_diff("", &after, 10).unwrap();
        assert_eq!(counts(&diff), Some((30, 0)), "the counts stay exact");
        assert!(diff.ends_with("… +20"));
    }

    #[test]
    fn far_apart_changes_keep_only_their_context() {
        let before: String = (0..40).map(|i| format!("{i}\n")).collect();
        let after: String = (0..40)
            .map(|i| match i {
                5 => "five\n".to_owned(),
                35 => "thirty-five\n".to_owned(),
                i => format!("{i}\n"),
            })
            .collect();
        let diff = line_diff(&before, &after, 100).unwrap();
        assert_eq!(counts(&diff), Some((2, 2)));
        assert!(diff.contains("\n…\n"), "{diff}");
        assert!(!diff.contains(" 20\n"));
    }

    #[test]
    fn identical_versions_have_no_diff() {
        assert_eq!(line_diff("x\n", "x\n", 10), None);
    }
}
