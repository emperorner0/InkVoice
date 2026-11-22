# Managing generated audio

The details panel of a book (click a cover in the library) has an AUDIO section that lists
everything the app has narrated for that book, per voice, and lets you delete what you do
not need any more.

## What the panel shows

The AUDIO header carries the totals for the whole book: the number of files, their combined
playing time and their combined size on disk.

Below it, each voice folder gets its own group. The group header names the voice, how many
sections have audio in it, the total playing time and the total size. Every file is one row:
the section title (dimmed when the section is skipped when listening), the playing time, the
file size and when the file was made, plus the play button the panel always had. Rows of the
voice currently selected in Settings also have a regenerate button, which makes that
section's audio again with the current settings. Rows of other voices only have play and
delete, because regeneration always uses the selected voice.

When leftover files exist, one line at the end of the section says how many there are and
how much space they take.

## What each action does

- Delete a file (the bin icon on a row) moves that audio file and its timing file to the
  Recycle Bin. No confirmation is asked, since the file can be restored from there.
- Regenerate a row (the circular arrow) deletes the section's audio for the selected voice
  and queues it again with the current settings, exactly like "Regenerate audio" in the
  reader's contents list.
- The folder icon on a voice header opens that voice's folder in the system file manager
  (Explorer on Windows).
- "Delete all audio of this voice" (the bin icon on a voice header) removes every audio and
  timing file of that voice in this book. It asks first, stating how many files and how much
  space is involved.
- "Delete all audio" at the end of the section removes the audio of every voice in this
  book. It also asks first. The book itself is untouched: its text, cover and reading place
  stay.
- "Clean up" removes exactly the leftover files, also to the Recycle Bin.
- In the reader, a section's context menu has "Delete audio" next to "Regenerate audio". It
  removes that section's audio files for the current voice, and only appears when the
  section has audio.

Everything the panel deletes goes to the Recycle Bin, where it can be restored. When a file
cannot be moved there, the app leaves it where it is
and says why; it never deletes permanently on its own. Files that are playing are stopped
first, and a section whose audio is queued or being generated is cancelled before its files
are deleted, so deleted audio does not quietly reappear. Deletion only ever touches files
inside the book's own audio folder.

## What counts as a leftover

Leftovers are files under a book's audio folder that no generation needs any more:

- `*.stream.pcm` files from streams that were interrupted, so long as no generation is
  writing them now
- `*.part` files, the half-written temporary files of an interrupted generation
- `*.json.estimated` files from older versions of the app
- timing `*.json` files whose audio file is gone, unless that audio is queued or being
  generated now
- audio files whose section the book no longer has, with their timing files

The leftovers line and "Clean up" only cover these; nothing else is touched.
