//! The launcher's search: a query is scored against a candidate name in bands.
//!
//! The bands are deliberately far apart, so a better *kind* of match always wins over a longer or
//! earlier one: exact (3000) > prefix (1401–2000) > word start (1400) > substring (601–1200) >
//! subsequence (100–600). A name that matches nowhere returns `None`, which is different from a weak
//! match: an empty query matches everything with a neutral 0.
//!
//! The arithmetic is the same as `Neb.Shell.Launcher.Fuzzy` in the C# build, down to the tie-breaks,
//! so the two launchers order the same Start Menu the same way. Text is compared lower-cased with
//! Unicode rules rather than ASCII ones, which is what makes `ёлка` match `Ёлка` and `ПАИНТ` match
//! `Paint`.

/// The score of a candidate that *is* the query.
pub const EXACT: u32 = 3000;

/// The score of a candidate that starts with the query, before the leftover penalty.
pub const PREFIX_BASE: u32 = 2000;

/// The score of a match that starts a word but not the candidate.
pub const WORD_START: u32 = 1400;

/// The score of a substring match at the beginning of the candidate.
pub const SUBSTRING_BASE: u32 = 1200;

/// The score of a subsequence with no gaps.
pub const SUBSEQUENCE_BASE: u32 = 600;

/// The lowest score a subsequence can have and still be a match.
pub const SUBSEQUENCE_FLOOR: u32 = 100;

/// A nudge for an entry that launches an executable rather than opening a document.
///
/// The launcher is an application launcher, so `7-Zip File Manager` should answer "7-zip" before
/// `7-Zip Help`, which opens a `.chm`. The nudge is deliberately smaller than a band: it can never
/// lift a word-start match over a prefix, only reorder neighbours inside one class of match.
pub const APPLICATION_BONUS: u32 = 10;

/// Characters that begin a word; the C# launcher uses the same list.
const WORD_SEPARATORS: [char; 10] = [' ', '-', '_', '.', ',', '(', ')', '[', ']', ':'];

/// How well `candidate` answers `query`: higher is better, `None` is no match.
///
/// Both sides are trimmed and lower-cased; an empty query scores 0 for every non-empty candidate.
pub fn score(query: &str, candidate: &str) -> Option<u32> {
    score_lowered(
        &query.trim().to_lowercase(),
        &candidate.trim().to_lowercase(),
    )
}

/// [`score`] for text that has already been trimmed and lower-cased.
///
/// This is what the launcher uses per keystroke: the entries' names are lowered once when the index
/// arrives, and only the query is lowered again.
pub fn score_lowered(needle: &str, haystack: &str) -> Option<u32> {
    if haystack.is_empty() {
        return None;
    }

    if needle.is_empty() {
        return Some(0);
    }

    let needle: Vec<char> = needle.chars().collect();
    let haystack: Vec<char> = haystack.chars().collect();

    if haystack == needle {
        return Some(EXACT);
    }

    if starts_with(&haystack, &needle) {
        // Every prefix outranks every other band; a shorter leftover keeps "Paint" above "Paint 3D".
        let leftover = (haystack.len() - needle.len()) as u32;
        return Some(PREFIX_BASE - leftover.min(PREFIX_BASE - WORD_START - 1));
    }

    if word_start(&haystack, &needle).is_some() {
        return Some(WORD_START);
    }

    if let Some(index) = find(&haystack, &needle) {
        let index = index as u32;
        return Some(SUBSTRING_BASE - index.min(SUBSTRING_BASE - SUBSEQUENCE_BASE - 1));
    }

    subsequence(&haystack, &needle)
}

/// The best `limit` entries for an already lower-cased query, best first and alphabetical inside a
/// tie. An empty query lists everything alphabetically, which is what the launcher shows before the
/// first keystroke.
///
/// `names` are the entries' names, trimmed and lower-cased, so a comparison here never allocates.
/// `applications` marks the entries whose target is an executable; those get
/// [`APPLICATION_BONUS`], which is what keeps "7-Zip File Manager" above "7-Zip Help" for "7-zip".
pub fn rank(needle: &str, names: &[String], applications: &[bool], limit: usize) -> Vec<usize> {
    let mut scored: Vec<(u32, usize)> = names
        .iter()
        .enumerate()
        .filter_map(|(index, name)| {
            let score = score_lowered(needle, name)?;
            let bonus =
                u32::from(applications.get(index).copied().unwrap_or(false)) * APPLICATION_BONUS;

            Some((score + bonus, index))
        })
        .collect();

    scored.sort_unstable_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| names[left.1].cmp(&names[right.1]))
    });
    scored.truncate(limit);

    scored.into_iter().map(|(_, index)| index).collect()
}

/// Whether `haystack` begins with `needle`.
fn starts_with(haystack: &[char], needle: &[char]) -> bool {
    haystack.len() >= needle.len() && haystack[..needle.len()] == *needle
}

/// The first occurrence of `needle` in `haystack`.
fn find(haystack: &[char], needle: &[char]) -> Option<usize> {
    if needle.len() > haystack.len() {
        return None;
    }

    (0..=haystack.len() - needle.len())
        .find(|start| haystack[*start..*start + needle.len()] == *needle)
}

/// The index of a match that starts a word, or `None`; the start of the candidate is a prefix, not
/// this band.
fn word_start(haystack: &[char], needle: &[char]) -> Option<usize> {
    let mut from = 0;

    while let Some(offset) = find(&haystack[from..], needle) {
        let index = from + offset;
        if index > 0 && WORD_SEPARATORS.contains(&haystack[index - 1]) {
            return Some(index);
        }

        from = index + 1;
        if from >= haystack.len() {
            break;
        }
    }

    None
}

/// Matches the letters in order, paying for the gaps between them; `None` when they are not all
/// there.
fn subsequence(haystack: &[char], needle: &[char]) -> Option<u32> {
    let mut position = 0;
    let mut gaps: u32 = 0;
    let mut last: Option<usize> = None;

    for wanted in needle {
        let found = haystack[position..]
            .iter()
            .position(|unit| unit == wanted)?
            + position;

        if let Some(previous) = last {
            gaps += (found - previous - 1) as u32;
        }

        last = Some(found);
        position = found + 1;
    }

    Some(
        SUBSEQUENCE_BASE
            .saturating_sub(gaps * 20)
            .max(SUBSEQUENCE_FLOOR),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exact_name_is_the_best_match_there_is() {
        assert_eq!(score("paint", "Paint"), Some(EXACT));
        assert_eq!(score("  PAINT  ", "  Paint "), Some(EXACT));
        assert_eq!(EXACT, 3000);
    }

    #[test]
    fn an_empty_query_matches_everything_neutrally() {
        assert_eq!(score("", "Paint"), Some(0));
        assert_eq!(score("   ", "Paint"), Some(0));
        assert_eq!(score("", ""), None, "a nameless entry is never a result");
        assert_eq!(score("paint", ""), None);
    }

    #[test]
    fn a_prefix_beat_a_word_start_which_beats_a_substring() {
        assert_eq!(score("pai", "Paint"), Some(1998));
        assert_eq!(score("pai", "Paint 3D"), Some(1995));
        assert_eq!(score("zip", "7-Zip File Manager"), Some(WORD_START));
        assert!(score("int", "Paint") == Some(SUBSTRING_BASE - 2));
        assert!(score("pai", "Paint") > score("zip", "7-Zip File Manager"));
        assert!(score("zip", "7-Zip File Manager") > score("int", "Paint"));
    }

    #[test]
    fn a_prefix_never_falls_below_the_word_start_band() {
        let long = format!("Paint{}", "x".repeat(5_000));

        assert!(
            score("paint", &long) >= Some(WORD_START + 1),
            "{:?}",
            score("paint", &long)
        );
    }

    #[test]
    fn a_substring_falls_with_its_position_but_stays_in_its_band() {
        let near = score("int", "Painter").expect("substring");
        let far = score("int", &format!("{}int", "x".repeat(1_000))).expect("substring");

        assert!(
            (SUBSEQUENCE_BASE + 1..=SUBSTRING_BASE).contains(&near),
            "{near}"
        );
        assert_eq!(far, SUBSEQUENCE_BASE + 1);
        assert!(near > far);
    }

    #[test]
    fn a_subsequence_matches_and_pays_for_its_gaps() {
        let compact = score("npt", "npt").expect("prefix wins here");
        let gapped = score("npd", "notepad").expect("n, p and d are all there, in order");
        let weaker = score("nd", "notepad").expect("a bigger gap costs more");

        assert_eq!(compact, EXACT);
        assert!(
            (SUBSEQUENCE_FLOOR..=SUBSEQUENCE_BASE).contains(&gapped),
            "{gapped}"
        );
        assert!(gapped > weaker, "{gapped} must beat {weaker}");
        assert_eq!(score("xyz", "notepad"), None);
        assert_eq!(score("npt", "notepad"), None, "t comes before p");
    }

    #[test]
    fn matching_ignores_case_in_both_alphabets() {
        assert_eq!(score("ёлка", "Ёлка"), Some(EXACT));
        assert_eq!(score("ЁЛКА", "ёлка"), Some(EXACT));
        assert_eq!(score("паинт", "Paint"), None);
        assert_eq!(score("бло", "Блокнот"), Some(1996));
    }

    #[test]
    fn the_band_constants_are_the_ones_the_brief_names() {
        assert_eq!((EXACT, PREFIX_BASE, WORD_START), (3000, 2000, 1400));
        assert_eq!(
            (SUBSTRING_BASE, SUBSEQUENCE_BASE, SUBSEQUENCE_FLOOR),
            (1200, 600, 100)
        );
    }

    #[test]
    fn rank_orders_by_band_then_alphabetically() {
        let names: Vec<String> = [
            "Paint 3D",
            "Paint",
            "paint.net",
            "Notepad",
            "7-Zip File Manager",
        ]
        .iter()
        .map(|name| name.to_lowercase())
        .collect();

        let ranked = rank("paint", &names, &[], 50);

        assert_eq!(ranked.first(), Some(&1), "exact 'Paint' first");
        assert_eq!(ranked[1], 0, "then the shorter prefix 'Paint 3D'");
        assert_eq!(ranked[2], 2, "then 'paint.net'");
        assert_eq!(ranked.len(), 3, "Notepad and 7-Zip do not contain 'paint'");
    }

    #[test]
    fn an_empty_rank_is_alphabetical_and_limited() {
        let names: Vec<String> = ["zebra", "alpha", "Mike"]
            .iter()
            .map(|name| name.to_lowercase())
            .collect();

        assert_eq!(rank("", &names, &[], 2), vec![1, 2]);
        assert!(rank("", &names, &[], 10).len() == 3);
        assert!(rank("nothing", &names, &[], 10).is_empty());
    }

    #[test]
    fn an_executable_outranks_a_document_inside_one_band() {
        let names: Vec<String> = ["7-Zip Help", "7-Zip File Manager"]
            .iter()
            .map(|name| name.to_lowercase())
            .collect();
        let applications = [false, true];

        let ranked = rank("7-zip", &names, &applications, 10);

        assert_eq!(
            ranked.first(),
            Some(&1),
            "the file manager is the application '7-zip' means"
        );
        assert_eq!(
            rank("7-zip", &names, &[false, false], 10).first(),
            Some(&0),
            "without the nudge the shorter name wins its prefix band"
        );
    }
}
