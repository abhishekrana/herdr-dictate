//! Word replacements applied to every transcript: the user's fix for a term
//! the model keeps writing wrong.
//!
//! A replacement matches whole words only, so `Basil` never rewrites
//! `Basilisk`. A `from` written in lowercase matches any casing; one with a
//! capital matches only as written, which keeps `Herder` from catching the
//! ordinary word. A match that opens with a capital opens its replacement with
//! one too, since that is a sentence start. The text is scanned once, left to
//! right, and the longest `from` wins where several start at the same place,
//! so a replacement's output is never rewritten by another.

use serde::Deserialize;

/// One `[[engine.replace]]` entry.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, try_from = "RawReplacement")]
pub struct Replacement {
    from: String,
    to: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawReplacement {
    from: String,
    to: String,
}

impl TryFrom<RawReplacement> for Replacement {
    type Error = String;

    fn try_from(raw: RawReplacement) -> Result<Self, Self::Error> {
        let from = raw.from.split_whitespace().collect::<Vec<_>>().join(" ");
        if from.is_empty() {
            return Err("a replacement needs a non-empty `from`".into());
        }
        Ok(Self { from, to: raw.to })
    }
}

impl Replacement {
    #[cfg(test)]
    pub fn new(from: &str, to: &str) -> Self {
        Self::try_from(RawReplacement {
            from: from.into(),
            to: to.into(),
        })
        .expect("a non-empty from")
    }

    /// The length in bytes of a match at the start of `text`, if there is one.
    fn match_len(&self, text: &str) -> Option<usize> {
        let exact = self.from.chars().any(char::is_uppercase);
        let mut wanted = self.from.chars();
        let mut len = 0;
        for c in text.chars() {
            let Some(w) = wanted.next() else { break };
            let same = if exact {
                c == w
            } else {
                c.to_lowercase().eq(w.to_lowercase())
            };
            if !same {
                return None;
            }
            len += c.len_utf8();
        }
        if wanted.next().is_some() {
            return None;
        }
        let ends_word = text[len..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric());
        ends_word.then_some(len)
    }

    /// `to`, opened with a capital when the matched text opened with one.
    fn render(&self, matched: &str) -> String {
        let capital = matched.chars().next().is_some_and(char::is_uppercase);
        let mut to = self.to.chars();
        match to.next() {
            Some(first) if capital && first.is_lowercase() => {
                first.to_uppercase().chain(to).collect()
            }
            _ => self.to.clone(),
        }
    }
}

/// The longest replacement matching at the start of `text`; the first listed
/// on a tie.
fn longest<'a>(replacements: &'a [Replacement], text: &str) -> Option<(&'a Replacement, usize)> {
    let mut best: Option<(&Replacement, usize)> = None;
    for replacement in replacements {
        if let Some(len) = replacement.match_len(text)
            && best.is_none_or(|(_, longest)| len > longest)
        {
            best = Some((replacement, len));
        }
    }
    best
}

/// Apply `replacements` to `text`.
pub fn apply(replacements: &[Replacement], text: &str) -> String {
    if replacements.is_empty() {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    let mut starts_word = true;
    while at < text.len() {
        let rest = &text[at..];
        let found = starts_word.then(|| longest(replacements, rest)).flatten();
        if let Some((replacement, len)) = found {
            out.push_str(&replacement.render(&rest[..len]));
            at += len;
            starts_word = !rest[..len].ends_with(char::is_alphanumeric);
            continue;
        }
        let c = rest.chars().next().expect("not at the end");
        out.push(c);
        at += c.len_utf8();
        starts_word = !c.is_alphanumeric();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> Vec<Replacement> {
        vec![
            Replacement::new("work tree", "worktree"),
            Replacement::new(".files", "dotfiles"),
            Replacement::new("Herder", "Herdr"),
            Replacement::new("Basil", "Bazel"),
            Replacement::new("cube control", "kubectl"),
        ]
    }

    #[test]
    fn whole_words_are_replaced() {
        assert_eq!(
            apply(&table(), "Open a merge request from this work tree."),
            "Open a merge request from this worktree."
        );
        assert_eq!(
            apply(&table(), "Check my .files repo"),
            "Check my dotfiles repo"
        );
        assert_eq!(
            apply(&table(), "Run cube control get pods"),
            "Run kubectl get pods"
        );
    }

    #[test]
    fn part_of_a_word_is_left_alone() {
        assert_eq!(apply(&table(), "Basilisk"), "Basilisk");
        assert_eq!(apply(&table(), "networks tree"), "networks tree");
        assert_eq!(apply(&table(), "rework tree"), "rework tree");
        assert_eq!(apply(&table(), "Herders"), "Herders");
    }

    #[test]
    fn a_lowercase_from_matches_any_casing() {
        assert_eq!(
            apply(&table(), "the Work Tree is clean"),
            "the Worktree is clean"
        );
        assert_eq!(apply(&table(), "Cube control"), "Kubectl");
    }

    #[test]
    fn a_capitalised_from_matches_only_as_written() {
        assert_eq!(apply(&table(), "the herder moved"), "the herder moved");
        assert_eq!(apply(&table(), "The Herder plugin"), "The Herdr plugin");
        assert_eq!(apply(&table(), "pesto needs basil"), "pesto needs basil");
    }

    #[test]
    fn a_sentence_start_keeps_its_capital() {
        assert_eq!(apply(&table(), "Work tree is ready."), "Worktree is ready.");
        assert_eq!(apply(&table(), "work tree is ready."), "worktree is ready.");
    }

    #[test]
    fn punctuation_ends_a_word() {
        assert_eq!(apply(&table(), "Herder, then Basil."), "Herdr, then Bazel.");
        assert_eq!(apply(&table(), "(work tree)"), "(worktree)");
    }

    #[test]
    fn the_longest_match_wins_and_output_is_not_rescanned() {
        let table = vec![
            Replacement::new("merge", "MERGE"),
            Replacement::new("merge request", "MR"),
            Replacement::new("MR", "never"),
        ];
        assert_eq!(apply(&table, "open a merge request"), "open a MR");
        assert_eq!(apply(&table, "merge it"), "MERGE it");
    }

    #[test]
    fn no_table_is_the_identity() {
        assert_eq!(apply(&[], "Herder work tree"), "Herder work tree");
    }

    #[test]
    fn non_ascii_text_is_walked_by_character() {
        assert_eq!(apply(&table(), "café work tree ✓"), "café worktree ✓");
    }

    #[test]
    fn config_parses_and_refuses_an_empty_from() {
        #[derive(Deserialize)]
        struct Wrapper {
            replace: Vec<Replacement>,
        }
        let parsed: Wrapper =
            toml::from_str("[[replace]]\nfrom = \"work  tree\"\nto = \"worktree\"\n").unwrap();
        assert_eq!(
            parsed.replace,
            vec![Replacement::new("work tree", "worktree")]
        );
        assert!(toml::from_str::<Wrapper>("[[replace]]\nfrom = \" \"\nto = \"x\"\n").is_err());
        assert!(
            toml::from_str::<Wrapper>("[[replace]]\nfrom = \"a\"\nto = \"b\"\nwith = 1\n").is_err(),
            "a misspelt key is refused"
        );
    }
}
