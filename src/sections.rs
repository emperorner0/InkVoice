//! What a section of a book is (story, title page, table of contents, ...), so front and back
//! matter can be left out of listening without renumbering anything: a chapter's position in
//! `Book::chapters` stays its identity for audio files, saved places and resume.
use crate::book::Book;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Cover,
    TitlePage,
    /// table of contents
    Contents,
    /// copyright and publication details
    Copyright,
    /// praise pages and "also by" lists
    Praise,
    /// about the author or editors
    About,
    Acknowledgments,
    /// notes, index, bibliography, glossary
    Notes,
    Dedication,
    Epigraph,
    /// foreword, preface, introduction
    Foreword,
    Afterword,
    /// other short material before the first story
    FrontMatter,
    /// other short material after the last story
    BackMatter,
    /// the book itself
    Body,
}

/// A keyword phrase and whether it normally carries a tail: "praise for <book>",
/// "about the author of <book>", "copyright ...", "also by ...". A tailed phrase settles a
/// section of any length; a plain one only settles a short section, or one outside the book's
/// stretch, so a story called "Notes from the Night Shift" keeps its place in the reading.
const PHRASES: &[(&[&str], Kind, bool)] = &[
    (&["table", "of", "contents"], Kind::Contents, false),
    (&["contents"], Kind::Contents, false),
    (&["title", "page"], Kind::TitlePage, false),
    (&["half", "title"], Kind::TitlePage, false),
    (&["copyright"], Kind::Copyright, true),
    (&["imprint"], Kind::Copyright, false),
    (&["colophon"], Kind::Copyright, false),
    (&["dedication"], Kind::Dedication, false),
    (&["epigraph"], Kind::Epigraph, false),
    (&["foreword"], Kind::Foreword, false),
    (&["preface"], Kind::Foreword, false),
    (&["introduction"], Kind::Foreword, false),
    (&["acknowledgments"], Kind::Acknowledgments, false),
    (&["acknowledgements"], Kind::Acknowledgments, false),
    (&["about", "the", "author"], Kind::About, true),
    (&["about", "the", "authors"], Kind::About, true),
    (&["about", "the", "editor"], Kind::About, true),
    (&["about", "the", "editors"], Kind::About, true),
    (&["praise", "for"], Kind::Praise, true),
    (&["acclaim", "for"], Kind::Praise, true),
    (&["critical", "acclaim"], Kind::Praise, true),
    (&["advance", "praise"], Kind::Praise, true),
    (&["praise"], Kind::Praise, false),
    (&["acclaim"], Kind::Praise, false),
    (&["reviews"], Kind::Praise, false),
    (&["also", "by"], Kind::Praise, true),
    (&["other", "books", "by"], Kind::Praise, true),
    (&["further", "reading"], Kind::Notes, false),
    (&["notes"], Kind::Notes, false),
    (&["endnotes"], Kind::Notes, false),
    (&["index"], Kind::Notes, false),
    (&["bibliography"], Kind::Notes, false),
    (&["glossary"], Kind::Notes, false),
    (&["afterword"], Kind::Afterword, false),
    (&["cover"], Kind::Cover, false),
    (&["frontmatter"], Kind::FrontMatter, false),
    (&["front", "matter"], Kind::FrontMatter, false),
];

impl Kind {
    /// Whether sections of this kind are left out of listening unless the reader says otherwise.
    pub fn skipped_by_default(self) -> bool {
        !matches!(self, Kind::Body | Kind::Dedication | Kind::Epigraph | Kind::Foreword | Kind::Afterword)
    }

    /// Short name shown in the interface.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Cover => "Cover",
            Kind::TitlePage => "Title page",
            Kind::Contents => "Contents",
            Kind::Copyright => "Copyright",
            Kind::Praise => "Praise",
            Kind::About => "About",
            Kind::Acknowledgments => "Acknowledgments",
            Kind::Notes => "Notes",
            Kind::Dedication => "Dedication",
            Kind::Epigraph => "Epigraph",
            Kind::Foreword => "Foreword",
            Kind::Afterword => "Afterword",
            Kind::FrontMatter => "Front matter",
            Kind::BackMatter => "Back matter",
            Kind::Body => "Chapter",
        }
    }

    /// Kind named by the source file's structural marker (lowercase EPUB vocabulary).
    /// "nonlinear" (a spine item marked linear="no") is out-of-line extra material.
    pub fn from_marker(marker: &str) -> Option<Kind> {
        Some(match marker {
            "cover" => Kind::Cover,
            "titlepage" => Kind::TitlePage,
            "toc" => Kind::Contents,
            "copyright-page" | "colophon" => Kind::Copyright,
            "dedication" => Kind::Dedication,
            "epigraph" => Kind::Epigraph,
            "foreword" | "preface" | "introduction" => Kind::Foreword,
            "acknowledgments" => Kind::Acknowledgments,
            "afterword" => Kind::Afterword,
            "bibliography" | "index" | "glossary" | "endnotes" => Kind::Notes,
            "frontmatter" => Kind::FrontMatter,
            "backmatter" | "nonlinear" => Kind::BackMatter,
            "bodymatter" => Kind::Body,
            _ => return None,
        })
    }
}

/// The kind a title suggests, when it suggests one, and whether that reading stands on its
/// own: true for a phrase the title is ("Contents", "Notes"), one it heads with a colon
/// ("Introduction: What These Stories Share"), or a tailed phrase ("praise for <book>").
/// False means a plain keyword merely opens a longer title ("Notes from the Night Shift"),
/// which only settles a short section or one outside the book's stretch.
fn title_kind(title: &str) -> Option<(Kind, bool)> {
    let mut words: Vec<String> = Vec::new();
    let mut colon_after: Vec<bool> = Vec::new(); // the word ended with ':' before trimming
    for w in deunicode::deunicode(title).to_lowercase().split_whitespace() {
        let ends_colon = w.ends_with(':');
        let t = w.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        if !t.is_empty() {
            words.push(t.to_string());
            colon_after.push(ends_colon);
        }
    }
    if words.first().map(String::as_str) == Some("the") {
        words.remove(0);
        colon_after.remove(0);
    }
    let eq = |k: &[&str]| words.len() == k.len() && words.iter().zip(k).all(|(w, kw)| w == kw);
    let opens = |k: &[&str]| words.len() > k.len() && words.iter().zip(k).all(|(w, kw)| w == kw);
    // the phrase heads the title: "Introduction: ..." (colon sits on the phrase's last word)
    let heads = |k: &[&str]| opens(k) && colon_after.get(k.len() - 1).copied().unwrap_or(false);
    for (k, kind, _) in PHRASES {
        if eq(k) || heads(k) {
            return Some((*kind, true));
        }
    }
    for (k, kind, tail) in PHRASES {
        if opens(k) {
            return Some((*kind, *tail));
        }
    }
    None
}

/// A section at most this long may be settled by a title keyword that opens it: praise pages,
/// copyright pages and "also by" lists of real books run this long, and stories do not.
const KEYWORD_SHORT_WORDS: usize = 350;
/// With no marker and no keyword, only a section this short becomes front or back matter by
/// position alone. Title cards, dedications and advert pages fit; a 250-word opening scene
/// or letter does not, and stays part of the reading.
const FILLER_WORDS: usize = 120;
/// The first and last section at least this long mark where the book itself starts and ends,
/// so only sections outside that stretch can be front or back matter.
const SUBSTANTIAL_WORDS: usize = 450;

/// Fill `kind` of every chapter that has none. Never touches a chapter that already has a
/// kind, so a hand classification survives re-runs. Returns whether anything changed (the
/// caller saves the book then).
pub fn classify(b: &mut Book) -> bool {
    let words: Vec<usize> = b
        .chapters
        .iter()
        .map(|c| c.pages.iter().map(|p| p.split_whitespace().count()).sum())
        .collect();
    let first_body = words.iter().position(|&w| w >= SUBSTANTIAL_WORDS);
    let last_body = words.iter().rposition(|&w| w >= SUBSTANTIAL_WORDS);
    let mut changed = false;
    for (i, c) in b.chapters.iter_mut().enumerate() {
        if c.kind.is_some() {
            continue;
        }
        let outer = first_body.is_some_and(|f| i < f) || last_body.is_some_and(|l| i > l);
        c.kind = Some(if let Some(k) = c.marker.as_deref().and_then(Kind::from_marker) {
            k
        } else if let Some((k, alone)) = title_kind(&c.title) {
            // a keyword that merely opens the title must not swallow reading: it settles
            // the section on its own, or outside the book's stretch, or when short
            (alone || outer || words[i] <= KEYWORD_SHORT_WORDS).then_some(k).unwrap_or(Kind::Body)
        } else if outer && words[i] <= FILLER_WORDS && !opens_like_chapter(&c.title) {
            if first_body.is_some_and(|f| i < f) { Kind::FrontMatter } else { Kind::BackMatter }
        } else {
            Kind::Body
        });
        changed = true;
    }
    changed
}

/// Titles that open a chapter or story even when the section is short: "Chapter 3", "Part Two",
/// "Book 2", "Prologue", "Epilogue", a bare number ("7") or a roman numeral ("VII").
fn opens_like_chapter(title: &str) -> bool {
    let words: Vec<String> = deunicode::deunicode(title)
        .to_lowercase()
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_ascii_alphanumeric()).to_string())
        .filter(|w| !w.is_empty())
        .collect();
    match words.first().map(String::as_str) {
        Some("chapter" | "part" | "book" | "prologue" | "epilogue") => true,
        Some(only) if words.len() == 1 => only.bytes().all(|b| b.is_ascii_digit()) || roman_numeral(only),
        _ => false,
    }
}

/// A well-formed lowercase roman numeral (i to mmm…): each letter at most three times in a
/// row, v l d never repeated, and a smaller letter may precede only the next two larger ones.
fn roman_numeral(s: &str) -> bool {
    let val = |c: u8| match c { b'i' => 1, b'v' => 5, b'x' => 10, b'l' => 50, b'c' => 100, b'd' => 500, b'm' => 1000, _ => 0 };
    let b = s.as_bytes();
    if b.is_empty() || b.iter().any(|&c| val(c) == 0) {
        return false;
    }
    let mut run = 1;
    for i in 1..b.len() {
        let (prev, cur) = (val(b[i - 1]), val(b[i]));
        if prev == cur {
            run += 1;
            if run > 3 || matches!(cur, 5 | 50 | 500) {
                return false;
            }
        } else {
            run = 1;
            let subtractive = matches!((prev, cur), (1, 5) | (1, 10) | (10, 50) | (10, 100) | (100, 500) | (100, 1000));
            if prev < cur && !subtractive {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::book::{Book, Chapter};

    fn sec(title: &str, words: usize) -> Chapter {
        Chapter::new(title.into(), vec!["word ".repeat(words)])
    }
    fn book(secs: Vec<Chapter>) -> Book {
        Book { id: "t".into(), title: "T".into(), author: String::new(), source: String::new(), chapters: secs, pos: (0, 0), opened: 0, listen: None }
    }
    fn kinds(b: &Book) -> Vec<&'static str> {
        b.chapters.iter().map(|c| c.kind.unwrap().label()).collect()
    }

    /// A story anthology: six short front sections, nineteen stories of 1,000 to 12,000 words
    /// (the first an introduction), three short back sections. The nine outer sections are
    /// skipped, the nineteen stories kept.
    #[test]
    fn anthology() {
        let story_titles = [
            "Introduction: What These Stories Share - R. Marlow",
            "The Quiet Harbor - Rosa Marlow",
            "Salt and Ember - J. Teague",
            "A Bell for the Ferryman - O. Shore",
            "Winter Cars - Lena Voss",
            "The Glass Astrolabe - P. Okafor",
            "Sixteen Minutes to Midnight - K. Ito",
            "The Orchard of Small Machines - D. Reyes",
            "Tide Charts - M. Almeida",
            "The Last Lighthouse Keeper - S. Bergman",
            "Paper Boats - H. Nakamura",
            "The Copper Kettle War - A. Dumont",
            "Rivers in Winter - G. Petrov",
            "The Map of Forgotten Doors - E. Whitlock",
            "Smoke over the Valley - T. Calhoun",
            "The Tailor's Clock - V. Ashworth",
            "Foxglove and Iron - N. Ferreira",
            "The Longest Night Market - W. Chen",
            "Every Exit Is an Entrance - B. Okoye",
        ];
        let mut secs: Vec<Chapter> = vec![
            sec("Contents", 80),
            sec("Praise for the collection", 260),
            sec("Praise for their earlier books", 310),
            sec("Also by these writers", 60),
            sec("Title Page", 8),
            sec("Frontmatter", 90),
        ];
        // 1,000 to 12,000 words each, varied so no two neighbours are equal
        for (i, t) in story_titles.iter().enumerate() {
            secs.push(sec(t, 1000 + i * 579 % 11000));
        }
        secs.push(sec("Copyright Acknowledgments", 210));
        secs.push(sec("About the Editors", 280));
        secs.push(sec("Notes", 340));
        let mut b = book(secs);
        classify(&mut b);
        assert_eq!(kinds(&b)[0..6], ["Contents", "Praise", "Praise", "Praise", "Title page", "Front matter"]);
        assert_eq!(kinds(&b)[6], "Foreword");
        assert_eq!(kinds(&b)[25..28], ["Copyright", "About", "Notes"]);
        let skipped: Vec<bool> = b.chapters.iter().map(Chapter::skipped).collect();
        assert_eq!(skipped.iter().filter(|&&s| s).count(), 9);
        assert!(skipped[0..6].iter().all(|&s| s));
        assert!(!skipped[6..25].iter().any(|&s| s));
        assert!(skipped[25..28].iter().all(|&s| s));
        // classifying again changes nothing, and a set kind is never replaced
        let mut again = b.clone();
        assert!(!classify(&mut again));
        again.chapters[7].kind = Some(Kind::Body);
        assert!(!classify(&mut again));
    }

    /// A novel: a short chapter in the middle and a short opener that names itself a chapter
    /// both stay chapters; untitled short filler at the edges is front and back matter.
    #[test]
    fn novel_keeps_short_chapters() {
        let mut b = book(vec![
            sec("Chapter One", 120),
            sec("The Road North", 5200),
            sec("Chapter Two", 150),
            sec("The Cold House", 8000),
            sec("Chapter Three", 6000),
        ]);
        classify(&mut b);
        assert_eq!(kinds(&b), vec!["Chapter", "Chapter", "Chapter", "Chapter", "Chapter"]);
        let mut edges = book(vec![sec("", 90), sec("The Long Way Around", 9000), sec("", 120)]);
        classify(&mut edges);
        assert_eq!(kinds(&edges), vec!["Front matter", "Chapter", "Back matter"]);
        // no substantial section at all: nothing is demoted to front or back matter
        let mut tiny = book(vec![sec("", 60), sec("One Morning", 200), sec("", 80)]);
        classify(&mut tiny);
        assert_eq!(kinds(&tiny), vec!["Chapter", "Chapter", "Chapter"]);
    }

    /// A keyword that merely opens a title must not swallow reading inside the book: long
    /// stories named "Notes ...", "Cover ...", "Index ...", "Introduction to ..." stay Body.
    #[test]
    fn keyword_titles_inside_the_book_stay_reading() {
        for title in ["Notes from the Night Shift", "Cover of Darkness", "Index of Small Mercies", "Introduction to Drowning"] {
            let mut b = book(vec![sec("The Opening", 5000), sec(title, 3000), sec("The Closing", 5000)]);
            classify(&mut b);
            assert_eq!(b.chapters[1].kind, Some(Kind::Body), "{title}");
        }
        // "Introduction: <subtitle>" heads a real introduction; a bare keyword title is
        // itself; a tailed praise phrase settles a praise page of any length
        let mut b = book(vec![sec("Introduction: How to Read This Book", 900), sec("Notes", 600), sec("Critical Acclaim for Some Book", 340), sec("The Long Middle", 9000)]);
        classify(&mut b);
        assert_eq!(kinds(&b)[0..3], ["Foreword", "Notes", "Praise"]);
        assert!(b.chapters[1].skipped() && b.chapters[2].skipped() && !b.chapters[0].skipped());
    }

    /// Without a marker or a keyword, position alone only demotes very short sections: a
    /// 250-word opening scene titled by its first line stays reading, a 60-word one goes.
    #[test]
    fn position_rule_is_for_very_short_fillers() {
        let mut b = book(vec![
            sec("Dear Editor, the letter began as they all did", 250),
            sec("The Long Middle", 9000),
            sec("The tide took the rest of it that autumn", 60),
        ]);
        classify(&mut b);
        assert_eq!(kinds(&b), vec!["Chapter", "Chapter", "Back matter"]);
    }

    /// The source file's marker decides first, and a story that merely contains a keyword
    /// in its title is not mistaken for back matter.
    #[test]
    fn markers_and_titles() {
        let mut c = sec("Anything", 5000);
        c.marker = Some("toc".into());
        let mut b = book(vec![c, sec("The Index of Lost Trains", 5000)]);
        classify(&mut b);
        assert_eq!(kinds(&b), vec!["Contents", "Chapter"]);
        assert_eq!(Kind::from_marker("nonlinear"), Some(Kind::BackMatter));
        assert_eq!(Kind::from_marker("bodymatter"), Some(Kind::Body));
        assert_eq!(Kind::from_marker("cover"), Some(Kind::Cover));
        assert_eq!(Kind::from_marker("nonsense"), None);
        assert_eq!(title_kind("Half Title"), Some((Kind::TitlePage, true)));
        assert_eq!(title_kind("Table of Contents"), Some((Kind::Contents, true)));
        assert_eq!(title_kind("COPYRIGHT PAGE"), Some((Kind::Copyright, true)));
        assert_eq!(title_kind("Acknowledgements"), Some((Kind::Acknowledgments, true)));
        assert_eq!(title_kind("Other Books by This Author"), Some((Kind::Praise, true)));
        assert_eq!(title_kind("Further Reading"), Some((Kind::Notes, true)));
        assert_eq!(title_kind("Advance Praise for the Novel"), Some((Kind::Praise, true)));
        assert_eq!(title_kind("Notes from the Night Shift"), Some((Kind::Notes, false)));
        assert_eq!(title_kind("Introduction: What These Stories Share"), Some((Kind::Foreword, true)));
        assert_eq!(title_kind("The Miller's Tale"), None);
        assert_eq!(title_kind("Epilogue: What Came After"), None);
    }

    #[test]
    fn chapter_like_titles() {
        assert!(opens_like_chapter("Chapter 12"));
        assert!(opens_like_chapter("PART TWO"));
        assert!(opens_like_chapter("Book 3"));
        assert!(opens_like_chapter("Prologue"));
        assert!(opens_like_chapter("Epilogue"));
        assert!(opens_like_chapter("7"));
        assert!(opens_like_chapter("XIV"));
        assert!(opens_like_chapter("mmxxiv"));
        assert!(!opens_like_chapter("The Miller's Tale"));
        assert!(!opens_like_chapter("14 Minutes"));
        assert!(!opens_like_chapter("Vixen")); // i before x twice over is not a numeral
    }
}
