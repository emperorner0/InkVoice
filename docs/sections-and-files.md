# Sections and file names

InkVoice looks at what each section of a book is (a story, the title page, the table of
contents, praise quotes, an afterword) and leaves the parts nobody listens to out of the
audiobook. Nothing is removed or renumbered: a section keeps its place in the book, its audio
files and your reading position, whether it is listened to or not.

## What is skipped when listening

Skipped by default: the cover, title and half-title pages, the table of contents, copyright
and imprint pages, praise and review pages, "also by" lists, about-the-author and
about-the-editor pages, acknowledgments, notes, endnotes, indexes, bibliographies, glossaries
and further-reading lists, and other very short front or back matter.

Kept by default: the book itself, dedications, epigraphs, forewords, prefaces, introductions
and afterwords.

A skipped section is left out of continuous playback, the previous and next buttons, the
prefetch of the next chapter and "Generate whole book". Playing or generating it on purpose
still works: click it in the contents list, use its play or generate button, or regenerate it
from its context menu.

To change one section, right-click it in the contents list and choose "Skip when listening"
or "Include when listening". The choice applies to that section only and is saved with the
book, so it survives restarts. Counts in the interface (the contents header, the ready count,
"Chapter X of N" in the reader) count only the sections you listen to; a skipped section
shows its kind where its number would be.

## How sections are recognised

When a book is imported, every section gets a kind, in this order:

1. The structural label the source file gave it (EPUB vocabulary: `cover`, `titlepage`,
   `toc`, `copyright-page`, `foreword`, `backmatter`, and so on). This wins when present.
2. Its title. A phrase like "Contents", "Title Page", "Dedication" or "Notes" settles the
   section when it is the whole title, or heads it with a colon ("Introduction: Why This
   Book"), or is one of the phrases that normally carry a tail ("Praise for ...", "Also by
   ...", "About the author of ...", "Copyright ..."). A plain keyword that merely opens a
   longer title ("Notes from the Night Shift") only counts when the section is short or sits
   outside the book's stretch, so stories named after matter keep their place in the reading.
3. Position and length. With no marker and no keyword, a section of at most 120 words before
   the first substantial section or after the last one becomes front or back matter, unless
   it opens like a chapter ("Chapter 3", "Part Two", "Prologue", a bare number or a roman
   numeral). The first and last section of at least 450 words mark where the book itself
   starts and ends; sections up to 350 words may also be settled by a title keyword.
4. Everything else is the book itself.

Books imported before this existed are classified the first time they load. A classification
is never recomputed for a section that already has a kind.

## How files and folders are named

Each book lives in `<data>/books/<id>` with `book.json`, `progress.json`, `cover.png` and its
audio. The id is `<slug>-<8 hex digits>`: the slug comes from the title (lowercased,
transliterated to ASCII, at most 40 characters), and the eight hex digits are a hash of the
legacy id the old scheme made from the file name, so the same file always maps to the same
folder and importing it twice is still recognised.

Audio for a voice sits in `<data>/books/<id>/audio/<engine>-<voice>/` and is named
`<NN>_<Title>_c<CCCC>.wav`, or `<NN>_<Title>_c<CCCC>_p<PPPP>.wav` in page mode. `NN` is the
section's index and `c<CCCC>` (with `_p<PPPP>` for one page) is the key the app relies on;
the title part is there for people browsing the folder. In the title part, words are joined
with underscores, and a "Title - Author" title becomes `Title_Author` with the author's
letters and digits run together ("Quiet Harbor - A. B. Carter" -> `Quiet_Harbor_ABCarter`).
Only ASCII letters, digits and underscores appear, capped at 60 characters; a title that
leaves nothing is simply omitted. Word timings sit in a `.json` beside each file with the
same name, and streaming audio grows a `<file>.stream.pcm` while it is generated.

Looking a section's audio up goes by the key at the end of the name, so a title change never
orphans existing audio, and both this form and the old bare `c0006.wav` form are read.

## The one-time migration

On start-up, before the library is used:

- Every book whose id still has the old form has its folder renamed to its new short id and
  the id rewritten in `book.json`. The updated file is written inside the folder before it
  moves, and takes the old file's place after, so an interrupted move cannot lose a book; if
  the target exists or any step fails, that book is left exactly as it was. Nothing is ever
  deleted.
- Books imported before sections had kinds are classified and saved.
- In every voice folder, audio files still named the old way (`c0006.wav` and its `.json`)
  are renamed to the readable form, together with their timing files. Leftover
  `<file>.stream.pcm` files from interrupted streams are removed; nothing else is touched. A
  file that cannot be renamed is left as it is, and the whole pass is idempotent: a second
  run finds nothing to do.

To see what would happen on a copy of your library without touching the real one, copy the
`books` folders into `<somewhere>/data/books`, then run:

```sh
MIGRATE_DIR=<somewhere> cargo test migrates_library_copy -- --ignored --nocapture
```

It prints folder name lengths, how many files each voice folder had renamed or left, and each
section's index, kind and skip state. It refuses to run unless both redirected folders are
inside `MIGRATE_DIR`, and prints no titles or book text.
