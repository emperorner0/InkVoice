# Exporting audio

The details panel of a book (click a cover in the library) has an AUDIO section. Every
voice header carries an export button, the tray icon next to the folder and delete icons.
It opens the Export window, which copies that voice's finished audio out of the library
into a folder you choose, in a form audiobook players understand. The library itself is
only read during an export; nothing in it is changed, moved or deleted.

## What is exported

Only chapters that have complete audio in that voice are exported. A chapter counts as
complete when it has a whole-chapter file, or, in page mode, when every one of its pages
has been generated; a chapter with only some pages ready is left out, and the window says
so in one line ("12 of 19 chapters have audio; the other 7 are left out").

Front and back matter, the sections the app skips when listening (title pages, copyright
pages, about-the-author notes and the like), is left out by default, because most players
show them as chapters nobody wants. Tick "Include skipped sections" to take along those
that happen to have audio. In page mode, a chapter's page files are joined into one file
in page order, so every export is one file per chapter however the audio was generated.

## The two formats

One WAV file per chapter suits every audiobook player and any music player, since WAV
needs no decoding. The files are written into a folder "<Author> - <Title>" (just the
title when the book has no author) inside the destination, and named "01 - <Chapter
title>.wav", "02 - ...", counting the exported chapters in book order. This format keeps
the audio exactly as the voice generated it.

One M4B file with chapters produces a single "<Author> - <Title>.m4b", which is the
format phones and apps such as BookPlayer, Audiobookshelf and Apple Books sort into their
audiobook shelves. It carries one chapter mark per exported chapter with its title, the
book's title and author as metadata, and the book's cover as embedded cover art. The
audio is re-encoded to AAC at 64 kb/s mono, a bitrate that suits speech and keeps a long
book small; this takes a little while, and the window shows how far the encoding has come.
The chapters are first written as WAV files into a temporary folder inside the
destination, so building an M4B briefly needs free space there for one copy of the
chapters; the folder is removed once the file is done. This format needs the ffmpeg
program: the app looks for "ffmpeg" on the PATH once, when the Export window opens. When
it cannot find it, the option is greyed out and the window says that M4B export needs
ffmpeg installed.

Characters that file names do not allow on Windows, macOS or Linux are replaced, each
name is capped at a reasonable length, and letters beyond plain ASCII (accents, other
alphabets) are kept as they are, since these files are for people and players to read.

## During and after the export

The export runs on a background thread, so the app stays usable, and the Cancel button
stops it. Every file is first written under a temporary name and only renamed when it is
complete, so a cancelled or failed export never leaves a half-written file behind;
temporary files are removed on cancel and on failure. Chapters that finished before a
cancel stay on disk, complete. Nothing that exists is ever overwritten: when a name is
already taken, the new file is written as "name (2)", "name (3)" and so on.

A destination inside the app's data folder is refused, since exports never write back
into the library. When the export finishes, the window says where the files are and
offers an "Open folder" button that shows them in the file manager.
