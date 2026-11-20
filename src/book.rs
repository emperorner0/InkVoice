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

// ---------------- ids and folder names ----------------

/// 64-bit FNV-1a hash, written out so ids stay the same across Rust versions
/// (`DefaultHasher` promises no such thing).
pub fn fnv64a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// `s` reduced to lowercase ASCII letters, digits and single underscores. Also the slug for
/// book folder names: non-alphanumerics collapse to `_`, runs fold together, ends trimmed.
pub fn ascii_slug(s: &str) -> String {
    let mut out = String::new();
    let mut was_us = false;
    for c in deunicode::deunicode(s).chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            was_us = false;
        } else if !was_us && !out.is_empty() {
            out.push('_');
            was_us = true;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    out
}

/// The id scheme before short folder names: the imported file's stem, lowercased, with every
/// non-alphanumeric character replaced by `_`. Both import and migration hash this same string,
/// so a book imported fresh and one migrated from an old folder land on the same id.
pub fn legacy_id(stem: &str) -> String {
    stem.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect::<String>().to_lowercase()
}

/// Whether an id is already in the `<slug>-<8 hex>` form: a slug of at most 40 characters
/// from `a-z 0-9 _`, a hyphen, exactly 8 lowercase hex digits. Never decided by hashing, so
/// migration cannot repeat on a later start.
pub fn is_new_id(id: &str) -> bool {
    let Some((slug, hex)) = id.split_once('-') else { return false };
    !slug.is_empty()
        && slug.len() <= 40
        && slug.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && hex.len() == 8
        && hex.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// A book's folder name: `<slug>-<8 hex>` with the slug from the title (or the legacy id when
/// there is no title), at most 40 characters, and the hex digits from the FNV-1a hash of the
/// legacy id, so the same file always maps to the same short id.
pub fn book_id(title: &str, legacy: &str) -> String {
    let mut slug = {
        let s = ascii_slug(title);
        (!s.is_empty()).then_some(s).unwrap_or_else(|| ascii_slug(legacy))
    };
    slug.truncate(40); // ascii_slug is pure ASCII
    while slug.ends_with('_') {
        slug.pop();
    }
    let slug = if slug.is_empty() { "book".to_string() } else { slug };
    format!("{slug}-{:08x}", fnv64a(legacy) as u32)
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
    let mut v: Vec<Book> = Vec::new();
    let Ok(rd) = std::fs::read_dir(books_dir()) else { return v };
    for e in rd.flatten() {
        let Ok(bytes) = std::fs::read(e.path().join("book.json")) else { continue };
        let Ok(mut b) = serde_json::from_slice::<Book>(&bytes) else { continue };
        // the folder name is the truth: a half-finished move must not leave a book pointing
        // at a folder that does not exist
        if let Some(name) = e.file_name().to_str() {
            if b.id != name {
                b.id = name.to_string();
                let _ = b.save();
            }
        }
        // classify anything imported before sections had kinds, and keep the result
        if crate::sections::classify(&mut b) {
            let _ = b.save();
        }
        b.load_progress();
        v.push(b);
    }
    sort_library(&mut v);
    v
}

/// One-time move to short book folder names: every book whose id still has the old form has
/// its folder renamed and its id rewritten; ids already in the new form are never touched
/// again. When the target exists, or any step fails, the book is left exactly as it was.
/// Returns the renames done as (from, to) folder names.
pub fn migrate_library() -> Vec<(String, String)> {
    migrate_library_in(&books_dir())
}

fn migrate_library_in(books: &Path) -> Vec<(String, String)> {
    let mut done = Vec::new();
    let Ok(rd) = std::fs::read_dir(books) else { return done };
    for e in rd.flatten() {
        let dir = e.path();
        let Ok(bytes) = std::fs::read(dir.join("book.json")) else { continue };
        let Ok(mut b) = serde_json::from_slice::<Book>(&bytes) else { continue };
        if is_new_id(&b.id) {
            continue; // already migrated; hashing the new id would drift on every start
        }
        // the stored old id is the legacy id the import once made from the file stem, so a
        // fresh import of the same file lands on the same new id
        b.id = book_id(&b.title, &b.id);
        let Ok(new_bytes) = serde_json::to_vec(&b) else { continue };
        let Some(from) = dir.file_name().and_then(|n| n.to_str()).map(String::from) else { continue };
        let to = books.join(&b.id);
        if to.exists() {
            continue; // never overwrite
        }
        // crash-safe: the updated book.json sits inside the folder before it moves, and takes
        // the old file's place after; a failure at any step restores what was
        let tmp = dir.join("book.json.new");
        if std::fs::write(&tmp, &new_bytes).is_err() {
            continue;
        }
        if std::fs::rename(&dir, &to).is_err() {
            let _ = std::fs::remove_file(&tmp);
            continue;
        }
        if std::fs::rename(to.join("book.json.new"), to.join("book.json")).is_err() {
            let _ = std::fs::rename(&to, &dir);
            let _ = std::fs::remove_file(dir.join("book.json.new"));
            continue;
        }
        done.push((from, b.id.clone()));
    }
    done
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
    book.id = book_id(&book.title, &legacy_id(&stem));
    if book.chapters.is_empty() {
        bail!("No readable text found (scanned PDF?)");
    }
    crate::sections::classify(&mut book);
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
    let mut doc = epub::doc::EpubDoc::new(path)?;
    let title = doc.mdata("title").map(|m| m.value.trim().to_string()).unwrap_or_else(|| stem.to_string());
    let author = doc.mdata("creator").map(|m| m.value.trim().to_string()).unwrap_or_default();
    let cover = doc.get_cover().map(|(bytes, _mime)| bytes);
    // Prefer the book's own table of contents (real chapter titles, and chapters that share a
    // file are split at their anchors); fall back to one chapter per file.
    let mut chapters = chapters_from_toc(&mut doc);
    if chapters.len() < 2 {
        chapters = chapters_from_spine(&mut doc);
    }
    Ok((Book { id: String::new(), title, author, source: String::new(), chapters, pos: (0, 0), opened: 0, listen: None }, cover))
}

/// One chapter per spine file, titled from its first line.
fn chapters_from_spine(doc: &mut epub::doc::EpubDoc<std::io::BufReader<std::fs::File>>) -> Vec<Chapter> {
    let mut chapters = Vec::new();
    for i in 0..doc.spine.len() {
        doc.set_current_chapter(i);
        let Some((html, _)) = doc.get_current_str() else { continue };
        let text = html_to_text(&html);
        if text.chars().count() < 80 {
            continue;
        }
        let title = title_from_text(&text).unwrap_or_else(|| format!("Section {}", chapters.len() + 1));
        chapters.push(Chapter::new(title, paginate(&text)));
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

fn chapters_from_toc(doc: &mut epub::doc::EpubDoc<std::io::BufReader<std::fs::File>>) -> Vec<Chapter> {
    // flatten the TOC: (label, file, fragment)
    fn flat(n: &[epub::doc::NavPoint], out: &mut Vec<(String, PathBuf, Option<String>)>) {
        for p in n {
            let s = p.content.to_string_lossy().into_owned();
            let (file, frag) = match s.split_once('#') {
                Some((f, fr)) => (PathBuf::from(f), Some(fr.to_string())),
                None => (PathBuf::from(&s), None),
            };
            let label = p.label.split_whitespace().collect::<Vec<_>>().join(" ");
            out.push((label, file, frag));
            flat(&p.children, out);
        }
    }
    let mut toc = Vec::new();
    let nav = doc.toc.clone();
    flat(&nav, &mut toc);
    if toc.len() < 2 {
        return Vec::new();
    }

    // (title, text) in reading order; text before a file's first anchor continues the previous chapter
    let mut parts: Vec<(String, String)> = Vec::new();
    let spine: Vec<String> = doc.spine.iter().map(|s| s.idref.clone()).collect();
    for idref in spine {
        let Some(file) = doc.resources.get(&idref).map(|r| r.path.clone()) else { continue };
        let Some(html) = doc.get_resource_str_by_path(&file) else { continue };
        let mut cuts: Vec<(usize, String)> = toc
            .iter()
            .filter(|(_, f, _)| *f == file)
            .filter_map(|(label, _, frag)| match frag {
                None => Some((0, label.clone())),
                Some(fr) => anchor_pos(&html, fr).map(|p| (p, label.clone())),
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
                    None => parts.push((title_from_text(&text).unwrap_or_else(|| "Beginning".into()), text)),
                }
            }
        }
        for (k, (start, label)) in cuts.iter().enumerate() {
            let text = html_to_text(&html[*start..bounds[k + 1]]);
            parts.push((label.clone(), text));
        }
    }
    parts
        .into_iter()
        .filter(|(_, t)| t.trim().chars().count() >= 20)
        .map(|(title, text)| {
            Chapter::new(if title.is_empty() { "Untitled".into() } else { title.chars().take(80).collect() }, paginate(text.trim()))
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
    fn ids_are_short_and_stable() {
        use super::{ascii_slug, book_id, fnv64a, is_new_id, legacy_id};
        // FNV-1a 64-bit, pinned: the hash written out, not DefaultHasher
        assert_eq!(fnv64a("the_salt_road"), 0x2742_642c_4525_57ac);
        assert_eq!(legacy_id("Quiet Harbor - A. B. Carter"), "quiet_harbor___a__b__carter");
        // a fresh import and an old library migrate to the same id
        let stem = "The Ellis Family History - scanned 1911 copy";
        let old = legacy_id(stem);
        assert_eq!(book_id("The Ellis Family History", &old), "the_ellis_family_history-fbf52b90");
        assert_eq!(book_id("Quiet Harbor", &legacy_id("Quiet Harbor - A. B. Carter")), "quiet_harbor-9f6689e3");
        assert_eq!(book_id("", &legacy_id("A Long Way From Home")), "a_long_way_from_home-94f15b0e");
        let long = book_id(
            "The Complete and Utter History of Almost Everything Worth Mentioning",
            &legacy_id("The Complete and Utter History of Almost Everything Worth Mentioning"),
        );
        assert_eq!(long, "the_complete_and_utter_history_of_almost-9805916d");
        assert_eq!(long.len(), 49); // 40 slug + hyphen + 8 hex
        assert!(is_new_id(&long) && is_new_id("a-00000000") && !is_new_id("quiet_harbor___a__b__carter_"));
        assert!(!is_new_id("quiet-123456789")); // nine hex digits
        assert!(!is_new_id("Quiet_Harbor-9f6689e3")); // capitals are not the written form
        assert_eq!(ascii_slug("  Salt  &  Ember Fire "), "salt_ember_fire");
    }

    /// A library tree built in a scratch folder: an old long-id book, a book already in the
    /// new form, and one whose target name is taken. Migration renames the first once,
    /// leaves the others alone, and a second run does nothing.
    #[test]
    fn migrates_book_folders() {
        use super::{book_id, is_new_id, legacy_id, migrate_library_in, Book, Chapter};
        let root = std::env::temp_dir().join("inkvoice-test-migrate-folders");
        let _ = std::fs::remove_dir_all(&root);
        let books = root.join("books");
        let mk = |id: &str, title: &str| {
            let dir = books.join(id);
            std::fs::create_dir_all(dir.join("audio/phonon-everly")).unwrap();
            let b = Book {
                id: id.into(),
                title: title.into(),
                author: String::new(),
                source: String::new(),
                chapters: vec![Chapter::new("One".into(), vec!["text one".into(), "text two".into()])],
                pos: (0, 0),
                opened: 0,
                listen: None,
            };
            std::fs::write(dir.join("book.json"), serde_json::to_vec(&b).unwrap()).unwrap();
            std::fs::write(dir.join("progress.json"), r#"{"ch":0,"pg":1,"opened":9}"#).unwrap();
            std::fs::write(dir.join("audio/phonon-everly/c0000.wav"), b"wav").unwrap();
        };
        let stem = "the_ellis_family_history_and_their_many_relations_in_the_north_1902_second_edition";
        mk(stem, "The Ellis Family History"); // an old long id
        let settled = book_id("Winter Cars", "winter_cars");
        mk(&settled, "Winter Cars"); // already migrated
        mk("tide_charts", "Tide Charts");
        // the target folder for "tide_charts" is taken by a book with the same title
        mk(&book_id("Tide Charts", "tide_charts"), "Tide Charts");

        let done = migrate_library_in(&books);
        assert_eq!(done.len(), 1);
        let want = book_id("The Ellis Family History", stem);
        assert_eq!(done[0], (stem.to_string(), want.clone()));
        assert!(books.join(&want).is_dir(), "folder renamed");
        assert!(!books.join(stem).exists(), "old folder gone");
        let moved: Book = serde_json::from_slice(&std::fs::read(books.join(&want).join("book.json")).unwrap()).unwrap();
        assert_eq!(moved.id, want);
        assert!(is_new_id(&moved.id));
        assert_eq!(std::fs::read_to_string(books.join(&want).join("progress.json")).unwrap(), r#"{"ch":0,"pg":1,"opened":9}"#);
        assert!(books.join(&want).join("audio/phonon-everly/c0000.wav").exists(), "audio came along");
        // the taken target and the settled book are untouched
        assert!(books.join("tide_charts").is_dir());
        assert!(books.join(&book_id("Tide Charts", "tide_charts")).is_dir());
        assert!(books.join(&settled).is_dir());
        // a fresh import of the same file lands on the same id as the migrated book
        let stem_words = "The Ellis Family History and their many relations in the north 1902 second edition";
        assert_eq!(book_id("The Ellis Family History", &legacy_id(stem_words)), want);

        // the second run renames nothing
        assert!(migrate_library_in(&books).is_empty());
        assert!(migrate_library_in(&books).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
