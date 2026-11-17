//! Named character references: XML's five and HTML 4's 253.
//!
//! EPUB content documents are XHTML, and books written by hand or by older
//! tools use HTML's named entities (`&mdash;`, `&hellip;`, `&eacute;`) without
//! declaring them, in content, in navigation documents and now and then in an
//! NCX or a package document. An XML parser that knows only `&lt;` and its
//! siblings would refuse all of those files. This is the fixed table XHTML
//! 1.0 names — Latin-1 (96), Symbols (124) and Special (32, which include
//! `quot`, `amp`, `lt` and `gt`), plus XML's `apos` — 253 names, each mapped
//! to the one character HTML 4 defines for it.
//!
//! It is a sorted static slice searched by binary search: no allocation, no
//! hashing, no build script and no crate. **Nothing else is ever expanded.**
//! An entity a document declares in its own DTD is not in this table and
//! stays literal, so an entity bomb costs its own length.

/// `(name, character)`, sorted by name in byte order (uppercase first), so
/// that [`lookup`] can binary-search it.
pub(crate) const ENTITIES: &[(&str, char)] = &[
    ("AElig", '\u{C6}'),
    ("Aacute", '\u{C1}'),
    ("Acirc", '\u{C2}'),
    ("Agrave", '\u{C0}'),
    ("Alpha", '\u{391}'),
    ("Aring", '\u{C5}'),
    ("Atilde", '\u{C3}'),
    ("Auml", '\u{C4}'),
    ("Beta", '\u{392}'),
    ("Ccedil", '\u{C7}'),
    ("Chi", '\u{3A7}'),
    ("Dagger", '\u{2021}'),
    ("Delta", '\u{394}'),
    ("ETH", '\u{D0}'),
    ("Eacute", '\u{C9}'),
    ("Ecirc", '\u{CA}'),
    ("Egrave", '\u{C8}'),
    ("Epsilon", '\u{395}'),
    ("Eta", '\u{397}'),
    ("Euml", '\u{CB}'),
    ("Gamma", '\u{393}'),
    ("Iacute", '\u{CD}'),
    ("Icirc", '\u{CE}'),
    ("Igrave", '\u{CC}'),
    ("Iota", '\u{399}'),
    ("Iuml", '\u{CF}'),
    ("Kappa", '\u{39A}'),
    ("Lambda", '\u{39B}'),
    ("Mu", '\u{39C}'),
    ("Ntilde", '\u{D1}'),
    ("Nu", '\u{39D}'),
    ("OElig", '\u{152}'),
    ("Oacute", '\u{D3}'),
    ("Ocirc", '\u{D4}'),
    ("Ograve", '\u{D2}'),
    ("Omega", '\u{3A9}'),
    ("Omicron", '\u{39F}'),
    ("Oslash", '\u{D8}'),
    ("Otilde", '\u{D5}'),
    ("Ouml", '\u{D6}'),
    ("Phi", '\u{3A6}'),
    ("Pi", '\u{3A0}'),
    ("Prime", '\u{2033}'),
    ("Psi", '\u{3A8}'),
    ("Rho", '\u{3A1}'),
    ("Scaron", '\u{160}'),
    ("Sigma", '\u{3A3}'),
    ("THORN", '\u{DE}'),
    ("Tau", '\u{3A4}'),
    ("Theta", '\u{398}'),
    ("Uacute", '\u{DA}'),
    ("Ucirc", '\u{DB}'),
    ("Ugrave", '\u{D9}'),
    ("Upsilon", '\u{3A5}'),
    ("Uuml", '\u{DC}'),
    ("Xi", '\u{39E}'),
    ("Yacute", '\u{DD}'),
    ("Yuml", '\u{178}'),
    ("Zeta", '\u{396}'),
    ("aacute", '\u{E1}'),
    ("acirc", '\u{E2}'),
    ("acute", '\u{B4}'),
    ("aelig", '\u{E6}'),
    ("agrave", '\u{E0}'),
    ("alefsym", '\u{2135}'),
    ("alpha", '\u{3B1}'),
    ("amp", '\u{26}'),
    ("and", '\u{2227}'),
    ("ang", '\u{2220}'),
    ("apos", '\u{27}'),
    ("aring", '\u{E5}'),
    ("asymp", '\u{2248}'),
    ("atilde", '\u{E3}'),
    ("auml", '\u{E4}'),
    ("bdquo", '\u{201E}'),
    ("beta", '\u{3B2}'),
    ("brvbar", '\u{A6}'),
    ("bull", '\u{2022}'),
    ("cap", '\u{2229}'),
    ("ccedil", '\u{E7}'),
    ("cedil", '\u{B8}'),
    ("cent", '\u{A2}'),
    ("chi", '\u{3C7}'),
    ("circ", '\u{2C6}'),
    ("clubs", '\u{2663}'),
    ("cong", '\u{2245}'),
    ("copy", '\u{A9}'),
    ("crarr", '\u{21B5}'),
    ("cup", '\u{222A}'),
    ("curren", '\u{A4}'),
    ("dArr", '\u{21D3}'),
    ("dagger", '\u{2020}'),
    ("darr", '\u{2193}'),
    ("deg", '\u{B0}'),
    ("delta", '\u{3B4}'),
    ("diams", '\u{2666}'),
    ("divide", '\u{F7}'),
    ("eacute", '\u{E9}'),
    ("ecirc", '\u{EA}'),
    ("egrave", '\u{E8}'),
    ("empty", '\u{2205}'),
    ("emsp", '\u{2003}'),
    ("ensp", '\u{2002}'),
    ("epsilon", '\u{3B5}'),
    ("equiv", '\u{2261}'),
    ("eta", '\u{3B7}'),
    ("eth", '\u{F0}'),
    ("euml", '\u{EB}'),
    ("euro", '\u{20AC}'),
    ("exist", '\u{2203}'),
    ("fnof", '\u{192}'),
    ("forall", '\u{2200}'),
    ("frac12", '\u{BD}'),
    ("frac14", '\u{BC}'),
    ("frac34", '\u{BE}'),
    ("frasl", '\u{2044}'),
    ("gamma", '\u{3B3}'),
    ("ge", '\u{2265}'),
    ("gt", '\u{3E}'),
    ("hArr", '\u{21D4}'),
    ("harr", '\u{2194}'),
    ("hearts", '\u{2665}'),
    ("hellip", '\u{2026}'),
    ("iacute", '\u{ED}'),
    ("icirc", '\u{EE}'),
    ("iexcl", '\u{A1}'),
    ("igrave", '\u{EC}'),
    ("image", '\u{2111}'),
    ("infin", '\u{221E}'),
    ("int", '\u{222B}'),
    ("iota", '\u{3B9}'),
    ("iquest", '\u{BF}'),
    ("isin", '\u{2208}'),
    ("iuml", '\u{EF}'),
    ("kappa", '\u{3BA}'),
    ("lArr", '\u{21D0}'),
    ("lambda", '\u{3BB}'),
    ("lang", '\u{2329}'),
    ("laquo", '\u{AB}'),
    ("larr", '\u{2190}'),
    ("lceil", '\u{2308}'),
    ("ldquo", '\u{201C}'),
    ("le", '\u{2264}'),
    ("lfloor", '\u{230A}'),
    ("lowast", '\u{2217}'),
    ("loz", '\u{25CA}'),
    ("lrm", '\u{200E}'),
    ("lsaquo", '\u{2039}'),
    ("lsquo", '\u{2018}'),
    ("lt", '\u{3C}'),
    ("macr", '\u{AF}'),
    ("mdash", '\u{2014}'),
    ("micro", '\u{B5}'),
    ("middot", '\u{B7}'),
    ("minus", '\u{2212}'),
    ("mu", '\u{3BC}'),
    ("nabla", '\u{2207}'),
    ("nbsp", '\u{A0}'),
    ("ndash", '\u{2013}'),
    ("ne", '\u{2260}'),
    ("ni", '\u{220B}'),
    ("not", '\u{AC}'),
    ("notin", '\u{2209}'),
    ("nsub", '\u{2284}'),
    ("ntilde", '\u{F1}'),
    ("nu", '\u{3BD}'),
    ("oacute", '\u{F3}'),
    ("ocirc", '\u{F4}'),
    ("oelig", '\u{153}'),
    ("ograve", '\u{F2}'),
    ("oline", '\u{203E}'),
    ("omega", '\u{3C9}'),
    ("omicron", '\u{3BF}'),
    ("oplus", '\u{2295}'),
    ("or", '\u{2228}'),
    ("ordf", '\u{AA}'),
    ("ordm", '\u{BA}'),
    ("oslash", '\u{F8}'),
    ("otilde", '\u{F5}'),
    ("otimes", '\u{2297}'),
    ("ouml", '\u{F6}'),
    ("para", '\u{B6}'),
    ("part", '\u{2202}'),
    ("permil", '\u{2030}'),
    ("perp", '\u{22A5}'),
    ("phi", '\u{3C6}'),
    ("pi", '\u{3C0}'),
    ("piv", '\u{3D6}'),
    ("plusmn", '\u{B1}'),
    ("pound", '\u{A3}'),
    ("prime", '\u{2032}'),
    ("prod", '\u{220F}'),
    ("prop", '\u{221D}'),
    ("psi", '\u{3C8}'),
    ("quot", '\u{22}'),
    ("rArr", '\u{21D2}'),
    ("radic", '\u{221A}'),
    ("rang", '\u{232A}'),
    ("raquo", '\u{BB}'),
    ("rarr", '\u{2192}'),
    ("rceil", '\u{2309}'),
    ("rdquo", '\u{201D}'),
    ("real", '\u{211C}'),
    ("reg", '\u{AE}'),
    ("rfloor", '\u{230B}'),
    ("rho", '\u{3C1}'),
    ("rlm", '\u{200F}'),
    ("rsaquo", '\u{203A}'),
    ("rsquo", '\u{2019}'),
    ("sbquo", '\u{201A}'),
    ("scaron", '\u{161}'),
    ("sdot", '\u{22C5}'),
    ("sect", '\u{A7}'),
    ("shy", '\u{AD}'),
    ("sigma", '\u{3C3}'),
    ("sigmaf", '\u{3C2}'),
    ("sim", '\u{223C}'),
    ("spades", '\u{2660}'),
    ("sub", '\u{2282}'),
    ("sube", '\u{2286}'),
    ("sum", '\u{2211}'),
    ("sup", '\u{2283}'),
    ("sup1", '\u{B9}'),
    ("sup2", '\u{B2}'),
    ("sup3", '\u{B3}'),
    ("supe", '\u{2287}'),
    ("szlig", '\u{DF}'),
    ("tau", '\u{3C4}'),
    ("there4", '\u{2234}'),
    ("theta", '\u{3B8}'),
    ("thetasym", '\u{3D1}'),
    ("thinsp", '\u{2009}'),
    ("thorn", '\u{FE}'),
    ("tilde", '\u{2DC}'),
    ("times", '\u{D7}'),
    ("trade", '\u{2122}'),
    ("uArr", '\u{21D1}'),
    ("uacute", '\u{FA}'),
    ("uarr", '\u{2191}'),
    ("ucirc", '\u{FB}'),
    ("ugrave", '\u{F9}'),
    ("uml", '\u{A8}'),
    ("upsih", '\u{3D2}'),
    ("upsilon", '\u{3C5}'),
    ("uuml", '\u{FC}'),
    ("weierp", '\u{2118}'),
    ("xi", '\u{3BE}'),
    ("yacute", '\u{FD}'),
    ("yen", '\u{A5}'),
    ("yuml", '\u{FF}'),
    ("zeta", '\u{3B6}'),
    ("zwj", '\u{200D}'),
    ("zwnj", '\u{200C}'),
];

/// The character a named reference stands for: one of XML's five predefined
/// entities or one of HTML 4's. Names are case-sensitive (`&Eacute;` is not
/// `&eacute;`).
pub(crate) fn lookup(name: &str) -> Option<char> {
    ENTITIES
        .binary_search_by(|(n, _)| (*n).cmp(name))
        .ok()
        .and_then(|i| ENTITIES.get(i))
        .map(|(_, c)| *c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_sorted_without_duplicates_and_complete() {
        assert_eq!(ENTITIES.len(), 253);
        for pair in ENTITIES.windows(2) {
            if let [(a, _), (b, _)] = pair {
                assert!(a < b, "{a} must sort before {b}");
            }
        }
    }

    #[test]
    fn every_entry_is_found_by_its_own_name() {
        for (name, c) in ENTITIES {
            assert_eq!(lookup(name), Some(*c), "{name}");
        }
    }

    /// The Latin-1 block is HTML 4's first list: one name for each of
    /// U+00A0..=U+00FF, no gaps.
    #[test]
    fn latin1_is_covered_exactly_once() {
        let mut seen: Vec<u32> = ENTITIES
            .iter()
            .map(|(_, c)| *c as u32)
            .filter(|c| (0xA0..=0xFF).contains(c))
            .collect();
        seen.sort_unstable();
        assert_eq!(seen, (0xA0..=0xFF).collect::<Vec<u32>>());
    }

    #[test]
    fn well_known_names() {
        for (name, c) in [
            ("nbsp", '\u{A0}'),
            ("mdash", '—'),
            ("hellip", '…'),
            ("euro", '€'),
            ("apos", '\''),
            ("amp", '&'),
            ("Eacute", 'É'),
            ("eacute", 'é'),
            ("rsquo", '’'),
            ("lang", '\u{2329}'),
        ] {
            assert_eq!(lookup(name), Some(c), "{name}");
        }
        for unknown in ["", "NBSP", "mdash;", "bogus", "lol9", "nbs"] {
            assert_eq!(lookup(unknown), None, "{unknown}");
        }
    }
}
