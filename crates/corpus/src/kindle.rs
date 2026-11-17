//! Tier-1 Kindle fixtures: the e-reader's `My Clippings.txt` and the Kindle
//! app's Export Notebook HTML.
//!
//! Written the way the devices write them, from cases declared here, with an
//! `expected.json` beside each set that this generator computes from its own
//! declaration — never from the engine's parser, which is what the expected
//! file checks. No randomness: every case is a shape, stated once.
//!
//! The prose is invented; no fixture quotes a real book.

use std::path::Path;

use serde_json::{Value, json};

/// What a declared clipping is for, which is what the expectations count.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Highlight,
    /// The same entry written twice; the importer keeps one.
    DuplicateHighlight,
    /// A note whose highlight is the one with this text.
    NoteOn(&'static str),
    /// A note no highlight contains.
    LoneNote,
    Bookmark,
    /// No language row reads it, or it is broken.
    Unparsed,
}

struct Clip {
    title: &'static str,
    /// Some firmware writes a BOM at the start of each title line.
    title_bom: bool,
    /// `None` is a broken entry: a title and nothing else.
    meta: Option<&'static str>,
    text: &'static str,
    role: Role,
}

const KINDRED: &str = "Kindred (Butler, Octavia E.)";
const GEB: &str = "Gödel, Escher, Bach: an Eternal Golden Braid (20th Anniversary Edition) (Hofstadter, Douglas R.)";
const MIDDLEMARCH: &str = "Middlemarch (Eliot, George)";
const UNOWNED: &str = "An Unowned Book (Nobody, A.)";

fn clip(title: &'static str, meta: &'static str, text: &'static str, role: Role) -> Clip {
    Clip {
        title,
        title_bom: false,
        meta: Some(meta),
        text,
        role,
    }
}

fn base_clips() -> Vec<Clip> {
    use Role::*;
    vec![
        clip(
            KINDRED,
            "- Your Highlight on page 12 | Location 180-183 | Added on Monday, March 3, 2025 10:15:22 AM",
            "The house had kept its own counsel for a hundred years.",
            Highlight,
        ),
        clip(
            KINDRED,
            "- Your Note on page 12 | Location 181 | Added on Monday, March 3, 2025 10:16:40 AM",
            "first thought",
            NoteOn("The house had kept its own counsel for a hundred years."),
        ),
        clip(
            KINDRED,
            "- Your Highlight on page 13 | Location 200-205 | Added on Monday, March 3, 2025 10:20:00 AM",
            "A first line of the passage.\nAnd its second line.",
            Highlight,
        ),
        clip(
            KINDRED,
            "- Your Bookmark on page 14 | Location 210 | Added on Monday, March 3, 2025 10:21:00 AM",
            "",
            Bookmark,
        ),
        clip(
            KINDRED,
            "- Your Highlight on Location 300 | Added on Monday, March 3, 2025 10:22:00 AM",
            "",
            Highlight,
        ),
        // The same note, edited on the device: a new entry at the same place.
        clip(
            KINDRED,
            "- Your Note on page 12 | Location 181 | Added on Tuesday, March 4, 2025 9:02:11 PM",
            "first thought, revised",
            NoteOn("The house had kept its own counsel for a hundred years."),
        ),
        clip(
            KINDRED,
            "- Your Note on Location 900 | Added on Tuesday, March 4, 2025 9:05:00 PM",
            "a note with no highlight",
            LoneNote,
        ),
        // A device set to French. No row reads it: reported, never guessed.
        clip(
            KINDRED,
            "- Votre surlignement sur la page 15 | emplacement 220-222 | Ajouté le mardi 4 mars 2025 21:10:00",
            "Une phrase que personne ne lira ici.",
            Unparsed,
        ),
        Clip {
            title: GEB,
            title_bom: true,
            meta: Some(
                "- Your Highlight on Location 1024-1030 | Added on Monday, 3 March 2025 22:15:22",
            ),
            text: "A loop that comes back to where it began is not a circle.",
            role: Highlight,
        },
        clip(
            GEB,
            "- Your Highlight on page 88 | Location 1500-1510 | Added on Monday, 3 March 2025 22:30:00",
            "<You have reached the clipping limit for this item>",
            Highlight,
        ),
        clip(
            GEB,
            "- Your Note on page 88 | Location 1505 | Added on Monday, 3 March 2025 22:31:00",
            "limit, of course",
            NoteOn("<You have reached the clipping limit for this item>"),
        ),
        // Kindle 3 firmware: no "Your", "Loc.", an abbreviated range.
        clip(
            MIDDLEMARCH,
            "- Highlight Loc. 466-69  | Added on Friday, March 30, 2012, 03:49 PM",
            "An old device wrote this one down.",
            Highlight,
        ),
        clip(
            MIDDLEMARCH,
            "- Your Highlight on Page 7 | Added on Saturday, March 31, 2012, 12:05 AM",
            "Page only, the way a PDF is clipped.",
            Highlight,
        ),
        clip(
            MIDDLEMARCH,
            "- Highlight Loc. 466-69  | Added on Friday, March 30, 2012, 03:49 PM",
            "An old device wrote this one down.",
            DuplicateHighlight,
        ),
        clip(
            UNOWNED,
            "- Your Highlight on Location 10-12 | Added on Sunday, March 2, 2025 8:00:00 AM",
            "Nobody here owns this book.",
            Highlight,
        ),
        clip(
            UNOWNED,
            "- Your Highlight on Location 40-41 | Added on Sunday, March 2, 2025 8:05:00 AM",
            "So these wait to be attached.",
            Highlight,
        ),
        Clip {
            title: KINDRED,
            title_bom: false,
            meta: None,
            text: "",
            role: Unparsed,
        },
    ]
}

/// What the device appends later: an edit of a note already there and two
/// new highlights.
fn appended_clips() -> Vec<Clip> {
    use Role::*;
    vec![
        clip(
            KINDRED,
            "- Your Note on page 12 | Location 181 | Added on Wednesday, March 5, 2025 8:00:00 AM",
            "first thought, third draft",
            NoteOn("The house had kept its own counsel for a hundred years."),
        ),
        clip(
            MIDDLEMARCH,
            "- Your Highlight on page 9 | Location 700-702 | Added on Wednesday, March 5, 2025 8:10:00 AM",
            "Something new, read years later.",
            Highlight,
        ),
        clip(
            GEB,
            "- Your Highlight on Location 2000-2004 | Added on Wednesday, 5 March 2025 08:20:00",
            "And one more strange loop.",
            Highlight,
        ),
    ]
}

fn render_clips(clips: &[Clip], eol: &str, bom: bool) -> String {
    let mut out = String::new();
    if bom {
        out.push('\u{feff}');
    }
    for c in clips {
        if c.title_bom {
            out.push('\u{feff}');
        }
        out.push_str(c.title);
        out.push_str(eol);
        if let Some(meta) = c.meta {
            out.push_str(meta);
            out.push_str(eol);
            out.push_str(eol);
            out.push_str(&c.text.replace('\n', eol));
            out.push_str(eol);
        }
        out.push_str("==========");
        out.push_str(eol);
    }
    out
}

/// Per-title expectations, computed from the roles.
fn clippings_expected(clips: &[Clip]) -> Value {
    let mut titles: Vec<&str> = Vec::new();
    for c in clips {
        if c.role != Role::Unparsed && !titles.contains(&c.title) {
            titles.push(c.title);
        }
    }
    let books: Vec<Value> = titles
        .iter()
        .map(|t| {
            let of = |r: fn(&Role) -> bool| clips.iter().filter(|c| c.title == *t && r(&c.role)).count();
            let highlights = of(|r| *r == Role::Highlight);
            let lone = of(|r| *r == Role::LoneNote);
            let mut notes: Vec<(&str, &str)> = Vec::new();
            for c in clips.iter().filter(|c| c.title == *t) {
                if let Role::NoteOn(host) = c.role {
                    // The last note written on a highlight's one place is its note.
                    notes.retain(|(h, _)| *h != host);
                    notes.push((host, c.text));
                }
            }
            json!({
                "title_line": t,
                "highlights": highlights,
                "lone_notes": lone,
                "rows": highlights + lone,
                "bookmarks": of(|r| *r == Role::Bookmark),
                "notes": notes.iter().map(|(h, n)| json!({"highlight": h, "note": n})).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({
        "unparsed": clips.iter().filter(|c| c.role == Role::Unparsed).count(),
        "books": books,
    })
}

// ---- the Kindle app's notebook ----------------------------------------------------

const PREAMBLE: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" ?>\n\
<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Strict//EN\" \"XHTML1-s.dtd\" >\n\
<html xmlns=\"http://www.w3.org/TR/1999/REC-html-in-xml\" xml:lang=\"en\" lang=\"en\">\n\
<head>\n<meta http-equiv=\"Content-Type\" content=\"text/html; charset=UTF-8\" />\n\
<style type=\"text/css\">\n.bodyContainer { font-size: 1.1em; }\n\
/* .noteHeading never appears as <div class=\"noteHeading\"> here */\n</style>\n\
<title></title>\n</head>\n<body>\n<div class=\"bodyContainer\">\n\
<div class=\"notebookFor\">\nNotebook Export\n</div>\n";

fn header(title: &str, authors: &str) -> String {
    format!(
        "{PREAMBLE}<div class=\"bookTitle\">\n{title}\n</div>\n<div class=\"authors\">\n{authors}\n</div>\n\
         <div class=\"citation\">\n</div>\n<hr />\n"
    )
}

const FOOTER: &str = "</div>\n</body>\n</html>\n";

/// The main notebook: colours, no page, no location, entities, `<br/>`, a note
/// after a highlight, a lone note, a bookmark, a heading no row reads, and the
/// markup variations the export has been seen with.
fn remains_notebook() -> String {
    let mut s = header("The Remains of the Day", "Ishiguro, Kazuo");
    s.push_str("<div class=\"sectionHeading\">\nPrologue: July 1956\n</div>\n");
    s.push_str(
        "<div class=\"noteHeading\">\nHighlight(<span class=\"highlight_yellow\">yellow</span>) - Prologue: July 1956 &gt; Page 3 &#183; Location 75\n</div>\n\
         <div class=\"noteText\">\nA journey would do the house good, he said.\n</div>\n",
    );
    s.push_str(
        "<div class=\"noteHeading\">Note - Prologue: July 1956 &gt; Page 3 &#183; Location 75</div>\n\
         <div class=\"noteText\">pairs with the yellow one</div>\n",
    );
    s.push_str(
        "<div class=\"noteHeading\">Highlight (<span class=\"highlight_blue\">blue</span>) - Location 120</div>\n\
         <div class=\"noteText\">Don&#8217;t &amp; won&#x2019;t&nbsp;stop</div>\n",
    );
    s.push_str("<div class=\"sectionHeading\">Day One &#8226; Evening</div>\n");
    s.push_str(
        "<div class=\"noteHeading\">Highlight(<span class=\"highlight_pink\">pink</span>) - Page xii</div>\n\
         <div class=\"noteText\">First line<br/>\n      second   line<br />third line</div>\n",
    );
    s.push_str(
        "<div class=\"noteHeading\">Bookmark - Page 12 &#183; Location 140</div>\n\
         <div class=\"noteText\"></div>\n",
    );
    s.push_str(
        "<div class=\"noteHeading\">Note - Location 141</div>\n\
         <div class=\"noteText\">a note standing alone</div>\n",
    );
    s.push_str(
        "<div class=\"noteHeading\">Surlignement (<span class=\"highlight_yellow\">jaune</span>) - Emplacement 150</div>\n\
         <div class=\"noteText\">Une phrase que personne ne lira ici.</div>\n",
    );
    // Attributes before the class, an unquoted class, a single-quoted one, a
    // text closed with </h3> (seen in real exports), no colour at all.
    s.push_str(
        "<div id=\"h7\" CLASS=noteHeading>Highlight - Location 160</div>\n\
         <div data-n='7' class='noteText'>\n  No colour on this one.\n</h3>\n",
    );
    s.push_str(
        "<div class=\"noteHeading\">Highlight(<span class=\"highlight_orange\">orange</span>) - Day One &gt; Page 20 &#183; Location 170</div>\n\
         <div class=\"noteText\">The last &lt;highlight&gt; in the book.</div>\n",
    );
    s.push_str(FOOTER);
    s
}

fn remains_expected() -> Value {
    json!({
        "title": "The Remains of the Day",
        "authors": ["Ishiguro, Kazuo"],
        "highlights": 5,
        "lone_notes": 1,
        "rows": 6,
        "bookmarks": 1,
        "unparsed": 1,
        "colors": ["yellow", "blue", "pink", null, "orange"],
        "texts": [
            "A journey would do the house good, he said.",
            "Don\u{2019}t & won\u{2019}t stop",
            "First line\nsecond line\nthird line",
            "No colour on this one.",
            "The last <highlight> in the book."
        ],
        "chapters": ["Prologue: July 1956", "Prologue: July 1956", "Day One \u{2022} Evening", "Day One \u{2022} Evening", "Day One \u{2022} Evening"],
        "pages": [3, null, null, null, 20],
        "locations": ["75", "120", null, "160", "170"],
        "notes": [{"highlight": "A journey would do the house good, he said.", "note": "pairs with the yellow one"}],
        "lone": ["a note standing alone"]
    })
}

fn unowned_notebook() -> String {
    let mut s = header("A Book Nobody Owns", "Nobody, A.");
    s.push_str(
        "<div class=\"noteHeading\">Highlight(<span class=\"highlight_yellow\">yellow</span>) - Location 5</div>\n\
         <div class=\"noteText\">Nobody here owns this one either.</div>\n",
    );
    s.push_str(FOOTER);
    s
}

const NOT_A_NOTEBOOK: &str = "<!DOCTYPE html>\n<html><head><title>Recipes</title></head>\n<body><div class=\"recipe\"><p>Bread.</p></div></body></html>\n";

fn write(path: &Path, contents: &str) -> std::io::Result<usize> {
    std::fs::write(path, contents)?;
    Ok(1)
}

fn pretty(v: &Value) -> String {
    let mut s = serde_json::to_string_pretty(v).expect("json");
    s.push('\n');
    s
}

/// Write every Kindle fixture under `root` (`crates/engine/tests/fixtures/kindle`).
pub fn generate(root: &Path) -> std::io::Result<usize> {
    let mut n = 0;
    let clips = root.join("clippings");
    for d in ["crlf", "lf", "appended"] {
        std::fs::create_dir_all(clips.join(d))?;
    }
    let base = base_clips();
    // As a Kindle writes it: a BOM, `\r\n`, a separator after every entry.
    n += write(
        &clips.join("crlf/My Clippings.txt"),
        &render_clips(&base, "\r\n", true),
    )?;
    n += write(
        &clips.join("lf/My Clippings.txt"),
        &render_clips(&base, "\n", false),
    )?;
    let mut grown = base_clips();
    grown.extend(appended_clips());
    n += write(
        &clips.join("appended/My Clippings.txt"),
        &render_clips(&grown, "\r\n", true),
    )?;
    let appended = appended_clips();
    n += write(
        &clips.join("expected.json"),
        &pretty(&json!({
            "base": clippings_expected(&base),
            "appended": {
                // The edit refreshes a highlight's note; the rest are new.
                "inserted": appended.iter().filter(|c| c.role == Role::Highlight).count(),
                "updated": appended.iter().filter(|c| matches!(c.role, Role::NoteOn(_))).count(),
                "book": clippings_expected(&grown),
            },
        })),
    )?;

    let nb = root.join("notebook");
    std::fs::create_dir_all(&nb)?;
    n += write(&nb.join("remains.html"), &remains_notebook())?;
    n += write(&nb.join("unowned.html"), &unowned_notebook())?;
    n += write(&nb.join("not-a-notebook.html"), NOT_A_NOTEBOOK)?;
    n += write(
        &nb.join("expected.json"),
        &pretty(&json!({
            "remains": remains_expected(),
            "unowned": {"title": "A Book Nobody Owns", "authors": ["Nobody, A."], "rows": 1},
        })),
    )?;
    Ok(n)
}
