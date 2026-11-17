//! Parses human names: the name to file under, display order, and the author sort key.

const PARTICLES: [&str; 18] = [
    "de", "del", "della", "der", "den", "di", "da", "du", "la", "le", "van", "von", "ten", "ter",
    "af", "av", "bin", "ibn",
];

const SUFFIXES: [&str; 8] = ["jr", "jr.", "sr", "sr.", "ii", "iii", "iv", "phd"];

fn is_suffix(word: &str) -> bool {
    SUFFIXES.contains(&word.trim_matches(',').to_lowercase().as_str())
}

fn comma_is_only_a_suffix(after: &str) -> bool {
    after.split_whitespace().all(is_suffix)
}

/// The name to file an author under: `Surname, Given` is trusted, suffixes dropped, particles kept.
pub fn last_name(author: &str) -> String {
    let author = author.trim();
    if let Some((head, tail)) = author.split_once(',') {
        let head = head.trim();
        if !head.is_empty() && !comma_is_only_a_suffix(tail) {
            return head.to_string();
        }
    }
    let mut words: Vec<&str> = author
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|w| !w.is_empty())
        .collect();
    while words.len() > 1 && is_suffix(words[words.len() - 1]) {
        words.pop();
    }
    match words.len() {
        0 => String::new(),
        1 => words[0].to_string(),
        n => {
            let particle = PARTICLES.contains(&words[n - 2].to_lowercase().as_str());
            if particle {
                format!("{} {}", words[n - 2], words[n - 1])
            } else {
                words[n - 1].to_string()
            }
        }
    }
}

/// The name as a person says it (`Borges, Jorge Luis` -> `Jorge Luis Borges`); inverse of
/// `last_name`.
/// A name with no comma is returned untouched, never rearranged on a guess, and a suffix comma
/// (`Martin Luther King, Jr.`) is not an inversion.
pub fn display_order(author: &str) -> String {
    let author = author.trim();
    let Some((head, rest)) = author.split_once(',') else {
        return author.to_string();
    };
    let head = head.trim();
    if head.is_empty() {
        return author.to_string();
    }

    let mut segments: Vec<&str> = rest
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let mut suffixes: Vec<&str> = Vec::new();
    while segments.last().is_some_and(|s| comma_is_only_a_suffix(s)) {
        suffixes.push(segments.pop().expect("checked by is_some_and"));
    }
    suffixes.reverse();

    let given = segments.join(", ");
    if given.is_empty() {
        return author.to_string();
    }

    let mut out = format!("{given} {head}");
    for s in suffixes {
        out.push_str(", ");
        out.push_str(s);
    }
    out
}

/// Sort key `(no-author flag, last name, full name)`; authorless books sort last.
pub fn sort_key(authors: &[String]) -> (u8, String, String) {
    match authors.iter().find(|a| !a.trim().is_empty()) {
        Some(a) => (0, last_name(a).to_lowercase(), a.trim().to_lowercase()),
        None => (1, String::new(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_an_inverted_name_under_the_half_it_named() {
        assert_eq!(last_name("Borges, Jorge Luis"), "Borges");
        assert_eq!(last_name("Le Guin, Ursula K."), "Le Guin");
    }

    #[test]
    fn files_a_plain_name_under_its_last_word() {
        assert_eq!(last_name("Emily St. John Mandel"), "Mandel");
        assert_eq!(last_name("Richard Powers"), "Powers");
    }

    #[test]
    fn a_particle_travels_with_the_surname() {
        assert_eq!(last_name("Ursula K. Le Guin"), "Le Guin");
        assert_eq!(last_name("Simone de Beauvoir"), "de Beauvoir");
        assert_eq!(last_name("Vincent van Gogh"), "van Gogh");
    }

    /// Two words, not three: only there do `n - 2` and `n / 2` pick different words.
    #[test]
    fn a_two_word_name_keeps_a_leading_particle() {
        assert_eq!(last_name("van Beethoven"), "van Beethoven");
    }

    #[test]
    fn a_four_word_name_without_a_particle_files_under_the_last_word() {
        assert_eq!(last_name("Mary Ann Evans Lewes"), "Lewes");
    }

    #[test]
    fn suffixes_are_never_the_filing_name() {
        assert_eq!(last_name("Kurt Vonnegut Jr."), "Vonnegut");
        assert_eq!(last_name("Martin Luther King, Jr."), "King");
    }

    #[test]
    fn a_mononym_is_its_own_surname() {
        assert_eq!(last_name("Colette"), "Colette");
        assert_eq!(last_name("Homer"), "Homer");
        assert_eq!(last_name(""), "");
        assert_eq!(last_name("   "), "");
    }

    #[test]
    fn a_last_name_survives_particles_initials_and_suffixes() {
        assert_eq!(last_name("George Eliot"), "Eliot");
        assert_eq!(last_name("Mandel, Emily St. John"), "Mandel");
        assert_eq!(last_name("King, Martin Luther Jr."), "King");
        assert_eq!(last_name("George Eliot,"), "Eliot");
    }

    #[test]
    fn stripping_suffixes_never_empties_a_name() {
        assert_eq!(last_name("Jr."), "Jr.");
        assert_eq!(last_name("III"), "III");
    }

    #[test]
    fn display_order_flips_only_a_real_inversion() {
        assert_eq!(display_order("Borges, Jorge Luis"), "Jorge Luis Borges");
        assert_eq!(display_order("Le Guin, Ursula K."), "Ursula K. Le Guin");
        assert_eq!(
            display_order("Emily St. John Mandel"),
            "Emily St. John Mandel"
        );
        assert_eq!(display_order("Colette"), "Colette");
        assert_eq!(display_order(""), "");
    }

    #[test]
    fn display_order_keeps_a_suffix_where_it_belongs() {
        assert_eq!(
            display_order("Martin Luther King, Jr."),
            "Martin Luther King, Jr."
        );
        assert_eq!(
            display_order("King, Martin Luther, Jr."),
            "Martin Luther King, Jr."
        );
    }

    #[test]
    fn an_authorless_book_sorts_last_rather_than_under_nothing() {
        let none: Vec<String> = Vec::new();
        assert_eq!(sort_key(&none).0, 1);
        assert_eq!(sort_key(&["   ".to_string()]).0, 1);
        assert_eq!(sort_key(&["Colette".to_string()]).0, 0);
    }

    #[test]
    fn sort_key_files_under_the_first_named_author() {
        let k = sort_key(&["Le Guin, Ursula K.".to_string(), "Someone Else".to_string()]);
        assert_eq!(k.1, "le guin");
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn arbitrary_input_never_panics(s in ".{0,80}") {
            let _ = last_name(&s);
            let _ = display_order(&s);
            let _ = sort_key(&[s]);
        }

        #[test]
        fn display_order_never_rearranges_an_uninverted_name(s in "[A-Za-z. ]{0,40}") {
            prop_assert_eq!(display_order(&s), s.trim().to_string());
        }

        #[test]
        fn filing_survives_the_flip(
            given in "[A-Za-z][a-z]{1,8}",
            surname in "[A-Za-z][a-z]{1,8}",
        ) {
            // A given name that is a particle, or a surname that is a suffix, is ambiguous without the comma.
            prop_assume!(!PARTICLES.contains(&given.to_lowercase().as_str()));
            prop_assume!(!is_suffix(&given) && !is_suffix(&surname));
            let inverted = format!("{surname}, {given}");
            prop_assert_eq!(last_name(&inverted), surname.clone());
            prop_assert_eq!(last_name(&display_order(&inverted)), surname);
        }
    }
}
