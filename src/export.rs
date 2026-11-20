//! Copying one voice's finished audio out of the library in a form audiobook players
//! understand: one WAV file per chapter, or one M4B with a chapter mark per chapter. The
//! library is only read; everything is written inside the folder the user chose.
use crate::book::{data_dir, Book};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

/// The PCM format of a wav, from its header: all files of one voice folder share it.
#[derive(PartialEq, Clone, Copy, Debug)]
pub struct WavFmt {
    pub rate: u32,
    pub channels: u16,
    pub bits: u16,
}

impl WavFmt {
    fn block(&self) -> u64 {
        self.channels as u64 * self.bits as u64 / 8
    }
}

/// One chapter's finished audio in the voice folder: its wav file, or all its page files
/// in page order (a chapter with only some pages generated has no audio). Files that
/// disagree on format or cannot be read still count as audio; joining them at export
/// time is what refuses, with a message naming the chapter.
pub struct ChapterAudio {
    pub files: Vec<PathBuf>,
    pub fmt: WavFmt,
    pub samples: u64,
    pub dur: f32,
}

/// One chapter of the export window's list (gathered once, when the window opens).
pub struct ExportChapter {
    pub title: String,
    pub skipped: bool,
    pub audio: Option<ChapterAudio>,
}

/// One chapter of an export run, with everything the run needs from the library.
pub struct PlannedChapter {
    pub title: String,
    pub files: Vec<PathBuf>,
    pub fmt: WavFmt,
    pub samples: u64,
    pub dur: f32,
}

/// Everything one export run writes and reads, fixed before its thread starts.
pub struct ExportPlan {
    pub title: String,
    pub author: String,
    pub cover: Option<PathBuf>,
    pub dest: PathBuf,
    pub chapters: Vec<PlannedChapter>,
}

/// How an export run ended: stopped by the user, or failed with a message for the window.
#[derive(Debug)]
pub enum ExportErr {
    Cancelled,
    Msg(String),
}

/// What an export background thread reports back to the app.
pub enum Msg {
    /// the folder dialog answered (None = dismissed)
    Dest(Option<PathBuf>),
    Progress(f32, String),
    Done(Result<PathBuf, ExportErr>),
}

/// The state of the Export window for one book and voice. Counts are gathered once, when
/// it opens, so drawing never touches the disk.
pub struct ExportWin {
    pub title: String,
    pub author: String,
    pub voice: String,
    pub cover: Option<PathBuf>,
    pub chapters: Vec<ExportChapter>,
    pub include_skipped: bool,
    /// the M4B format is chosen (false = one wav per chapter)
    pub m4b: bool,
    /// where the ffmpeg program was found, once per window (M4B needs it)
    pub ffmpeg: Option<PathBuf>,
    pub dest: Option<PathBuf>,
    pub dest_err: Option<String>,
    /// the folder dialog is open
    pub picking: bool,
    pub busy: Option<Busy>,
    /// how the last run ended: Ok((where the files are, it was an M4B)), Err(message)
    pub result: Option<Result<(PathBuf, bool), String>>,
}

/// A running export: the shared stop flag and what its thread last reported.
pub struct Busy {
    pub cancel: Arc<AtomicBool>,
    pub frac: f32,
    pub label: String,
    pub cancelling: bool,
}

impl ExportWin {
    pub fn new(b: &Book, profile: &str, voice_dir: &Path, dest: Option<PathBuf>) -> Self {
        Self {
            title: b.title.clone(),
            author: b.author.clone(),
            voice: profile.to_string(),
            cover: b.cover_path().exists().then(|| b.cover_path()),
            chapters: gather(b, voice_dir),
            include_skipped: false,
            m4b: false,
            ffmpeg: find_ffmpeg(),
            dest,
            dest_err: None,
            picking: false,
            busy: None,
            result: None,
        }
    }

    /// The chapters to write now, in book order, with their audio.
    pub fn plan(&self) -> ExportPlan {
        let chapters = selected(&self.chapters, self.include_skipped)
            .into_iter()
            .filter_map(|i| {
                self.chapters[i].audio.as_ref().map(|a| PlannedChapter {
                    title: self.chapters[i].title.clone(),
                    files: a.files.clone(),
                    fmt: a.fmt,
                    samples: a.samples,
                    dur: a.dur,
                })
            })
            .collect();
        ExportPlan {
            title: self.title.clone(),
            author: self.author.clone(),
            cover: self.cover.clone(),
            dest: self.dest.clone().unwrap_or_default(),
            chapters,
        }
    }
}

/// (format, offset of the data chunk's sample bytes, their length) of a PCM wav.
fn wav_layout(p: &Path) -> Result<(WavFmt, u64, u64), String> {
    let mut f = std::fs::File::open(p).map_err(|e| e.to_string())?;
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    let mut hdr = [0u8; 12];
    f.read_exact(&mut hdr).map_err(|e| e.to_string())?;
    if &hdr[0..4] != b"RIFF" || &hdr[8..12] != b"WAVE" {
        return Err("not a WAV file".into());
    }
    let (mut fmt, mut data, mut at) = (None, None, 12u64);
    while fmt.is_none() || data.is_none() {
        if at + 8 > len {
            return Err("WAV has no data chunk".into());
        }
        let mut ch = [0u8; 8];
        f.seek(SeekFrom::Start(at)).and_then(|_| f.read_exact(&mut ch)).map_err(|e| e.to_string())?;
        let clen = u32::from_le_bytes(ch[4..8].try_into().unwrap()) as u64;
        if &ch[0..4] == b"fmt " && fmt.is_none() && clen >= 16 {
            let mut b = vec![0u8; 16];
            f.read_exact(&mut b).map_err(|e| e.to_string())?;
            if u16::from_le_bytes(b[0..2].try_into().unwrap()) != 1 {
                return Err("not uncompressed PCM".into());
            }
            fmt = Some(WavFmt {
                channels: u16::from_le_bytes(b[2..4].try_into().unwrap()),
                rate: u32::from_le_bytes(b[4..8].try_into().unwrap()),
                bits: u16::from_le_bytes(b[14..16].try_into().unwrap()),
            });
        } else if &ch[0..4] == b"data" && data.is_none() {
            data = Some((at + 8, clen.min(len.saturating_sub(at + 8))));
        }
        at += 8 + clen + (clen & 1);
    }
    Ok((fmt.unwrap(), data.unwrap().0, data.unwrap().1))
}

/// A canonical 44-byte PCM wav header for `data` sample bytes of this format.
fn wav_header(fmt: WavFmt, data: u64) -> Result<Vec<u8>, String> {
    if data > u32::MAX as u64 - 36 {
        return Err("the audio is too long for one WAV file".into());
    }
    let block = fmt.block() as u32;
    let mut h = Vec::with_capacity(44);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&(36 + data as u32).to_le_bytes());
    h.extend_from_slice(b"WAVEfmt ");
    h.extend_from_slice(&16u32.to_le_bytes());
    h.extend_from_slice(&1u16.to_le_bytes());
    h.extend_from_slice(&fmt.channels.to_le_bytes());
    h.extend_from_slice(&fmt.rate.to_le_bytes());
    h.extend_from_slice(&(fmt.rate * block).to_le_bytes());
    h.extend_from_slice(&(block as u16).to_le_bytes());
    h.extend_from_slice(&fmt.bits.to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend_from_slice(&(data as u32).to_le_bytes());
    Ok(h)
}

/// The chapter's files as one wav at `out`: their sample data in order under one new
/// header, which needs all of them to share the format. Refuses with a message for the
/// window when they do not.
fn join_wav(files: &[PathBuf], out: &Path, title: &str) -> Result<(), String> {
    let mut fmt: Option<WavFmt> = None;
    let mut parts = Vec::new();
    let mut total = 0u64;
    for f in files {
        let (wf, off, len) = wav_layout(f).map_err(|e| format!("“{title}”: {} ({e})", f.display()))?;
        if fmt.is_some_and(|x| x != wf) {
            return Err(format!("“{title}” was generated with mixed audio formats; make its audio again before exporting"));
        }
        fmt = Some(wf);
        parts.push((off, len));
        total += len;
    }
    let Some(fmt) = fmt else { return Err(format!("“{title}” has no audio")) };
    let mut w = std::io::BufWriter::new(std::fs::File::create(out).map_err(|e| e.to_string())?);
    w.write_all(&wav_header(fmt, total)?).map_err(|e| e.to_string())?;
    for (f, (off, len)) in files.iter().zip(&parts) {
        let mut src = std::fs::File::open(f).map_err(|e| e.to_string())?;
        src.seek(SeekFrom::Start(*off)).and_then(|_| std::io::copy(&mut src.take(*len), &mut w)).map_err(|e| e.to_string())?;
    }
    w.flush().map_err(|e| e.to_string())
}

/// Which chapters an export writes: those the skip filter keeps and that have complete
/// audio, in book order (indices into the gathered list).
pub fn selected(chs: &[ExportChapter], include_skipped: bool) -> Vec<usize> {
    chs.iter()
        .enumerate()
        .filter(|(_, c)| (include_skipped || !c.skipped) && c.audio.is_some())
        .map(|(i, _)| i)
        .collect()
}

/// (chapters the skip filter keeps, how many of them have complete audio)
pub fn counts(chs: &[ExportChapter], include_skipped: bool) -> (usize, usize) {
    let kept: Vec<&ExportChapter> = chs.iter().filter(|c| include_skipped || !c.skipped).collect();
    let have = kept.iter().filter(|c| c.audio.is_some()).count();
    (kept.len(), have)
}

/// One chapter's audio in the voice folder, from the files that are there: a whole-chapter
/// file, or every page file (nothing when pages are missing, or a file cannot be read).
fn gather(b: &Book, voice_dir: &Path) -> Vec<ExportChapter> {
    let mut found: HashMap<(usize, Option<usize>), PathBuf> = HashMap::new();
    if let Ok(rd) = std::fs::read_dir(voice_dir) {
        for p in rd.flatten().map(|e| e.path()) {
            if p.extension().is_some_and(|e| e == "wav") {
                if let Some(k) = p.file_stem().and_then(|s| s.to_str()).and_then(crate::parse_unit_name) {
                    found.entry(k).or_insert(p);
                }
            }
        }
    }
    b.chapters
        .iter()
        .enumerate()
        .map(|(ch, c)| {
            let pages = c.pages.len();
            let mut files: Vec<PathBuf> = Vec::new();
            if let Some(f) = found.get(&(ch, None)) {
                files.push(f.clone()); // whole-chapter form, whatever the name
            } else if pages > 0 && (0..pages).all(|p| found.contains_key(&(ch, Some(p)))) {
                files = (0..pages).filter_map(|p| found.get(&(ch, Some(p)))).cloned().collect();
            }
            let mut audio = None;
            if !files.is_empty() {
                let mut fmt = WavFmt { rate: 0, channels: 0, bits: 0 };
                let mut samples = 0u64;
                let mut broken = false;
                for f in &files {
                    match wav_layout(f) {
                        Ok((wf, _, len)) if !broken && (fmt.rate == 0 || wf == fmt) && len % wf.block() == 0 => {
                            fmt = wf;
                            samples += len / wf.block();
                        }
                        _ => broken = true,
                    }
                }
                if broken {
                    samples = 0; // times are unknown; joining will refuse anyway
                }
                audio = Some(ChapterAudio { dur: samples as f32 / fmt.rate.max(1) as f32, fmt, samples, files });
            }
            ExportChapter { title: c.title.clone(), skipped: c.skipped(), audio }
        })
        .collect()
}

// ---------------- names ----------------

/// A name part safe in file names on Windows, macOS and Linux: reserved and control
/// characters replaced with "_", trailing dots and spaces dropped, Windows device names
/// (CON, COM1, ...) prefixed, at most 120 characters. Unicode letters stay as they are.
pub fn safe_name(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c })
        .collect();
    while s.ends_with('.') || s.ends_with(' ') {
        s.pop();
    }
    let devices = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9"];
    if devices.contains(&s.split('.').next().unwrap_or("").to_ascii_uppercase().as_str()) {
        s.insert(0, '_');
    }
    if s.chars().count() > 120 {
        s = s.chars().take(120).collect();
        while s.ends_with('.') || s.ends_with(' ') {
            s.pop();
        }
    }
    if s.is_empty() {
        s.push('_');
    }
    s
}

/// "<Author> - <Title>", or just the title when there is no author; cleaned for file names.
pub fn book_name(title: &str, author: &str) -> String {
    if author.is_empty() { safe_name(title) } else { safe_name(&format!("{author} - {title}")) }
}

/// The first free name for `path`: itself when nothing is there yet, otherwise
/// "name (2)", "name (3)" and so on before the extension. Folders count as taken too.
pub fn free_name(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    for i in 2.. {
        let p = path.with_file_name(format!("{stem} ({i}){ext}"));
        if !p.exists() {
            return p;
        }
    }
    unreachable!()
}

/// The file stem of exported chapter `n` (from 1) of `total`: "03 - Title", with the
/// width 100 or more exported chapters need.
fn chapter_stem(n: usize, total: usize, title: &str) -> String {
    let w = if total >= 100 { 3 } else { 2 };
    format!("{n:0w$} - {}", safe_name(title), w = w)
}

// ---------------- the runs ----------------

/// Why `dest` cannot be written: exports never write into the app's data folder, where
/// the library lives.
pub fn dest_refusal(dest: &Path, data: &Path) -> Option<String> {
    match (dest.canonicalize().ok(), data.canonicalize().ok()) {
        (Some(d), Some(lib)) if d.starts_with(&lib) => {
            Some("The destination is inside the app's data folder; choose a folder outside it.".into())
        }
        (None, _) => Some(format!("The destination folder {} cannot be opened.", dest.display())),
        _ => None,
    }
}

/// Write each chapter as one wav in "<dest>/<Book folder>/NN - <Chapter title>.wav",
/// numbered from 01 in book order. Every file is written to a temporary name first and
/// renamed when complete, so a cancel or failure leaves nothing half-written.
pub fn run_wav(plan: &ExportPlan, cancel: &AtomicBool, prog: &mut dyn FnMut(f32, &str)) -> Result<PathBuf, ExportErr> {
    if let Some(e) = dest_refusal(&plan.dest, &data_dir()) {
        return Err(ExportErr::Msg(e));
    }
    if plan.chapters.is_empty() {
        return Err(ExportErr::Msg("Nothing to export".into()));
    }
    let folder = free_name(&plan.dest.join(book_name(&plan.title, &plan.author)));
    std::fs::create_dir(&folder).map_err(|e| ExportErr::Msg(format!("Cannot create {}: {e}", folder.display())))?;
    let n = plan.chapters.len();
    let mut done = 0;
    for (i, c) in plan.chapters.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(cancel_wav(&folder, done));
        }
        prog(i as f32 / n as f32, &format!("{} · {} of {}", c.title, i + 1, n));
        let final_p = free_name(&folder.join(format!("{}.wav", chapter_stem(i + 1, n, &c.title))));
        let tmp = folder.join(format!("{}.part", final_p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()));
        if let Err(e) = join_wav(&c.files, &tmp, &c.title) {
            let _ = std::fs::remove_file(&tmp);
            let _ = std::fs::remove_dir(&folder);
            return Err(ExportErr::Msg(e));
        }
        if let Err(e) = std::fs::rename(&tmp, &final_p) {
            let _ = std::fs::remove_file(&tmp);
            let _ = std::fs::remove_dir(&folder);
            return Err(ExportErr::Msg(format!("Cannot write {}: {e}", final_p.display())));
        }
        done += 1;
    }
    prog(1.0, "");
    Ok(folder)
}

/// On cancel, drop the temporary files; the fresh book folder goes too while it is still
/// empty, and stays when it already holds complete chapters.
fn cancel_wav(folder: &Path, done: usize) -> ExportErr {
    if done == 0 {
        let _ = std::fs::remove_dir_all(folder);
    }
    ExportErr::Cancelled
}

/// The FFMETADATA chapter file handed to ffmpeg: one chapter per exported chapter, with
/// times in sample counts under the timebase 1/<sample rate>, and the escaped titles the
/// format requires.
fn ffmetadata(title: &str, author: &str, chs: &[PlannedChapter]) -> String {
    let esc = |s: &str| {
        let mut out = String::new();
        for c in s.chars() {
            match c {
                '\\' | '=' | ';' | '#' => {
                    out.push('\\');
                    out.push(c);
                }
                '\n' => out.push_str("\\n"),
                _ => out.push(c),
            }
        }
        out
    };
    let mut s = format!(";FFMETADATA1\ntitle={}", esc(title));
    if !author.is_empty() {
        s.push_str(&format!("\nartist={}", esc(author)));
    }
    let mut t = 0u64;
    for c in chs {
        s.push_str(&format!(
            "\n[CHAPTER]\nTIMEBASE=1/{}\nSTART={}\nEND={}\ntitle={}",
            c.fmt.rate,
            t,
            t + c.samples,
            esc(&c.title)
        ));
        t += c.samples;
    }
    s.push('\n');
    s
}

/// The concat list of a run's chapter wavs, written beside them and named by number.
fn concat_list(n: usize) -> String {
    let w = if n >= 100 { 3 } else { 2 };
    (1..=n).map(|i| format!("file '{i:0w$}.wav'\n", w = w)).collect()
}

/// Look for the ffmpeg program on the PATH (the exe name on Windows).
pub fn find_ffmpeg() -> Option<PathBuf> {
    let name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
    let sep = if cfg!(windows) { ';' } else { ':' };
    std::env::var_os("PATH").and_then(|path| {
        path.to_string_lossy()
            .split(sep)
            .map(|dir| Path::new(dir).join(name))
            .find(|p| p.is_file())
    })
}

/// Encode the chapters into "<dest>/<Book folder>.m4b": AAC at 64 kb/s mono, one chapter
/// mark per chapter, the book's title and author as metadata and its cover as cover art.
/// The chapter wavs, the concat list and the FFMETADATA file are built in a temporary
/// folder inside the destination, which is removed afterwards however the run ends.
pub fn run_m4b(plan: &ExportPlan, ffmpeg: Option<&Path>, cancel: &AtomicBool, prog: &mut dyn FnMut(f32, &str)) -> Result<PathBuf, ExportErr> {
    let Some(ffmpeg) = ffmpeg else { return Err(ExportErr::Msg("M4B export needs ffmpeg installed".into())) };
    if let Some(e) = dest_refusal(&plan.dest, &data_dir()) {
        return Err(ExportErr::Msg(e));
    }
    if plan.chapters.is_empty() {
        return Err(ExportErr::Msg("Nothing to export".into()));
    }
    if plan.chapters.iter().any(|c| c.fmt != plan.chapters[0].fmt) {
        return Err(ExportErr::Msg(
            "The chapters were generated with different audio formats, so they cannot go into one M4B. Export them as one WAV per chapter instead.".into(),
        ));
    }
    let final_p = free_name(&plan.dest.join(format!("{}.m4b", book_name(&plan.title, &plan.author))));
    let tmp = temp_dir(&plan.dest);
    let n = plan.chapters.len();
    let total: f32 = plan.chapters.iter().map(|c| c.dur).sum();
    let mut run = || -> Result<(), ExportErr> {
        std::fs::create_dir(&tmp).map_err(|e| ExportErr::Msg(format!("Cannot create {}: {e}", tmp.display())))?;
        // the chapters first, joined and numbered; progress runs to a fifth, the encode takes the rest
        for (i, c) in plan.chapters.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Err(ExportErr::Cancelled);
            }
            let w = if n >= 100 { 3 } else { 2 };
            prog(0.2 * i as f32 / n as f32, &format!("Preparing {} · {} of {}", c.title, i + 1, n));
            let f = tmp.join(format!("{:0w$}.wav", i + 1, w = w));
            join_wav(&c.files, &f, &c.title).map_err(ExportErr::Msg)?;
        }
        std::fs::write(tmp.join("list.txt"), concat_list(n)).map_err(|e| ExportErr::Msg(e.to_string()))?;
        std::fs::write(tmp.join("meta.txt"), ffmetadata(&plan.title, &plan.author, &plan.chapters)).map_err(|e| ExportErr::Msg(e.to_string()))?;
        let mut args: Vec<String> = ["-y", "-f", "concat", "-safe", "0", "-i", "list.txt", "-i", "meta.txt"]
            .iter().map(|s| s.to_string()).collect();
        if let Some(cover) = &plan.cover {
            args.extend(["-i".into(), cover.display().to_string()]);
        }
        args.push("-map".into());
        args.push("0:a".into());
        if plan.cover.is_some() {
            args.extend(["-map".into(), "2:v".into(), "-c:v".into(), "mjpeg".into(), "-disposition:v:0".into(), "attached_pic".into()]);
        }
        args.extend([
            "-map_metadata".into(), "1".into(), "-c:a".into(), "aac".into(), "-b:a".into(), "64k".into(),
            "-ac".into(), "1".into(), "-progress".into(), "pipe:1".into(), "-nostats".into(),
            "-loglevel".into(), "error".into(), "out.m4b".into(),
        ]);
        let mut child = Command::new(ffmpeg)
            .args(&args)
            .current_dir(&tmp)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| ExportErr::Msg(format!("Cannot run ffmpeg: {e}")))?;
        // progress lines on one thread, the error text on another, so neither pipe can fill
        let (tx, rx) = mpsc::channel::<String>();
        let out = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let err = child.stderr.take().unwrap();
        let err_h = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = BufReader::new(err).read_to_string(&mut s);
            s
        });
        loop {
            if cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ExportErr::Cancelled);
            }
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(line) => {
                    // out_time_us carries microseconds; out_time_ms does too, despite its name
                    let t = line
                        .strip_prefix("out_time_us=")
                        .or_else(|| line.strip_prefix("out_time_ms="))
                        .and_then(|v| v.trim().parse::<f64>().ok())
                        .map(|us| (us / 1e6) as f32)
                        .unwrap_or(0.0);
                    if t > 0.0 && total > 0.0 {
                        prog((0.2 + 0.8 * (t / total).min(1.0)).min(1.0), "Encoding…");
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        let status = child.wait().map_err(|e| ExportErr::Msg(e.to_string()))?;
        if !status.success() {
            let last = err_h.join().map_or(String::new(), |s| s).lines().rev().find(|l| !l.trim().is_empty()).map(str::to_string);
            return Err(ExportErr::Msg(match last {
                Some(l) => format!("ffmpeg failed: {l}"),
                None => format!("ffmpeg failed: {}", status.code().map_or("killed".into(), |c| format!("exit {c}"))),
            }));
        }
        std::fs::rename(tmp.join("out.m4b"), &final_p).map_err(|e| ExportErr::Msg(format!("Cannot write {}: {e}", final_p.display())))?;
        Ok(())
    };
    let r = run();
    let _ = std::fs::remove_dir_all(&tmp);
    r?;
    prog(1.0, "");
    Ok(final_p)
}

/// A free temporary folder inside the destination for the M4B build.
fn temp_dir(dest: &Path) -> PathBuf {
    let mut p = dest.join(".inkvoice-export");
    let mut i = 1;
    while p.exists() {
        i += 1;
        p = dest.join(format!(".inkvoice-export-{i}"));
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book(n: usize, pages: usize) -> Book {
        Book {
            id: "quiet_harbor-9f6689e3".into(),
            title: "Quiet Harbor".into(),
            author: String::new(),
            source: String::new(),
            chapters: (0..n)
                .map(|i| crate::book::Chapter::new(format!("Chapter {i}"), vec!["page text".into(); pages]))
                .collect(),
            pos: (0, 0),
            opened: 0,
            listen: None,
        }
    }

    /// A real PCM wav of `samples` silent samples, with the chapter key in its name.
    fn wav(dir: &Path, key: &str, rate: u32, samples: usize) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let fmt = WavFmt { rate, channels: 1, bits: 16 };
        let mut b = wav_header(fmt, (samples * 2) as u64).unwrap();
        b.extend(std::iter::repeat(0u8).take(samples * 2));
        let p = dir.join(format!("{key}.wav"));
        std::fs::write(&p, b).unwrap();
        p
    }

    fn plan(b: &Book, dir: &Path, dest: &Path, include_skipped: bool) -> ExportPlan {
        let chapters = gather(b, dir);
        let sel = selected(&chapters, include_skipped);
        ExportPlan {
            title: b.title.clone(),
            author: b.author.clone(),
            cover: None,
            dest: dest.to_path_buf(),
            chapters: sel
                .iter()
                .map(|&i| {
                    let a = chapters[i].audio.as_ref().unwrap();
                    PlannedChapter { title: chapters[i].title.clone(), files: a.files.clone(), fmt: a.fmt, samples: a.samples, dur: a.dur }
                })
                .collect(),
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("inkvoice-export-test-{tag}"));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn listing(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    /// Which chapters are exported, and how they are numbered: skipped sections are left
    /// out unless asked for, chapters without audio always are, numbering follows the
    /// exported order and widens at 100.
    #[test]
    fn selects_and_numbers_chapters() {
        let root = scratch("select");
        let mut b = book(6, 1);
        b.chapters[0].skip = Some(true); // skipped front matter with audio
        b.chapters[3].skip = Some(true); // skipped back matter without audio
        let dir = root.join("audio/phonon-everly");
        let k = |ch: usize| format!("0{ch}_Chapter_{ch}_c000{ch}");
        wav(&dir, &k(0), 24000, 12000);
        wav(&dir, &k(1), 24000, 24000);
        // chapter 2 has no audio
        wav(&dir, &k(4), 24000, 24000);
        wav(&dir, &k(5), 24000, 6000);
        let chapters = gather(&b, &dir);
        assert_eq!(counts(&chapters, false), (4, 3)); // four kept, three of them ready
        assert_eq!(selected(&chapters, false), vec![1, 4, 5]);
        assert_eq!(selected(&chapters, true), vec![0, 1, 4, 5]);
        let dest = root.join("out");
        std::fs::create_dir_all(&dest).unwrap();
        let cancel = AtomicBool::new(false);
        let folder = run_wav(&plan(&b, &dir, &dest, false), &cancel, &mut |_, _| {}).unwrap();
        assert_eq!(folder, dest.join("Quiet Harbor"));
        assert_eq!(listing(&folder), vec!["01 - Chapter 1.wav", "02 - Chapter 4.wav", "03 - Chapter 5.wav"]);
        // a second export never overwrites: the folder name gets a suffix
        let again = run_wav(&plan(&b, &dir, &dest, false), &cancel, &mut |_, _| {}).unwrap();
        assert_eq!(again, dest.join("Quiet Harbor (2)"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Page-mode chapters join into one wav: one header, the sample data of all pages in
    /// order. A chapter with only some pages generated has no audio, and pages of mixed
    /// formats are refused with a clear message.
    #[test]
    fn joins_page_files() {
        let root = scratch("pages");
        let b = book(2, 3);
        let dir = root.join("audio/phonon-everly");
        let p = |c: usize, pg: usize| format!("0{c}_Chapter_{c}_c000{c}_p000{pg}");
        wav(&dir, &p(0, 0), 24000, 100);
        wav(&dir, &p(0, 1), 24000, 250);
        wav(&dir, &p(0, 2), 24000, 50);
        wav(&dir, &p(1, 0), 24000, 80);
        // chapter 1 keeps a page missing
        let chapters = gather(&b, &dir);
        assert_eq!(selected(&chapters, false), vec![0]);
        let a = &chapters[0].audio.as_ref().unwrap();
        assert_eq!((a.samples, a.fmt.rate), (400, 24000));
        let dest = root.join("out");
        std::fs::create_dir_all(&dest).unwrap();
        let cancel = AtomicBool::new(false);
        let folder = run_wav(&plan(&b, &dir, &dest, false), &cancel, &mut |_, _| {}).unwrap();
        let joined = std::fs::read(&folder.join("01 - Chapter 0.wav")).unwrap();
        let rate = u32::from_le_bytes(joined[24..28].try_into().unwrap());
        let data = u32::from_le_bytes(joined[40..44].try_into().unwrap());
        assert_eq!((rate, data), (24000, 800)); // 400 samples, two bytes each
        // mixed formats refuse the export
        wav(&dir, &p(0, 2), 22050, 50);
        let r = run_wav(&plan(&b, &dir, &dest, false), &cancel, &mut |_, _| {});
        assert!(matches!(&r, Err(ExportErr::Msg(m)) if m.contains("mixed audio formats")), "{r:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Every name rule: reserved characters, control characters, trailing dots and
    /// spaces, Windows device names, the 120-character cap, and Unicode kept.
    #[test]
    fn cleans_file_names() {
        assert_eq!(safe_name("What? A <quiet> harbor: part 1/2"), "What_ A _quiet_ harbor_ part 1_2");
        assert_eq!(safe_name("Two\u{1}lines"), "Two_lines"); // the control character becomes _
        assert_eq!(safe_name("Trailing dots... "), "Trailing dots");
        assert_eq!(safe_name("aux"), "_aux");
        assert_eq!(safe_name("COM1.notes.txt"), "_COM1.notes.txt");
        assert_eq!(safe_name("lpt9"), "_lpt9");
        let long: String = "Nguyễn Đình x".repeat(20);
        assert_eq!(safe_name(&long).chars().count(), 120);
        assert_eq!(safe_name("Nguyễn Đình"), "Nguyễn Đình");
        assert_eq!(safe_name("«???»"), "«___»"); // Unicode punctuation stays, ? goes
        assert_eq!(safe_name("..."), "_"); // nothing is left after the rules
        assert_eq!(book_name("The Salt Road", "A. B. Carter"), "A. B. Carter - The Salt Road");
        assert_eq!(book_name("The Salt Road", ""), "The Salt Road");
    }

    /// A taken name is never overwritten: the next free "name (n)" form is used.
    #[test]
    fn frees_taken_names() {
        let root = scratch("free");
        std::fs::write(root.join("01 - A.wav"), b"x").unwrap();
        std::fs::create_dir(root.join("Quiet Harbor")).unwrap();
        assert_eq!(free_name(&root.join("01 - A.wav")), root.join("01 - A (2).wav"));
        std::fs::write(root.join("01 - A (2).wav"), b"x").unwrap();
        assert_eq!(free_name(&root.join("01 - A.wav")), root.join("01 - A (3).wav"));
        assert_eq!(free_name(&root.join("Quiet Harbor")), root.join("Quiet Harbor (2)"));
        assert_eq!(free_name(&root.join("New one.wav")), root.join("New one.wav"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The FFMETADATA text for three chapters: times in the declared timebase, titles
    /// and book metadata escaped the way the format reads them back.
    #[test]
    fn writes_ffmetadata() {
        let ch = |title: &str, samples: u64| PlannedChapter {
            title: title.into(),
            files: vec![],
            fmt: WavFmt { rate: 24000, channels: 1, bits: 16 },
            samples,
            dur: samples as f32 / 24000.0,
        };
        let m = ffmetadata(
            "Quiet Harbor #2",
            "A. B. Carter",
            &[ch("First crossing", 24000), ch("Second = part; two", 12000), ch("Third\\light", 6000)],
        );
        assert_eq!(
            m,
            ";FFMETADATA1\ntitle=Quiet Harbor \\#2\nartist=A. B. Carter\n\
             [CHAPTER]\nTIMEBASE=1/24000\nSTART=0\nEND=24000\ntitle=First crossing\n\
             [CHAPTER]\nTIMEBASE=1/24000\nSTART=24000\nEND=36000\ntitle=Second \\= part\\; two\n\
             [CHAPTER]\nTIMEBASE=1/24000\nSTART=36000\nEND=42000\ntitle=Third\\\\light\n"
        );
        assert_eq!(concat_list(3), "file '01.wav'\nfile '02.wav'\nfile '03.wav'\n");
    }

    /// The Export window's chapter duration is the wav's own duration, and the chapter
    /// times in the FFMETADATA come from the same sample counts.
    #[test]
    fn chapter_duration_matches_wav() {
        let root = scratch("dur");
        let b = book(2, 1);
        let dir = root.join("audio/phonon-everly");
        wav(&dir, "00_Chapter_0_c0000", 24000, 24000); // one second
        wav(&dir, "01_Chapter_1_c0001", 24000, 7200); // 0.3 s
        let chapters = gather(&b, &dir);
        let planned: Vec<PlannedChapter> = selected(&chapters, false)
            .into_iter()
            .map(|i| {
                let a = chapters[i].audio.as_ref().unwrap();
                assert_eq!(a.dur, crate::player::wav_duration(&a.files[0]).unwrap(), "{}", a.files[0].display());
                PlannedChapter { title: chapters[i].title.clone(), files: a.files.clone(), fmt: a.fmt, samples: a.samples, dur: a.dur }
            })
            .collect();
        // the chapter times are the cumulative sample counts the durations came from
        let m = ffmetadata("Quiet Harbor", "", &planned);
        assert!(m.contains("[CHAPTER]\nTIMEBASE=1/24000\nSTART=0\nEND=24000\n"), "{m}");
        assert!(m.contains("[CHAPTER]\nTIMEBASE=1/24000\nSTART=24000\nEND=31200\n"), "{m}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A destination inside the app's data folder is refused; one beside it is fine.
    #[test]
    fn refuses_dest_inside_data() {
        let root = scratch("dest");
        let data = root.join("data");
        std::fs::create_dir_all(data.join("books")).unwrap();
        assert!(dest_refusal(&data, &data).is_some());
        assert!(dest_refusal(&data.join("books"), &data).is_some());
        assert!(dest_refusal(&data.join("books/x/.."), &data).is_some());
        assert_eq!(dest_refusal(&root, &data), None);
        assert!(dest_refusal(&root.join("missing"), &data).is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A cancelled export leaves nothing behind: no chapter files, no temporary files,
    /// and no empty book folder.
    #[test]
    fn cancelled_export_writes_nothing() {
        let root = scratch("cancel");
        let b = book(2, 1);
        let dir = root.join("audio/phonon-everly");
        wav(&dir, "00_Chapter_0_c0000", 24000, 100);
        wav(&dir, "01_Chapter_1_c0001", 24000, 100);
        let dest = root.join("out");
        std::fs::create_dir_all(&dest).unwrap();
        let cancel = AtomicBool::new(true);
        assert!(matches!(run_wav(&plan(&b, &dir, &dest, false), &cancel, &mut |_, _| {}), Err(ExportErr::Cancelled)));
        assert_eq!(listing(&dest), Vec::<String>::new());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The real ffmpeg on three one-second chapters: the M4B carries one chapter mark
    /// per chapter with its title, the book's metadata and about the right duration.
    /// Skips itself quietly when no ffmpeg is on the PATH (CI machines may lack it).
    #[test]
    fn m4b_chapters_with_real_ffmpeg() {
        let Some(ffmpeg) = find_ffmpeg() else { return };
        let root = scratch("m4b");
        let mut b = book(3, 1);
        b.chapters[0].title = "First crossing".into();
        b.chapters[1].title = "Second letter".into();
        b.chapters[2].title = "Third light".into();
        let dir = root.join("audio/phonon-everly");
        for i in 0..3 {
            wav(&dir, &format!("0{i}_c000{i}"), 24000, 24000);
        }
        // a one-pixel cover, so the cover art path runs too
        let cover = root.join("cover.png");
        std::fs::write(&cover, &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00,
            0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78,
            0xda, 0x63, 0x64, 0x60, 0xf8, 0xff, 0x9f, 0x00, 0x01, 0x01, 0x01, 0x00, 0x86, 0xa8, 0x22, 0xc1, 0x00, 0x00, 0x00, 0x00, 0x49,
            0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ]).unwrap();
        let mut p = plan(&b, &dir, &root, false);
        p.cover = Some(cover);
        let cancel = AtomicBool::new(false);
        let out = run_m4b(&p, Some(&ffmpeg), &cancel, &mut |_, _| {}).unwrap();
        assert_eq!(out, root.join("Quiet Harbor.m4b"));
        // no temporary folder stays behind
        assert!(!root.join(".inkvoice-export").exists());
        // what ffmpeg itself reports about the file
        let probe = Command::new(&ffmpeg).arg("-i").arg(&out).output().unwrap();
        let info = String::from_utf8_lossy(&probe.stderr).into_owned();
        for t in ["First crossing", "Second letter", "Third light"] {
            assert!(info.contains(&format!("title           : {t}")), "missing chapter {t} in\n{info}");
        }
        assert!(info.contains("Duration: 00:00:03.0"), "duration should be about three seconds\n{info}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
