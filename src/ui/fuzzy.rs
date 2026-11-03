//! Fuzzy matching shared by the command palette, the `gt` cell picker and the
//! files picker. Callers keep one `Utf32String` haystack per item and pass
//! `h.slice(..)`.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as MatcherConfig, Matcher, Utf32Str};

pub struct Fuzzy {
    matcher: Matcher,
    pattern: Option<Pattern>,
}

impl Fuzzy {
    /// `paths` turns on nucleo's path matching (bonus after `/`).
    pub fn new(paths: bool) -> Self {
        let config = if paths {
            MatcherConfig::DEFAULT.match_paths()
        } else {
            MatcherConfig::DEFAULT
        };
        Fuzzy {
            matcher: Matcher::new(config),
            pattern: None,
        }
    }

    pub fn set_query(&mut self, q: &str) {
        self.pattern = if q.is_empty() {
            None
        } else {
            Some(Pattern::parse(
                q,
                CaseMatching::Ignore,
                Normalization::Smart,
            ))
        };
    }

    /// The score of one haystack, or `None` when it does not match. With no
    /// query, everything scores `Some(0)`.
    pub fn score(&mut self, hay: Utf32Str<'_>) -> Option<u32> {
        match &self.pattern {
            Some(p) => p.score(hay, &mut self.matcher),
            None => Some(0),
        }
    }

    /// The matched character positions in `hay`, sorted and unique.
    pub fn indices(&mut self, hay: Utf32Str<'_>, out: &mut Vec<u32>) {
        out.clear();
        if let Some(p) = &self.pattern {
            p.indices(hay, &mut self.matcher, out);
        }
        out.sort_unstable();
        out.dedup();
    }

    /// The indices of the matching items, best first. Each item is
    /// `(index, haystack, tie-break length)`; equal scores put the shorter
    /// length first, then the lower index.
    pub fn rank<'a, I>(&mut self, items: I) -> Vec<u32>
    where
        I: Iterator<Item = (u32, Utf32Str<'a>, u32)>,
    {
        let mut scored: Vec<(u32, u32, u32)> = items
            .filter_map(|(i, hay, len)| self.score(hay).map(|s| (i, s, len)))
            .collect();
        scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)).then(a.0.cmp(&b.0)));
        scored.into_iter().map(|(i, _, _)| i).collect()
    }
}

#[cfg(test)]
mod tests {
    use nucleo_matcher::Utf32String;

    use super::*;

    fn hays(items: &[&str]) -> Vec<Utf32String> {
        items.iter().map(|s| Utf32String::from(*s)).collect()
    }

    fn rank(f: &mut Fuzzy, h: &[Utf32String], lens: &[u32]) -> Vec<u32> {
        f.rank(
            h.iter()
                .enumerate()
                .map(|(i, h)| (i as u32, h.slice(..), lens[i])),
        )
    }

    #[test]
    fn empty_query_keeps_order() {
        let mut f = Fuzzy::new(false);
        let h = hays(&["b", "a", "c"]);
        assert_eq!(rank(&mut f, &h, &[0, 0, 0]), vec![0, 1, 2]);
    }

    #[test]
    fn non_match_is_dropped() {
        let mut f = Fuzzy::new(false);
        f.set_query("xyz");
        let h = hays(&["abc", "xaybzc", "nothing"]);
        assert_eq!(rank(&mut f, &h, &[0, 0, 0]), vec![1]);
    }

    #[test]
    fn equal_scores_keep_index_order_with_zero_length() {
        let mut f = Fuzzy::new(false);
        f.set_query("ab");
        let h = hays(&["ab", "ab", "ab"]);
        assert_eq!(rank(&mut f, &h, &[0, 0, 0]), vec![0, 1, 2]);
    }

    #[test]
    fn equal_scores_put_shorter_length_first() {
        let mut f = Fuzzy::new(false);
        f.set_query("ab");
        let h = hays(&["ab", "ab"]);
        assert_eq!(rank(&mut f, &h, &[5, 3]), vec![1, 0]);
    }

    #[test]
    fn path_query_prefers_contiguous_name() {
        let mut f = Fuzzy::new(true);
        f.set_query("config");
        let h = hays(&["c/o/n/f/i/g.txt", "config.py"]);
        let lens = [15, 9];
        assert_eq!(rank(&mut f, &h, &lens), vec![1, 0]);
    }

    #[test]
    fn indices_are_sorted_and_unique() {
        let mut f = Fuzzy::new(true);
        f.set_query("cfg cfg");
        let h = Utf32String::from("src/config.rs");
        let mut out = vec![99];
        f.indices(h.slice(..), &mut out);
        assert!(!out.is_empty());
        assert!(out.windows(2).all(|w| w[0] < w[1]));
    }
}
