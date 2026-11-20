use crate::sections::Kind;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const PAGE_CHARS: usize = 1800;
const PDF_PAGES_PER_CHAPTER: usize = 10;

#[derive(Serialize, Deserialize, Clone)]
pub struct Chapter {
    pub title: String,
    pub pages: Vec<String>,
    /// structural label the source file gave this section, lowercase EPUB vocabulary
    /// ("cover", "titlepage", "toc", "copyright-page", "bodymatter", ...; "nonlinear" for a
    /// spine item marked linear="no"); None when the file says nothing
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marker: Option<String>,
    /// what the section is; None until the book has been classified
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
    /// the reader's own choice to skip (true) or include (false) this section when listening;
    /// None follows `kind`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip: Option<bool>,
}

impl Chapter {
    pub fn new(title: String, pages: Vec<String>) -> Self {
        Chapter { title, pages, marker: None, kind: None, skip: None }
    }
    /// Whether continuous listening and "Generate whole book" leave this section out.
    pub fn skipped(&self) -> bool {
        self.skip.unwrap_or_else(|| self.kind.is_some_and(Kind::skipped_by_default))
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Book {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub author: String,
    /// original file, for re-import
    #[serde(default)]
    pub source: String,
    pub chapters: Vec<Chapter>,
    /// last read (chapter, page); lives in progress.json, not book.json
    #[serde(skip, default)]
    pub pos: (usize, usize),
    /// unix seconds the book was last opened (progress.json)
    #[serde(skip, default)]
    pub opened: u64,
    /// the word last heard, so listening resumes exactly there (progress.json)
    #[serde(skip, default)]
    pub listen: Option<Listen>,
}

/// Where listening stopped: the audio unit (chapter, or chapter + page in page mode) and the
/// char offset of the spoken word inside that unit's text. Text offsets (not seconds) stay valid
/// when the audio is regenerated or the voice changes.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Listen {
    pub ch: usize,
    pub pg: Option<usize>,
    pub chr: usize,
}

#[derive(Serialize, Deserialize, Default)]
struct Progress {
    ch: usize,
    pg: usize,
    opened: u64,
    #[serde(default)]
    listen: Option<Listen>,
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Where books, audio, voice engines and their models live
/// (~/.local/share/inkvoice on Linux, %LOCALAPPDATA%\inkvoice on Windows). On Windows the
/// Known Folder API ignores LOCALAPPDATA, so INKVOICE_DATA is the only redirect that works
/// everywhere; the worker processes read the same variable (workers/common.py).
pub fn data_dir() -> PathBuf {
    std::env::var_os("INKVOICE_DATA").map_or_else(
        || dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")).join("inkvoice"),
        PathBuf::from,
    )
}

pub fn config_dir() -> PathBuf {
    std::env::var_os("INKVOICE_CONFIG").map_or_else(
        || dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("inkvoice"),
        PathBuf::from,
    )
}

/// Early versions were called BookReader: move their data and settings over once.
pub fn migrate_legacy() {
    let moves = [
        (dirs::data_dir().map(|d| d.join("bookreader")), data_dir()),
        (dirs::config_dir().map(|d| d.join("bookreader")), config_dir()),
    ];
    for (old, new) in moves {
        if let Some(old) = old {
            if old.is_dir() && !new.exists() {
                let _ = std::fs::create_dir_all(new.parent().unwrap_or(&new));
                let _ = std::fs::rename(&old, &new);
            }
        }
    }
}
pub fn books_dir() -> PathBuf {
    data_dir().join("books")
}
impl Book {
    pub fn dir(&self) -> PathBuf {
        books_dir().join(&self.id)
    }
    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all(self.dir())?;
        std::fs::write(self.dir().join("book.json"), serde_json::to_vec(self)?)?;
        Ok(())
    }
    pub fn cover_path(&self) -> PathBuf {
        self.dir().join("cover.png")
    }
    /// Load position + last-opened from progress.json (also accepts the old `[ch, pg]` format).
    fn load_progress(&mut self) {
        let bytes = std::fs::read(self.dir().join("progress.json")).unwrap_or_default();
        let p = serde_json::from_slice::<Progress>(&bytes)
            .or_else(|_| serde_json::from_slice::<(usize, usize)>(&bytes).map(|(ch, pg)| Progress { ch, pg, opened: 0, listen: None }))
            .unwrap_or_default();
        let valid = p.ch < self.chapters.len() && p.pg < self.chapters[p.ch].pages.len();
        self.pos = if valid { (p.ch, p.pg) } else { (0, 0) };
        self.opened = p.opened;
        self.listen = p.listen.filter(|l| l.ch < self.chapters.len() && l.pg.map_or(true, |pg| pg < self.chapters[l.ch].pages.len()));
    }
    pub fn save_progress(&self) {
        let p = Progress { ch: self.pos.0, pg: self.pos.1, opened: self.opened, listen: self.listen.clone() };
        let _ = std::fs::write(self.dir().join("progress.json"), serde_json::to_vec(&p).unwrap());
    }
    /// Fraction of pages before the current position.
    pub fn read_fraction(&self) -> f32 {
        let before: usize = self.chapters[..self.pos.0].iter().map(|c| c.pages.len()).sum::<usize>() + self.pos.1;
        before as f32 / self.page_count().max(1) as f32
    }
    /// Char offset (Unicode scalar count) where each page starts inside `chapter_text(ci)`.
    pub fn page_char_starts(&self, ci: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut at = 0;
        for p in &self.chapters[ci].pages {
            out.push(at);
            at += p.chars().count() + 2; // "\n\n" separator
        }
        out
    }
    pub fn page_count(&self) -> usize {
        self.chapters.iter().map(|c| c.pages.len()).sum()
    }
    pub fn chapter_text(&self, ci: usize) -> String {
        self.chapters[ci].pages.join("\n\n")
    }
}

pub fn load_library() -> Vec<Book> {
    let mut v: Vec<Book> = std::fs::read_dir(books_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| std::fs::read(e.path().join("book.json")).ok())
        .filter_map(|b| serde_json::from_slice::<Book>(&b).ok())
        .map(|mut b| {
            b.load_progress();
            b
        })
        .collect();
    sort_library(&mut v);
    v
}

/// Most recently opened first, then by title.
pub fn sort_library(v: &mut [Book]) {
    v.sort_by(|a, b| b.opened.cmp(&a.opened).then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase())));
}

pub fn import(path: &Path) -> Result<Book> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("book").to_string();
    let (mut book, cover) = match ext.as_str() {
        "epub" => import_epub(path, &stem)?,
        "pdf" => (import_pdf(path, &stem)?, None),
        _ => bail!("Unsupported file type (use .epub or .pdf)"),
    };
    book.source = path.to_string_lossy().into_owned();
    book.id = stem
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect::<String>()
        .to_lowercase();
    if book.chapters.is_empty() {
        bail!("No readable text found (scanned PDF?)");
    }
    book.save()?;
    if let Some(bytes) = cover {
        // store a small thumbnail; a missing/odd cover is not an import error
        if let Ok(img) = image::load_from_memory(&bytes) {
            let _ = img.thumbnail(320, 480).save(book.cover_path());
        }
    }
    book.load_progress();
    Ok(book)
}

fn import_epub(path: &Path, stem: &str) -> Result<(Book, Option<Vec<u8>>)> {
    let doc = rbook::Epub::open(path)?;
    let title = doc.metadata().title().map(|m| m.value().trim().to_string()).unwrap_or_else(|| stem.to_string());
    let author = doc.metadata().creators().next().map(|m| m.value().trim().to_string()).unwrap_or_default();
    let cover = doc.manifest().cover_image().and_then(|c| c.read_bytes().ok());
    // Prefer the book's own table of contents (real chapter titles, and chapters that share a
    // file are split at their anchors); fall back to one chapter per file.
    let mut chapters = chapters_from_toc(&doc);
    if chapters.len() < 2 {
        chapters = chapters_from_spine(&doc);
    }
    Ok((Book { id: String::new(), title, author, source: String::new(), chapters, pos: (0, 0), opened: 0, listen: None }, cover))
}

/// One chapter per spine file, titled from its first line.
fn chapters_from_spine(doc: &rbook::Epub) -> Vec<Chapter> {
    let marks = landmark_markers(doc);
    let mut chapters = Vec::new();
    for entry in doc.spine() {
        let Some(m) = entry.manifest_entry() else { continue };
        let Ok(html) = doc.read_resource_str(m.resource()) else { continue };
        let text = html_to_text(&html);
        if text.chars().count() < 80 {
            continue;
        }
        // a landmark onto this file marks the chapter that starts at it, fragment or not
        let file = m.href();
        let marker = marks
            .iter()
            .find(|(f, fr, _)| f.as_str() == file.as_str() && fr.is_none())
            .or_else(|| marks.iter().find(|(f, _, _)| f.as_str() == file.as_str()))
            .map(|(_, _, k)| *k)
            .or_else(|| body_marker(&html))
            .or_else(|| (!entry.is_linear()).then_some("nonlinear"));
        let title = title_from_text(&text).unwrap_or_else(|| format!("Section {}", chapters.len() + 1));
        let mut ch = Chapter::new(title, paginate(&text));
        ch.marker = marker.map(str::to_string);
        chapters.push(ch);
    }
    chapters
}

fn title_from_text(text: &str) -> Option<String> {
    // first real line (skip picture placeholders such as "image")
    let first = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.eq_ignore_ascii_case("image") && !l.eq_ignore_ascii_case("[image]"))?;
    (first.chars().count() <= 70).then(|| first.to_string())
}

fn chapters_from_toc(doc: &rbook::Epub) -> Vec<Chapter> {
    // flatten the TOC: (label, file, fragment)
    let mut toc: Vec<(String, String, Option<String>)> = Vec::new();
    if let Some(nav) = doc.toc().contents() {
        for p in nav.flatten() {
            let Some(href) = p.href() else { continue };
            let label = p.label().split_whitespace().collect::<Vec<_>>().join(" ");
            toc.push((label, href.path().as_str().to_string(), href.fragment().map(str::to_string)));
        }
    }
    if toc.len() < 2 {
        return Vec::new();
    }
    let marks = landmark_markers(doc);

    // (title, text, marker) in reading order; text before a file's first anchor continues the previous chapter
    let mut parts: Vec<(String, String, Option<&'static str>)> = Vec::new();
    for entry in doc.spine() {
        let Some(m) = entry.manifest_entry() else { continue };
        let Ok(html) = doc.read_resource_str(m.resource()) else { continue };
        let file = m.href().as_str().to_string();
        let doc_type = body_marker(&html);
        let mut cuts: Vec<(usize, &str, Option<&str>)> = toc
            .iter()
            .filter(|(_, f, _)| f == &file)
            .filter_map(|(label, _, frag)| match frag.as_deref() {
                None => Some((0, label.as_str(), None)),
                Some(fr) => anchor_pos(&html, fr).map(|p| (p, label.as_str(), Some(fr))),
            })
            .collect();
        cuts.sort_by_key(|c| c.0);
        cuts.dedup_by_key(|c| c.0);
        let mut bounds: Vec<usize> = cuts.iter().map(|c| c.0).collect();
        bounds.push(html.len());
        let head_end = bounds.first().copied().unwrap_or(html.len());
        // text before the first chapter start in this file
        if head_end > 0 {
            let text = html_to_text(&html[..head_end]);
            if !text.trim().is_empty() {
                match parts.last_mut() {
                    Some(last) => {
                        last.1.push_str("\n\n");
                        last.1.push_str(&text);
                    }
                    None => {
                        let marker = marks
                            .iter()
                            .find(|(f, fr, _)| f == &file && fr.is_none())
                            .map(|(_, _, k)| *k)
                            .or(doc_type)
                            .or((!entry.is_linear()).then_some("nonlinear"));
                        parts.push((title_from_text(&text).unwrap_or_else(|| "Beginning".into()), text, marker));
                    }
                }
            }
        }
        for (k, (start, label, frag)) in cuts.iter().enumerate() {
            let text = html_to_text(&html[*start..bounds[k + 1]]);
            let marker = marks
                .iter()
                .find(|(f, fr, _)| f == &file && fr.as_deref() == *frag)
                .map(|(_, _, k)| *k)
                .or(doc_type)
                .or((!entry.is_linear()).then_some("nonlinear"));
            parts.push((label.to_string(), text, marker));
        }
    }
    parts
        .into_iter()
        .filter(|(_, t, _)| t.trim().chars().count() >= 20)
        .map(|(title, text, marker)| {
            let mut ch = Chapter::new(
                if title.is_empty() { "Untitled".into() } else { title.chars().take(80).collect() },
                paginate(text.trim()),
            );
            ch.marker = marker.map(str::to_string);
            ch
        })
        .collect()
}

/// Byte offset of the tag carrying `id="frag"` (start of the chapter inside a shared file).
fn anchor_pos(html: &str, frag: &str) -> Option<usize> {
    for q in ['"', '\''] {
        for attr in ["id=", "name="] {
            let needle = format!("{attr}{q}{frag}{q}");
            if let Some(i) = html.find(&needle) {
                return html[..i].rfind('<');
            }
        }
    }
    None
}

/// Markers from the EPUB 3 landmarks nav or, failing that, the EPUB 2 OPF guide (rbook serves
/// both as the "landmarks" group, the nav winning when both exist): (file, fragment, marker).
fn landmark_markers(doc: &rbook::Epub) -> Vec<(String, Option<String>, &'static str)> {
    let mut marks = Vec::new();
    if let Some(root) = doc.toc().landmarks() {
        for e in root.flatten() {
            let marker = marker_term(e.kind_raw().unwrap_or_default());
            if let (Some(href), Some(marker)) = (e.href(), marker) {
                marks.push((href.path().as_str().to_string(), href.fragment().map(str::to_string), marker));
            }
        }
    }
    marks
}

/// Map a landmark/guide type or `epub:type` onto the marker vocabulary; None for other terms.
fn marker_term(raw: &str) -> Option<&'static str> {
    raw.split_whitespace().find_map(|t| {
        Some(match t.to_ascii_lowercase().as_str() {
            "cover" => "cover",
            "titlepage" | "title-page" => "titlepage",
            "toc" => "toc",
            "copyright-page" | "copyright" => "copyright-page",
            "dedication" => "dedication",
            "epigraph" => "epigraph",
            "foreword" => "foreword",
            "preface" => "preface",
            "introduction" => "introduction",
            "acknowledgments" | "acknowledgements" => "acknowledgments",
            "afterword" => "afterword",
            "colophon" => "colophon",
            "bibliography" => "bibliography",
            "index" => "index",
            "glossary" => "glossary",
            "endnotes" | "notes" => "endnotes",
            "frontmatter" => "frontmatter",
            "bodymatter" | "text" => "bodymatter",
            "backmatter" => "backmatter",
            _ => return None,
        })
    })
}

/// `epub:type` on the `<body>`, else on the first sectioning element, when it names a section.
fn body_marker(html: &str) -> Option<&'static str> {
    let at = tag_pos(html, "body")
        .or_else(|| ["article", "aside", "nav", "section"].iter().filter_map(|t| tag_pos(html, t)).min())?;
    let end = at + html[at..].find('>')?;
    attr_value(&html[at..end], "epub:type").and_then(marker_term)
}

/// Byte offset of `<tag`, when it is not the prefix of a longer tag name.
fn tag_pos(html: &str, tag: &str) -> Option<usize> {
    let needle = format!("<{tag}");
    let mut from = 0;
    while let Some(i) = html[from..].find(&needle) {
        let at = from + i;
        let after = html[at + needle.len()..].chars().next().unwrap_or(' ');
        if !after.is_ascii_alphanumeric() {
            return Some(at);
        }
        from = at + needle.len();
    }
    None
}

/// Value of a quoted attribute inside one tag.
fn attr_value<'a>(tag: &'a str, attr: &str) -> Option<&'a str> {
    let rest = tag.find(attr).map(|i| tag[i + attr.len()..].trim_start())?.strip_prefix('=')?.trim_start();
    let q = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    Some(&rest[1..1 + rest[1..].find(q)?])
}

fn html_to_text(html: &str) -> String {
    // plain, undecorated text: no markdown `*emphasis*`, `[link][1]` footnotes or `#` headers,
    // which would otherwise show up in the reader and be read aloud by the voice
    let raw = html2text::config::plain_no_decorate().string_from_read(html.as_bytes(), 100_000).unwrap_or_default();
    raw.lines()
        .map(clean_line)
        .collect::<Vec<_>>()
        .join("\n")
        .replace("\n\n\n", "\n\n")
        .trim()
        .to_string()
}

/// Drop leftover markup from the converter: `# ` heading marks and `[link]` brackets.
fn clean_line(l: &str) -> String {
    let l = l.trim_end();
    let l = if l.starts_with('#') { l.trim_start_matches('#').trim_start() } else { l };
    l.chars().filter(|c| *c != '[' && *c != ']').collect()
}

fn import_pdf(path: &Path, stem: &str) -> Result<Book> {
    let pages = pdf_extract::extract_text_by_pages(path)?;
    let pages: Vec<String> = pages
        .into_iter()
        .map(|p| p.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    let chapters = pages
        .chunks(PDF_PAGES_PER_CHAPTER)
        .enumerate()
        .map(|(i, c)| {
            let start = i * PDF_PAGES_PER_CHAPTER + 1;
            Chapter::new(format!("Pages {}-{}", start, start + c.len() - 1), c.to_vec())
        })
        .collect();
    Ok(Book { id: String::new(), title: stem.to_string(), author: String::new(), source: String::new(), chapters, pos: (0, 0), opened: 0, listen: None })
}

/// Split text into ~PAGE_CHARS pages on paragraph (then sentence/word) boundaries.
fn paginate(text: &str) -> Vec<String> {
    let mut pages = Vec::new();
    let mut cur = String::new();
    for para in text.split("\n\n").map(str::trim).filter(|p| !p.is_empty()) {
        for piece in split_long(para) {
            if !cur.is_empty() && cur.chars().count() + piece.chars().count() > PAGE_CHARS {
                pages.push(std::mem::take(&mut cur));
            }
            if !cur.is_empty() {
                cur.push_str("\n\n");
            }
            cur.push_str(&piece);
        }
    }
    if !cur.is_empty() {
        pages.push(cur);
    }
    pages
}

fn split_long(p: &str) -> Vec<String> {
    if p.chars().count() <= PAGE_CHARS {
        return vec![p.to_string()];
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    for w in p.split_whitespace() {
        if cur.chars().count() + w.len() + 1 > PAGE_CHARS {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(w);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    /// progress.json written by older versions (no listening position) still loads, and the
    /// listening position round-trips.
    #[test]
    fn progress_keeps_listening_position() {
        use super::{Listen, Progress};
        let old: Progress = serde_json::from_str(r#"{"ch":3,"pg":1,"opened":7}"#).unwrap();
        assert!(old.listen.is_none() && old.ch == 3 && old.pg == 1);
        let p = Progress { ch: 2, pg: 0, opened: 1, listen: Some(Listen { ch: 2, pg: None, chr: 1234 }) };
        let back: Progress = serde_json::from_slice(&serde_json::to_vec(&p).unwrap()).unwrap();
        assert_eq!(back.listen, Some(Listen { ch: 2, pg: None, chr: 1234 }));
    }

    #[test]
    fn imports_epub() {
        let Ok(p) = std::env::var("TEST_EPUB") else { return };
        let (b, _) = super::import_epub(std::path::Path::new(&p), "x").unwrap();
        assert_eq!(b.title, "Test Book");
        assert_eq!(b.chapters.len(), 2);
        assert_eq!(b.chapters[0].title, "Chapter One");
        assert!(b.chapters[0].pages.len() > 1);
        assert!(b.chapters[0].pages.iter().all(|p| p.chars().count() <= 2000));
        println!("{}", b.chapters[0].pages[0]);
    }

    #[test]
    fn imports_epub2_guide_fixture() {
        let (b, _) = super::import_epub(std::path::Path::new("tests/fixtures/epub2-guide.epub"), "x").unwrap();
        assert_eq!(b.title, "The Tide Clock");
        assert_eq!(b.author, "Adela Marsh");
        let titles: Vec<&str> = b.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, ["Cover", "Title Page", "Contents", "Chapter One", "Chapter Two"]);
        // guide reference types mapped onto the marker vocabulary; Chapter Two starts
        // mid-file where no guide entry points, so it carries no marker
        let markers: Vec<Option<&str>> = b.chapters.iter().map(|c| c.marker.as_deref()).collect();
        assert_eq!(markers, [Some("cover"), Some("titlepage"), Some("toc"), Some("bodymatter"), None]);
        // the anchor split: each side of the shared file lands in its own chapter
        let one = b.chapter_text(3);
        assert!(one.contains("Maren inherited the clock"));
        assert!(!one.contains("winter visitor"));
        let two = b.chapter_text(4);
        assert!(two.contains("winter visitor"));
        assert!(!two.contains("Maren inherited the clock"));
        // text before the first anchor of the shared file continues the previous chapter
        assert!(b.chapter_text(2).contains("The harbour keeps its own hours"));
    }

    #[test]
    fn imports_epub3_landmarks_fixture() {
        let (b, _) = super::import_epub(std::path::Path::new("tests/fixtures/epub3-landmarks.epub"), "x").unwrap();
        assert_eq!(b.title, "Signal Hill");
        let titles: Vec<&str> = b.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, ["Cover", "Dedication", "First Part", "Sidebar Notes"]);
        // cover and bodymatter from the landmarks nav, dedication from epub:type on the
        // body, the non-linear spine item from its linear="no"
        let markers: Vec<Option<&str>> = b.chapters.iter().map(|c| c.marker.as_deref()).collect();
        assert_eq!(markers, [Some("cover"), Some("dedication"), Some("bodymatter"), Some("nonlinear")]);
        assert!(b.chapter_text(2).contains("blinked in threes"));
        assert!(b.chapter_text(3).contains("how many lamps"));
    }

}
