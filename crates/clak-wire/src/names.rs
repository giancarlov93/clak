//! The right name for a misspelled one: `--intnet` → `intent`. Used by the hub (parameters,
//! commands, programs) and the command line: an error that also says "did you mean…?" gets
//! fixed on the first try.

/// The closest of `names`, if it is close enough to be a typo and not another name.
/// Case, hyphens and underscores don't count.
pub fn closest<'a, I, S>(wrong: Option<&str>, names: I) -> Option<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str> + 'a,
{
    let plain = plain(wrong.unwrap_or(""));
    if plain.is_empty() {
        return None;
    }
    let mut best: Option<String> = None;
    let mut best_distance = usize::MAX;
    for name in names {
        let distance = distance(&plain, &plain_of(name.as_ref()));
        if distance < best_distance {
            best = Some(name.as_ref().to_owned());
            best_distance = distance;
        }
    }
    let limit = (plain.len() / 3).clamp(1, 3);
    if best_distance <= limit { best } else { None }
}

fn plain(text: &str) -> Vec<char> {
    plain_of(text)
}

fn plain_of(text: &str) -> Vec<char> {
    text.chars()
        .filter(|c| *c != '-' && *c != '_')
        .flat_map(char::to_lowercase)
        .collect()
}

/// The edits to get from one name to the other; two swapped letters count as one.
fn distance(a: &[char], b: &[char]) -> usize {
    let (n, m) = (a.len(), b.len());
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[n][m]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_the_closest_name() {
        assert_eq!(
            closest(Some("intnet"), ["intent", "other"]),
            Some("intent".into())
        );
        assert_eq!(
            closest(Some("get-text"), ["get_text"]),
            Some("get_text".into())
        );
        assert_eq!(closest(Some("zzz"), ["intent"]), None);
        assert_eq!(closest(None, ["intent"]), None);
    }
}
