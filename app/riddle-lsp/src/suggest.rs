//! "Did you mean" suggestions for unresolved names.

/// Maximum edit distance for a name to be offered as a suggestion. Longer
/// names tolerate proportionally more edits so `my_counter_value` can still
/// match `my_contor_value`.
#[must_use]
pub fn suggestion_distance_budget(name: &str) -> usize {
    let chars = name.chars().count();
    if chars <= 3 {
        1
    } else if chars <= 8 {
        2
    } else {
        3
    }
}

/// Returns the candidate closest to `target` under Damerau-Levenshtein
/// distance, ignoring candidates whose distance exceeds
/// [`suggestion_distance_budget`] for the target. Ties break toward the
/// lexicographically smaller name so suggestions stay deterministic.
pub fn closest_name<'a>(
    candidates: impl IntoIterator<Item = &'a str>,
    target: &str,
) -> Option<&'a str> {
    let budget = suggestion_distance_budget(target);
    candidates
        .into_iter()
        .filter(|candidate| !candidate.is_empty() && *candidate != target)
        .map(|candidate| (candidate, damerau_levenshtein(target, candidate)))
        .filter(|&(_, distance)| distance <= budget)
        .min_by_key(|&(candidate, distance)| (distance, candidate))
        .map(|(candidate, _)| candidate)
}

/// Damerau-Levenshtein distance (with adjacent transpositions), unrestricted
/// over the two strings. Good enough for short identifiers.
#[must_use]
pub fn damerau_levenshtein(source: &str, target: &str) -> usize {
    let source: Vec<char> = source.chars().collect();
    let target: Vec<char> = target.chars().collect();
    let (rows, columns) = (source.len(), target.len());
    if rows == 0 {
        return columns;
    }
    if columns == 0 {
        return rows;
    }

    // Identifiers are short, so the full `(rows + 1) x (columns + 1)` table is
    // fine and keeps the transposition case simple.
    let mut table = vec![0usize; (rows + 1) * (columns + 1)];
    let index = |row: usize, column: usize| row * (columns + 1) + column;
    for row in 0..=rows {
        table[index(row, 0)] = row;
    }
    for column in 0..=columns {
        table[index(0, column)] = column;
    }
    for row in 1..=rows {
        for column in 1..=columns {
            let substitution_cost = usize::from(source[row - 1] != target[column - 1]);
            let mut value = (table[index(row - 1, column)] + 1)
                .min(table[index(row, column - 1)] + 1)
                .min(table[index(row - 1, column - 1)] + substitution_cost);
            if row > 1
                && column > 1
                && source[row - 1] == target[column - 2]
                && source[row - 2] == target[column - 1]
            {
                value = value.min(table[index(row - 2, column - 2)] + 1);
            }
            table[index(row, column)] = value;
        }
    }
    table[index(rows, columns)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_counts_edits_and_transpositions() {
        assert_eq!(damerau_levenshtein("abc", "abc"), 0);
        assert_eq!(damerau_levenshtein("abc", "abd"), 1);
        assert_eq!(damerau_levenshtein("abc", "acb"), 1);
        assert_eq!(damerau_levenshtein("foo", ""), 3);
    }

    #[test]
    fn closest_name_respects_budget_and_determinism() {
        assert_eq!(
            closest_name(["lenght", "lengthy", "width"], "lenght"),
            Some("lengthy")
        );
        assert_eq!(closest_name(["a", "b"], "zzz"), None);
        assert_eq!(closest_name(["ab", "ba", "cd"], "ab"), Some("ba"));
    }
}
