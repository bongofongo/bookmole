//! `{quote}`, `{title}` and `{author}` placeholders are expanded by [`note_for`].

pub const KO_NOTES_EN: &[&str] = &[
    "Come back to this.",
    "This is the turn — everything before was setup.",
    "Why \"{quote}\"? The word choice is doing more than it seems.",
    "Compare with the opening chapter; the same image, reversed.",
    "See [[{title}]] — this is the passage the whole book hangs on.",
    "Does the narrator believe this, or is it irony?",
    "Underlined twice on paper too.",
    "Reminds me of something in [[{author}]]'s letters.",
    "Possible epigraph.",
    "I disagree, but it is beautifully put.",
    "Look up the historical context here.",
    "\"{quote}\" — say it out loud; the rhythm is the point.",
    "First time the theme is named outright.",
    "Funny. Genuinely funny.",
    "Is this foreshadowing? Check after finishing.",
    "Quote for the review.",
];

pub const KO_NOTES_FR: &[&str] = &[
    "À relire.",
    "« {quote} » — tout le livre est là.",
    "Comparer avec le premier chapitre.",
    "Voir [[{title}]] pour le contexte.",
    "Ironie, ou le narrateur y croit-il vraiment ?",
    "Belle phrase, mais je ne suis pas d'accord.",
    "Chercher le sens exact de ce mot.",
    "Le thème apparaît ici pour la première fois.",
];

pub const KO_NOTES_DE: &[&str] = &[
    "Nochmal lesen.",
    "„{quote}“ — hier kippt die Geschichte.",
    "Vergleiche mit dem Anfang.",
    "Siehe [[{title}]].",
    "Meint der Erzähler das ernst?",
    "Schöner Satz, aber ich sehe es anders.",
    "Wortbedeutung nachschlagen.",
    "Erinnert an [[{author}]]s Briefe.",
];

pub const KO_NOTES_ES: &[&str] = &[
    "Volver a esto.",
    "«{quote}» — aquí cambia todo.",
    "¿Ironía, o lo dice en serio?",
    "Ver [[{title}]].",
    "Comparar con el primer capítulo.",
    "Muy bien dicho, aunque no estoy de acuerdo.",
    "Buscar el contexto histórico.",
];

pub const KO_NOTES_IT: &[&str] = &[
    "Da rileggere.",
    "«{quote}» — qui la storia gira.",
    "Ironia, o ci crede davvero?",
    "Vedi [[{title}]].",
    "Confrontare con l'inizio.",
    "Bella frase, ma non sono d'accordo.",
];

pub const KO_NOTES_RU: &[&str] = &[
    "Перечитать.",
    "«{quote}» — вот главное.",
    "Сравнить с началом.",
    "См. [[{title}]].",
];

pub const KO_NOTES_EL: &[&str] = &[
    "Να το ξαναδιαβάσω.",
    "«{quote}» — εδώ αλλάζουν όλα.",
    "Βλ. [[{title}]].",
];

pub fn ko_notes(language: &str) -> &'static [&'static str] {
    match language {
        "fr" => KO_NOTES_FR,
        "de" => KO_NOTES_DE,
        "es" => KO_NOTES_ES,
        "it" => KO_NOTES_IT,
        "ru" => KO_NOTES_RU,
        "el" => KO_NOTES_EL,
        _ => KO_NOTES_EN,
    }
}

pub fn note_for(template: &str, highlight: &str, title: &str, author: &str) -> String {
    let quote: String = highlight
        .split_whitespace()
        .take(4)
        .collect::<Vec<_>>()
        .join(" ");
    // A space-less script yields one "word"; clip by characters.
    let quote = if quote.chars().count() > 40 {
        quote.chars().take(12).collect()
    } else {
        quote
    };
    template
        .replace("{quote}", quote.trim_end_matches([',', ';', ':']))
        .replace("{title}", title)
        .replace("{author}", author)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bank_is_non_empty_and_has_no_stray_placeholders() {
        for lang in ["en", "fr", "de", "es", "it", "ru", "el", "xx"] {
            let bank = ko_notes(lang);
            assert!(!bank.is_empty(), "{lang}");
            for t in bank {
                let out = note_for(t, "one two three four five", "T", "A");
                assert!(!out.contains('{'), "{lang}: {out}");
            }
        }
    }
}
