# InkVoice

**[Website & downloads](https://sabeel111.github.io/InkVoice/)**

A tiny, dark-mode ebook reader that turns your EPUB and PDF books into audiobooks — with small
text-to-speech models running entirely on your own machine. No cloud, no Electron, no PyTorch.

![Reader with word highlighting](docs/screenshots/5_streaming.png)

## Features

- **Native & light** — a single ~10 MB app (Rust + egui) for Linux and Windows. Dark theme built for reading at night.
- **Library** with real cover art, reading progress, search, and every generated audio file per voice.
- **Read page by page or by chapter**, serif typography, adjustable size / spacing / width.
- **Listen per chapter or per page**, generated on your machine.
- **Streaming playback** — starts speaking a few seconds after you press play, from the page you are on,
  while the rest of the chapter is still being generated.
- **Follow-along** — the spoken word is highlighted and pages turn by themselves. Click any word to listen from there.
- **Speed control (0.5×–2.5×) without chipmunk pitch** (built-in WSOLA time-stretching).
- **Swappable voice engines**, each in its own process:

  | Engine | Size | Notes | License |
  |---|---|---|---|
  | [Kokoro](https://huggingface.co/hexgrad/Kokoro-82M) 82M (ONNX) | 115 MB | 28 English voices (US/UK) | Apache-2.0 |
  | [Phonon](https://gradium.ai/phonon) 31M | 125 MB | 24 English voices, ~2–4× real time on CPU | Apache-2.0 |
  | [Underdog Woof](https://huggingface.co/ConwayResearch/Underdog-Woof) | 41 MB | 2 voices, numpy only | **Non-commercial** |

  Engines are installed from inside the app (Settings → Voice engines); no PyTorch, no GPU needed.

- **Natural pacing** — whole paragraphs per generation step, trimmed model silences, adjustable pauses.
- **Advanced settings** per engine (temperature, weights, sentence splitting, …) and *Regenerate audio* per chapter.
- Continuous play into the next chapter, pre-generation of the next chapter, generation queue with progress & ETA.

| Library | Settings |
|---|---|
| ![Library](docs/screenshots/1_library.png) | ![Settings](docs/screenshots/4_settings.png) |

## What this fork adds

This repository is a fork of [sabeel111/InkVoice](https://github.com/sabeel111/InkVoice); the changes below sit on top of it.

- Qwen3 cloned-voice engine: a fourth engine that clones the voice in a reference sample. It needs an NVIDIA
  GPU with about 3 GB of free GPU memory. It installs from the app's voice-engine settings like the other
  engines, into its own Python environment (about 7 GB with PyTorch and the model); that in-app install has
  not been tested yet. A voice is a folder `<InkVoice data>/voices/<name>/` holding `ref.wav`, a clean 10 to
  25 s sample of the voice, and `ref.txt`, the exact words spoken in that sample. The app currently lists one
  voice, `narrator`.
- Measured word timings for that engine: a small wav2vec2 model force-aligns the generated audio to the text,
  so the highlighted word follows the voice. If alignment fails, the app falls back to the estimated timings
  from punctuation and pauses.
- Exact resume: the app remembers the word last heard for each book, and Play continues from that word after a
  pause or an app restart.
- Pronunciation helper (experimental, off by default): built-in rules read numbers, money, times and
  common abbreviations aloud correctly, with no server needed; an optional language model behind any
  OpenAI-compatible server settles ambiguous words like a past-tense "read". It works with every
  engine. See [docs/normalization.md](docs/normalization.md).
- Sections recognised as front or back matter (title pages, contents, praise, "also by", about-the-author,
  notes and the like) are left out of listening, generating and the chapter numbering, each one overridable
  from its context menu in the contents list. See [docs/sections-and-files.md](docs/sections-and-files.md).
- Short book folder names (`<title-slug>-<8 hex>`) and readable audio file names
  (`03_The_Quiet_Harbor_c0003.wav`), moved over once at start-up; audio lookups go by the chapter key, so
  old names and title changes never orphan a file.
- Audio manager in the book details panel: every generated file per voice with its size, duration and
  date, delete and regenerate per section, delete all for a voice or the whole book, a clean-up for
  leftover files, and a button that opens a voice's folder in the file manager. Deleted files go to the
  Recycle Bin, where they can be restored. See [docs/managing-audio.md](docs/managing-audio.md).

## Install

### Linux (x86_64)

```sh
curl -fsSL https://raw.githubusercontent.com/sabeel111/InkVoice/main/packaging/linux/get.sh | sh
```

Or download `inkvoice-linux-x86_64.tar.gz` from [Releases](https://github.com/sabeel111/InkVoice/releases),
unpack it and run `./install.sh` (installs for your user, no root). `./install.sh --uninstall` removes it.

### Windows 10/11 (x64)

Download **`InkVoice-Setup-x86_64.exe`** from [Releases](https://github.com/sabeel111/InkVoice/releases) and run it
(no admin rights needed), or use the portable zip. Windows may show a SmartScreen warning because the app
isn't code-signed yet — choose *More info → Run anyway*.

### Voices

On first start, open **Settings → Voice engines** and install a voice. InkVoice downloads everything it needs
into its own folder — a private Python (≈ 285 MB, once) plus the engine you pick — so nothing is installed
system-wide and no Python knowledge is needed. Remove engines there too.

### Build from source

```sh
git clone https://github.com/sabeel111/InkVoice.git && cd InkVoice
cargo build --release          # Linux needs ALSA headers (libasound2-dev / alsa-lib)
./target/release/inkvoice
```

Open a book directly with `inkvoice path/to/book.epub`, or import headlessly with `inkvoice --import book.epub`.

## Keyboard

| Key | Action |
|---|---|
| Space | Play / pause |
| ← / → | Previous / next page |
| Shift + ← / → | Back / forward 15 s |
| [ / ] | Slower / faster |
| Ctrl + O | Import a book |
| Ctrl + = / - | Text size |
| Ctrl + , | Settings |
| Esc | Back to library / close |

## How it works

```
src/main.rs      app state: library, queue, playback, streaming, follow-along
src/ui.rs        all drawing (library, reader, player bar, settings)
src/book.rs      EPUB/PDF import, pagination, covers, reading progress
src/player.rs    audio output, growing stream buffer, WSOLA speed change
src/tts.rs       talks to a voice worker over JSON lines (priority queue, cancel)
src/config.rs    settings, engines, voices, advanced parameters
src/installer.rs installs voice engines: uv → private Python → packages → model files
workers/         Python voice workers — common.py does pacing, word timing and streaming
```

Each engine is a small worker process; the UI never links a model, so engines can be swapped,
added or removed without touching the app. Word timings are estimated by matching punctuation to the
pauses detected in the audio.

Data lives in `~/.local/share/inkvoice` (Windows: `%LOCALAPPDATA%\inkvoice`) — books, audio, voice engines —
and settings in `~/.config/inkvoice/config.json` (Windows: `%APPDATA%\inkvoice`).

## License

InkVoice is released under the [MIT License](LICENSE).

## Model licenses

The models are downloaded by the app when you install an engine; they are not stored in this repository. Underdog Woof is free for
non-commercial use only; Phonon (31M English) and Kokoro are Apache-2.0.
