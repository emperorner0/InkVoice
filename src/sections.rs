//! What a section of a book is (story, title page, table of contents, ...), so front and back
//! matter can be left out of listening without renumbering anything: a chapter's position in
//! `Book::chapters` stays its identity for audio files, saved places and resume.
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

impl Kind {
    /// Whether sections of this kind are left out of listening unless the reader says otherwise.
    pub fn skipped_by_default(self) -> bool {
        !matches!(self, Kind::Body | Kind::Dedication | Kind::Epigraph | Kind::Foreword | Kind::Afterword)
    }
}
