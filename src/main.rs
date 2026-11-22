#![cfg_attr(windows, windows_subsystem = "windows")] // no console window on Windows
mod book;
mod config;
mod export;
mod installer;
mod player;
mod sections;
mod theme;
mod tts;
mod ui;

use book::{data_dir, Book};
use config::Config;
use eframe::egui;
use player::{Player, Unit};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Instant;

const WORKERS: &[(&str, &str)] = &[
    ("common.py", include_str!("../workers/common.py")),
    ("aligner.py", include_str!("../workers/aligner.py")),
    ("normalizer.py", include_str!("../workers/normalizer.py")),
    ("spoken_rules.py", include_str!("../workers/spoken_rules.py")),
    ("phonon_worker.py", include_str!("../workers/phonon_worker.py")),
    ("woof_worker.py", include_str!("../workers/woof_worker.py")),
    ("kokoro_worker.py", include_str!("../workers/kokoro_worker.py")),
    ("qwen3_worker.py", include_str!("../workers/qwen3_worker.py")),
];

#[derive(PartialEq, Clone, Copy)]
enum View {
    Library,
    Reader,
}

/// A generated audio file on disk, for the library's audio list.
#[derive(Clone)]
struct AudioItem {
    profile: String,
    path: PathBuf,
    unit: Unit,
    label: String,
    duration: f32,
    /// bytes on disk (the wav alone)
    size: u64,
    /// unix seconds of the file's modification time
    modified: u64,
}

/// A leftover file under a book's audio folder, offered for clean-up.
#[derive(Clone)]
struct JunkFile {
    path: PathBuf,
    size: u64,
}

/// How audio files are deleted: to the system trash in the app, plain removal in tests.
trait Remover {
    fn remove(&mut self, path: &Path) -> Result<(), String>;
    /// A group in one operation when the platform offers it; by default one by one.
    fn remove_all(&mut self, paths: &[PathBuf]) -> Result<(), String> {
        for p in paths {
            self.remove(p)?;
        }
        Ok(())
    }
}

/// The app's remover: every file goes to the Recycle Bin, where it can be restored.
struct RecycleBin;
impl Remover for RecycleBin {
    fn remove(&mut self, path: &Path) -> Result<(), String> {
        trash::delete(path).map_err(|e| e.to_string())
    }
    fn remove_all(&mut self, paths: &[PathBuf]) -> Result<(), String> {
        trash::delete_all(paths.to_vec()).map_err(|e| e.to_string())
    }
}

struct Job {
    path: PathBuf,
    progress: f32,
    started: Instant,
}

/// Audio being streamed into the player while it is generated.
struct Feed {
    out: PathBuf,
    pcm: PathBuf,
    read: u64,
}

pub const SPEEDS: &[f32] = &[0.75, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0];
/// The text of a voice preview (own words, ~10 s spoken).
const PREVIEW_TEXT: &str = "The lamps along the river had already been lit when she reached the bridge. \
                            “You came back,” he said quietly. She smiled, and for a moment neither of them spoke.";

/// How an engine install ended (shown in the install window until dismissed).
pub enum InstallOutcome {
    Ok,
    Failed(String),
}

pub struct InstallUi {
    pub plan: Vec<installer::StepInfo>,
    pub step: usize,
    pub frac: f32,
    pub detail: String,
    /// overall progress, never decreasing
    pub target: f32,
    /// what the bar shows (eases toward `target`)
    pub shown: f32,
    pub started: Instant,
    pub visible: bool,
    pub result: Option<(String, InstallOutcome, Instant)>,
}

struct Status {
    text: String,
    error: bool,
    at: Instant,
}

/// The reader's "Set pronunciation" window for one word (opened by right-clicking it).
#[derive(Clone)]
struct PronWin {
    word: String,
    /// committed draft of the spoken form; the field commits on focus loss
    say: String,
    saved: bool,
    err: Option<String>,
}

/// Chapters of the open book that contain a word whose pronunciation just changed and
/// already have audio; regenerating them is offered once, next to the change.
#[derive(Clone)]
struct RegenOffer {
    word: String,
    chapters: Vec<usize>,
    /// shown in the reader's Set pronunciation window rather than the Settings list
    reader: bool,
}

pub struct App {
    cfg: Config,
    lib: Vec<Book>,
    /// book open in the reader
    open: Option<String>,
    /// book selected in the library (details panel)
    sel: Option<String>,
    ch: usize,
    pg: usize,
    view: View,
    tts: tts::Tts,
    /// queued + running jobs: wav path -> label
    pending: HashMap<PathBuf, String>,
    job: Option<Job>,
    autoplay: Option<PathBuf>,
    import_rx: Receiver<Result<Book, String>>,
    import_tx: Sender<Result<Book, String>>,
    importing: bool,
    /// the Export window for one book and voice (None = closed)
    export: Option<export::ExportWin>,
    export_rx: Receiver<export::Msg>,
    export_tx: Sender<export::Msg>,
    /// last export destination, remembered for the session
    export_dir: Option<PathBuf>,
    player: Option<Player>,
    /// ready wav files of the open book for the current voice profile
    have: HashSet<PathBuf>,
    /// their unit keys, so a unit is found whatever name form its file is in
    have_keys: AudioKeys,
    lib_audio: HashMap<String, Vec<AudioItem>>,
    /// leftover files per book, for the audio manager's clean-up offer
    lib_junk: HashMap<String, Vec<JunkFile>>,
    status: Option<Status>,
    ctx: egui::Context,
    scroll_top: bool,
    confirm_remove: Option<String>,
    /// a pending "delete all audio" confirmation (counts gathered when it was opened)
    confirm_del_audio: Option<ConfirmDel>,
    covers: HashMap<String, Option<egui::TextureHandle>>,
    search: String,
    show_settings: bool,
    worker_dirty: bool,
    follow_target: Option<(usize, usize)>,
    seek_drag: Option<f32>,
    sidebar: bool,
    /// last highlighted word we scrolled to
    last_word: Option<usize>,
    /// voice preview being generated
    preview: Option<PathBuf>,
    feed: Option<Feed>,
    /// last "play now" request, to ignore accidental repeats
    last_stream: Option<(PathBuf, usize, Instant)>,
    /// voice engine being installed + its current step / progress
    install: Option<installer::Install>,
    inst: InstallUi,
    installed: HashSet<String>,
    /// engines that are not installed but still have folders on disk, so Remove stays offered
    leftover: HashSet<String>,
    confirm_uninstall: Option<String>,
    /// when the listening position was last written to progress.json
    listen_saved: Instant,
    /// the reader's "Set pronunciation" window (None = closed)
    pron_win: Option<PronWin>,
    /// word under the pointer when the reader's context menu opened
    pron_ask: Option<String>,
    /// chapters offered for regeneration after a pronunciation changed
    regen_offer: Option<RegenOffer>,
    /// a rejected entry: None = the add row, Some(row) = that list row
    pron_err: Option<(Option<usize>, String)>,
    /// the pronunciation table, kept in memory; re-read when the file's state changes
    pron: config::Pronounce,
    pron_state: Option<(std::time::SystemTime, u64)>,
}

/// A "delete all audio" confirmation target: one voice folder, or every voice of a book.
#[derive(Clone, PartialEq)]
enum DelAll {
    Voice(String),
    Book,
}

/// A pending "delete all audio" confirmation: what would go, counted once when it was
/// asked for, so drawing it never touches the disk.
#[derive(Clone)]
struct ConfirmDel {
    id: String,
    scope: DelAll,
    files: usize,
    bytes: u64,
}

impl App {
    fn new(cc: &eframe::CreationContext, open_file: Option<PathBuf>) -> Self {
        theme::install(&cc.egui_ctx);
        install_workers();
        let cfg = Config::load();
        cfg.save();
        let (import_tx, import_rx) = channel();
        let (export_tx, export_rx) = channel();
        let tts = tts::Tts::start(cfg.worker_command(), cfg.normalize_key(), cc.egui_ctx.clone());
        let mut app = Self {
            lib: migrate_startup(),
            open: None,
            sel: None,
            ch: 0,
            pg: 0,
            view: View::Library,
            tts,
            pending: HashMap::new(),
            job: None,
            autoplay: None,
            import_rx,
            import_tx,
            importing: false,
            export: None,
            export_rx,
            export_tx,
            export_dir: None,
            player: Player::new(cfg.volume, cfg.speed),
            status: None,
            ctx: cc.egui_ctx.clone(),
            scroll_top: false,
            have: HashSet::new(),
            have_keys: HashMap::new(),
            lib_audio: HashMap::new(),
            confirm_remove: None,
            covers: HashMap::new(),
            search: String::new(),
            show_settings: false,
            worker_dirty: false,
            follow_target: None,
            seek_drag: None,
            lib_junk: HashMap::new(),
            confirm_del_audio: None,
            sidebar: true,
            last_word: None,
            preview: None,
            feed: None,
            last_stream: None,
            install: None,
            inst: InstallUi {
                plan: Vec::new(),
                step: 0,
                frac: 0.0,
                detail: String::new(),
                target: 0.0,
                shown: 0.0,
                started: Instant::now(),
                visible: false,
                result: None,
            },
            installed: installer::SPECS.iter().filter(|s| installer::is_installed(s.name)).map(|s| s.name.to_string()).collect(),
            leftover: leftover_engines(),
            confirm_uninstall: None,
            listen_saved: Instant::now(),
            pron_win: None,
            pron_ask: None,
            regen_offer: None,
            pron_err: None,
            pron: config::Pronounce::load(),
            pron_state: config::Pronounce::file_state(),
            cfg,
        };
        app.sel = app.lib.first().map(|b| b.id.clone());
        // use an installed engine if the selected one isn't installed
        if !app.installed.contains(&app.cfg.engine) {
            if let Some(e) = installer::SPECS.iter().find(|s| app.installed.contains(s.name)) {
                app.cfg.engine = e.name.to_string();
                app.cfg.save();
                app.tts = tts::Tts::start(app.cfg.worker_command(), app.cfg.normalize_key(), app.ctx.clone());
            }
        }
        app.refresh_lib_audio();
        if app.player.is_none() {
            app.notify_err("No audio output device found");
        }
        if let Some(p) = open_file {
            app.import_path(p);
        }
        app
    }

    // ---------- lookups ----------

    fn book(&self, id: &str) -> Option<&Book> {
        self.lib.iter().find(|b| b.id == id)
    }
    fn open_book(&self) -> Option<&Book> {
        self.open.as_deref().and_then(|id| self.book(id))
    }
    fn open_book_mut(&mut self) -> Option<&mut Book> {
        let id = self.open.clone()?;
        self.lib.iter_mut().find(|b| b.id == id)
    }

    fn audio_path(&self, b: &Book, ch: usize, pg: Option<usize>) -> PathBuf {
        b.dir().join("audio").join(self.cfg.profile()).join(unit_stem(b, ch, pg)).with_extension("wav")
    }
    fn unit_path(&self, u: &Unit) -> Option<PathBuf> {
        Some(self.audio_path(self.book(&u.book)?, u.ch, u.pg))
    }
    fn unit_label(&self, u: &Unit) -> String {
        let Some(b) = self.book(&u.book) else { return String::new() };
        let Some(c) = b.chapters.get(u.ch) else { return String::new() };
        match u.pg {
            None => c.title.clone(),
            Some(p) => format!("{} · page {}", c.title, p + 1),
        }
    }
    fn unit_text(&self, u: &Unit) -> Option<String> {
        let b = self.book(&u.book)?;
        Some(match u.pg {
            None => b.chapter_text(u.ch),
            Some(p) => b.chapters.get(u.ch)?.pages.get(p)?.clone(),
        })
    }
    /// The unit the reader is on, in the current voice mode.
    fn reader_unit(&self) -> Option<Unit> {
        Some(Unit { book: self.open.clone()?, ch: self.ch, pg: self.cfg.page_mode.then_some(self.pg) })
    }
    fn next_unit(&self, u: &Unit) -> Option<Unit> {
        self.book(&u.book).and_then(|b| step_unit(b, u, true))
    }
    fn prev_unit(&self, u: &Unit) -> Option<Unit> {
        self.book(&u.book).and_then(|b| step_unit(b, u, false))
    }
    /// The audio file that exists for this unit's key, whichever name form it is in.
    fn existing_audio(&self, path: &Path) -> Option<PathBuf> {
        if self.have.contains(path) || path.exists() {
            return Some(path.to_path_buf());
        }
        known_audio(&self.have_keys, path)
    }
    fn is_ready(&self, path: &Path) -> bool {
        self.existing_audio(path).is_some()
    }

    // ---------- status ----------

    fn notify(&mut self, text: impl Into<String>) {
        self.status = Some(Status { text: text.into(), error: false, at: Instant::now() });
    }
    fn notify_err(&mut self, text: impl Into<String>) {
        self.status = Some(Status { text: text.into(), error: true, at: Instant::now() });
    }

    // ---------- library ----------

    fn refresh_have(&mut self) {
        self.have.clear();
        self.have_keys.clear();
        let Some(b) = self.open_book() else { return };
        let dir = b.dir().join("audio").join(self.cfg.profile());
        if let Ok(rd) = std::fs::read_dir(dir) {
            let mut paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "wav")).collect();
            paths.sort(); // the readable name form sorts before the old bare "c..." form
            for p in paths {
                if let Some(k) = audio_key(&p) {
                    self.have_keys.entry(k).or_insert_with(|| p.clone());
                }
                self.have.insert(p);
            }
        }
    }

    fn refresh_lib_audio(&mut self) {
        let busy: HashSet<PathBuf> = self.pending.keys().cloned().collect();
        let (mut audio, mut junk) = (HashMap::new(), HashMap::new());
        for b in &self.lib {
            audio.insert(b.id.clone(), scan_audio(b));
            junk.insert(b.id.clone(), scan_leftovers(b, &busy));
        }
        self.lib_audio = audio;
        self.lib_junk = junk;
    }

    fn open_reader(&mut self, id: &str) {
        let Some(b) = self.lib.iter_mut().find(|b| b.id == id) else { return };
        b.opened = book::now_secs();
        b.save_progress();
        let (ch, pg) = b.pos;
        self.open = Some(id.to_string());
        self.sel = Some(id.to_string());
        self.ch = ch;
        self.pg = pg;
        self.scroll_top = true;
        self.follow_target = None;
        self.refresh_have();
        self.view = View::Reader;
    }

    fn go(&mut self, ch: usize, pg: usize) {
        self.ch = ch;
        self.pg = pg;
        self.scroll_top = true;
        if let Some(b) = self.open_book_mut() {
            b.pos = (ch, pg);
            b.save_progress();
        }
    }
    fn next_page(&mut self) {
        let Some(b) = self.open_book() else { return };
        if self.pg + 1 < b.chapters[self.ch].pages.len() {
            self.go(self.ch, self.pg + 1);
        } else if self.ch + 1 < b.chapters.len() {
            self.go(self.ch + 1, 0);
        }
    }
    fn prev_page(&mut self) {
        if self.pg > 0 {
            self.go(self.ch, self.pg - 1);
        } else if self.ch > 0 {
            let last = self.open_book().unwrap().chapters[self.ch - 1].pages.len() - 1;
            self.go(self.ch - 1, last);
        }
    }

    fn remove_book(&mut self, id: &str) {
        let Some(i) = self.lib.iter().position(|b| b.id == id) else { return };
        let b = self.lib.remove(i);
        if self.player.as_ref().and_then(|p| p.now.as_ref()).is_some_and(|n| n.unit.book == b.id) {
            if let Some(p) = &mut self.player { p.stop(); }
        }
        if self.pending.keys().any(|p| p.starts_with(b.dir())) {
            self.cancel_generation();
        }
        let _ = std::fs::remove_dir_all(b.dir());
        self.lib_audio.remove(&b.id);
        self.lib_junk.remove(&b.id);
        self.covers.remove(&b.id);
        if self.open.as_deref() == Some(id) {
            self.open = None;
        }
        self.sel = self.lib.first().map(|b| b.id.clone());
        self.confirm_remove = None;
        self.notify(format!("Removed “{}”", b.title));
    }

    // ---------- audio management ----------

    /// Re-read the open book's ready files and one book's library entries after audio files
    /// changed on disk.
    fn refresh_audio_after(&mut self, id: &str) {
        self.refresh_have();
        if let Some(b) = self.book(id) {
            let busy: HashSet<PathBuf> = self.pending.keys().cloned().collect();
            let (a, j) = (scan_audio(b), scan_leftovers(b, &busy));
            self.lib_audio.insert(id.to_string(), a);
            self.lib_junk.insert(id.to_string(), j);
        }
    }

    /// Send `paths` (each wav with its timing file) to the Recycle Bin. Playback of those
    /// files stops and their queued or running generations are cancelled first, so deleted
    /// audio cannot reappear; a file that cannot be moved is left where it is.
    fn trash_audio(&mut self, id: &str, paths: &[PathBuf]) -> Deleted {
        let Some(audio_dir) = self.book(id).map(|b| b.dir().join("audio")) else {
            return Deleted { moved: 0, failed: Some((PathBuf::new(), "book not found".into())) };
        };
        for p in paths {
            halt_unit_audio(&mut self.player, &mut self.feed, &mut self.pending, &mut self.job, &mut self.autoplay, p, &self.tts);
        }
        delete_audio_files(paths, &audio_dir, &mut RecycleBin)
    }

    /// Delete one audio file and its timing file (a row of the details panel's audio list).
    fn delete_audio_file(&mut self, path: &Path) {
        let Some(id) = book_id_of(path) else { return };
        let r = self.trash_audio(&id, &[path.to_path_buf()]);
        self.report_delete(&id, r, "Moved to the Recycle Bin");
    }

    /// Delete one section's audio for the current voice (the reader's "Delete audio"), in
    /// whatever name form its files are in.
    fn delete_section_audio(&mut self, id: &str, ch: usize) {
        let Some(b) = self.book(id) else { return };
        let dir = b.dir().join("audio").join(self.cfg.profile());
        let title = b.chapters.get(ch).map(|c| c.title.clone()).unwrap_or_default();
        let files = section_audio_paths(b, &dir, ch, self.cfg.page_mode);
        let r = self.trash_audio(id, &files);
        self.report_delete(id, r, &format!("Deleted the audio of {title}"));
    }

    /// Open the "delete all audio" confirmation, counting once what it covers.
    fn ask_del_all(&mut self, id: &str, scope: DelAll) {
        let Some(b) = self.book(id) else { return };
        let dir = match &scope {
            DelAll::Voice(p) => b.dir().join("audio").join(p),
            DelAll::Book => b.dir().join("audio"),
        };
        let files = folder_files(&dir);
        let bytes = files.iter().filter_map(|f| std::fs::metadata(f).ok()).map(|m| m.len()).sum();
        self.confirm_del_audio = Some(ConfirmDel { id: id.to_string(), scope, files: files.len(), bytes });
    }

    /// Delete every audio and timing file of one voice in this book (after confirmation).
    /// Generations still writing in the folder are stopped first, their temp files included.
    fn delete_voice_audio(&mut self, id: &str, profile: &str) {
        self.confirm_del_audio = None;
        let Some(b) = self.book(id) else { return };
        let dir = b.dir().join("audio").join(profile);
        let audio = b.dir().join("audio");
        let r = clear_audio_dir(&dir, &audio, &mut self.player, &mut self.feed, &mut self.pending, &mut self.job, &mut self.autoplay, &self.tts, &mut RecycleBin);
        let _ = std::fs::remove_dir(&dir); // empty now
        self.report_delete(id, r, &format!("Deleted all {} audio", config::profile_label(profile)));
    }

    /// Delete every voice's audio in this book (after confirmation). The book itself, its
    /// text, cover and reading place stay.
    fn delete_book_audio(&mut self, id: &str) {
        self.confirm_del_audio = None;
        let Some(b) = self.book(id) else { return };
        let root = b.dir().join("audio");
        let r = clear_audio_dir(&root, &root, &mut self.player, &mut self.feed, &mut self.pending, &mut self.job, &mut self.autoplay, &self.tts, &mut RecycleBin);
        if let Ok(rd) = std::fs::read_dir(&root) {
            for d in rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
                let _ = std::fs::remove_dir(d); // empty voice folders
            }
        }
        let _ = std::fs::remove_dir(&root);
        self.report_delete(id, r, "Deleted all audio of this book");
    }

    /// Delete exactly the leftover files of a book (see `scan_leftovers`).
    fn clean_leftovers(&mut self, id: &str) {
        let Some(b) = self.book(id) else { return };
        let busy: HashSet<PathBuf> = self.pending.keys().cloned().collect();
        let junk = scan_leftovers(b, &busy);
        if junk.is_empty() {
            self.notify("No leftover files");
            return;
        }
        let paths: Vec<PathBuf> = junk.into_iter().map(|j| j.path).collect();
        let r = self.trash_audio(id, &paths);
        self.report_delete(id, r, "Cleaned up the leftover files");
    }

    /// Tell the user how a deletion went: what moved, and the first file that could not.
    fn report_delete(&mut self, id: &str, r: Deleted, done: &str) {
        match r.failed {
            None => self.notify(format!(
                "{done}: {} {} to the Recycle Bin",
                r.moved,
                if r.moved == 1 { "file moved" } else { "files moved" }
            )),
            Some((p, e)) => {
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.notify_err(format!(
                    "{done}: {} {} to the Recycle Bin, {name} could not be moved ({e})",
                    r.moved,
                    if r.moved == 1 { "file moved" } else { "files moved" }
                ));
            }
        }
        self.refresh_audio_after(id);
    }

    fn import_dialog(&mut self) {
        if self.importing {
            return;
        }
        self.importing = true;
        let (tx, ctx) = (self.import_tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let r = match rfd::FileDialog::new().add_filter("Books", &["epub", "pdf"]).pick_file() {
                Some(p) => book::import(&p).map_err(|e| e.to_string()),
                None => Err(String::new()),
            };
            let _ = tx.send(r);
            ctx.request_repaint();
        });
    }
    fn import_path(&mut self, p: PathBuf) {
        self.importing = true;
        let (tx, ctx) = (self.import_tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(book::import(&p).map_err(|e| e.to_string()));
            ctx.request_repaint();
        });
    }

    // ---------- export ----------

    /// Open the Export window for one book and voice, gathering its chapter counts once.
    fn open_export(&mut self, id: &str, profile: &str) {
        let Some(b) = self.book(id) else { return };
        let dest = self.export_dir.clone();
        self.export = Some(export::ExportWin::new(b, profile, &b.dir().join("audio").join(profile), dest));
    }

    /// Ask where the export should go. The folder dialog runs on its own thread, like the
    /// import one, and the answer arrives through the export channel.
    fn pick_export_dest(&mut self) {
        if self.export.as_ref().is_some_and(|w| w.picking || w.busy.is_some()) {
            return;
        }
        if let Some(w) = &mut self.export {
            w.picking = true;
        }
        let (tx, ctx) = (self.export_tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(export::Msg::Dest(rfd::FileDialog::new().pick_folder()));
            ctx.request_repaint();
        });
    }

    /// Start the export on a background thread; progress and the outcome arrive through
    /// the export channel and are applied in `poll`.
    fn start_export(&mut self) {
        let Some(w) = &self.export else { return };
        if w.busy.is_some() || w.picking || w.dest.is_none() || w.dest_err.is_some() {
            return;
        }
        if let Some(e) = export::dest_refusal(w.dest.as_ref().unwrap(), &data_dir()) {
            if let Some(w) = &mut self.export {
                w.dest_err = Some(e);
            }
            return;
        }
        let plan = w.plan();
        if plan.chapters.is_empty() {
            return;
        }
        let (m4b, ffmpeg) = (w.m4b, w.ffmpeg.clone());
        if m4b && ffmpeg.is_none() {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        if let Some(w) = &mut self.export {
            w.result = None;
            w.busy = Some(export::Busy { cancel: cancel.clone(), frac: 0.0, label: String::new(), cancelling: false });
        }
        let (tx, ctx) = (self.export_tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let mut prog = |frac: f32, label: &str| {
                let _ = tx.send(export::Msg::Progress(frac, label.to_string()));
                ctx.request_repaint();
            };
            let r = if m4b {
                export::run_m4b(&plan, ffmpeg.as_deref(), &cancel, &mut prog)
            } else {
                export::run_wav(&plan, &cancel, &mut prog)
            };
            let _ = tx.send(export::Msg::Done(r));
            ctx.request_repaint();
        });
    }

    /// Ask the running export to stop; it removes its temporary files itself.
    fn cancel_export(&mut self) {
        if let Some(w) = &mut self.export {
            if let Some(b) = &mut w.busy {
                b.cancelling = true;
                b.cancel.store(true, Ordering::Relaxed);
            }
        }
    }

    // ---------- generation ----------

    /// Queue audio for a unit unless it exists or is queued. Returns its path.
    fn enqueue(&mut self, u: &Unit) -> Option<PathBuf> {
        if !self.installed.contains(&self.cfg.engine) {
            return None;
        }
        let path = self.unit_path(u)?;
        if self.is_ready(&path) || self.pending.contains_key(&path) {
            return Some(path);
        }
        let text = self.unit_text(u)?;
        let _ = std::fs::create_dir_all(path.parent()?);
        let label = self.unit_label(u);
        self.pending.insert(path.clone(), label);
        self.tts.submit(tts::Job { text, out: path.clone(), stream: None }, false);
        Some(path)
    }

    /// Generate a unit with streaming, ahead of the queue, starting at `start_char`.
    /// Generate a unit with streaming, starting at `start_char`, *now*: anything being generated
    /// in the background is paused and resumed afterwards. If this very unit was being made in
    /// the background, it restarts as a stream so playback begins within seconds.
    fn enqueue_stream(&mut self, u: &Unit, start_char: usize) -> Option<PathBuf> {
        let path = self.unit_path(u)?;
        // a double-click (or impatient second click) shouldn't restart the same request
        if let Some((p, c, at)) = &self.last_stream {
            if *p == path && *c == start_char && at.elapsed().as_secs_f32() < 2.0 {
                return Some(path);
            }
        }
        let text = self.unit_text(u)?;
        let _ = std::fs::create_dir_all(path.parent()?);
        let label = self.unit_label(u);
        self.pending.insert(path.clone(), label);
        self.tts.play_now(tts::Job { text, out: path.clone(), stream: Some(start_char) });
        self.last_stream = Some((path.clone(), start_char, Instant::now()));
        Some(path)
    }

    fn generate_all(&mut self) {
        if !self.engine_ready() {
            return;
        }
        let Some(b) = self.open_book() else { return };
        let id = b.id.clone();
        // sections left out of listening are not generated either
        let shape: Vec<(usize, usize)> = b
            .chapters
            .iter()
            .enumerate()
            .filter(|(_, c)| !c.skipped())
            .map(|(i, c)| (i, c.pages.len()))
            .collect();
        let mut n = 0;
        for (ch, pages) in shape.into_iter() {
            let units: Vec<Option<usize>> = if self.cfg.page_mode { (0..pages).map(Some).collect() } else { vec![None] };
            for pg in units {
                let u = Unit { book: id.clone(), ch, pg };
                if let Some(p) = self.unit_path(&u) {
                    if !self.is_ready(&p) && !self.pending.contains_key(&p) {
                        self.enqueue(&u);
                        n += 1;
                    }
                }
            }
        }
        if n == 0 {
            self.notify("Everything is already generated for this voice");
        } else {
            self.notify(format!("Queued {n} {}", if self.cfg.page_mode { "pages" } else { "chapters" }));
        }
    }

    fn set_speed(&mut self, v: f32) {
        self.cfg.speed = v;
        if let Some(p) = &mut self.player {
            p.set_speed(v);
        }
        self.cfg.save();
    }

    /// Throw away a chapter's audio (current voice) and generate it again with the current settings.
    fn regenerate_chapter(&mut self, id: &str, ch: usize) {
        if !self.engine_ready() {
            return;
        }
        let Some(b) = self.book(id) else { return };
        let pages = b.chapters.get(ch).map_or(0, |c| c.pages.len());
        let units: Vec<Unit> = if self.cfg.page_mode {
            (0..pages).map(|p| Unit { book: id.to_string(), ch, pg: Some(p) }).collect()
        } else {
            vec![Unit { book: id.to_string(), ch, pg: None }]
        };
        for u in &units {
            let Some(path) = self.unit_path(u) else { continue };
            if self.pending.contains_key(&path) {
                continue; // already being made
            }
            if self.player.as_ref().and_then(|p| p.now.as_ref()).is_some_and(|n| n.unit == *u) {
                if let Some(p) = &mut self.player {
                    p.stop();
                }
            }
            // delete the unit's files in whatever name form they are in
            if let Some(dir) = path.parent() {
                remove_unit_files(dir, ch, u.pg);
            }
            self.have.remove(&path);
            if let Some(k) = audio_key(&path) {
                self.have_keys.remove(&k);
            }
            self.enqueue(u);
        }
        self.refresh_audio_after(id);
        let title = self.unit_label(&Unit { book: id.to_string(), ch, pg: None });
        self.notify(format!("Regenerating {title} with the current settings"));
    }

    fn cancel_generation(&mut self) {
        if self.feed.is_some() {
            if let Some(p) = &mut self.player {
                p.stop();
            }
            self.feed = None;
        }
        self.tts.cancel();
        self.pending.clear();
        self.job = None;
        self.autoplay = None;
    }

    fn engine_ready(&mut self) -> bool {
        if self.installed.contains(&self.cfg.engine) {
            return true;
        }
        self.notify_err("Install a voice engine first (Settings → Voice engines)");
        self.show_settings = true;
        false
    }

    fn install_engine(&mut self, name: &str) {
        if self.install.is_none() {
            self.confirm_uninstall = None;
            self.inst.plan.clear();
            self.inst.step = 0;
            self.inst.frac = 0.0;
            self.inst.detail = "Getting ready…".into();
            self.inst.target = 0.0;
            self.inst.shown = 0.0;
            self.inst.started = Instant::now();
            self.inst.visible = true;
            self.inst.result = None;
            self.install = Some(installer::start(name, self.ctx.clone()));
        }
    }

    fn cancel_install(&mut self) {
        if let Some(i) = &self.install {
            i.cancel();
        }
    }

    /// Switch to a freshly installed engine and play a short sample of it.
    fn try_engine(&mut self, name: &str) {
        if self.cfg.engine != name {
            self.cfg.engine = name.to_string();
            self.worker_dirty = true;
        }
        self.preview_voice();
    }

    fn uninstall_engine(&mut self, name: &str) {
        if self.cfg.engine == name || self.worker_dirty {
            self.cancel_generation();
            self.tts.stop();
        }
        let removal = installer::remove(name, self.install.is_none());
        self.confirm_uninstall = None;
        self.leftover = leftover_engines();
        if !removal.installed {
            self.installed.remove(name);
            if self.cfg.engine == name {
                if let Some(e) = installer::SPECS.iter().find(|s| self.installed.contains(s.name)) {
                    self.cfg.engine = e.name.to_string();
                    self.worker_dirty = true;
                    self.apply_worker_settings();
                }
            }
        }
        match removal.notice(installer::spec(name).map_or(name, |s| s.title)) {
            Ok(text) => self.notify(text),
            Err(text) => self.notify_err(text),
        }
    }

    fn poll_install(&mut self) {
        let Some(inst) = &self.install else { return };
        let mut finished = None;
        while let Ok(ev) = inst.rx.try_recv() {
            match ev {
                installer::Ev::Plan(p) => self.inst.plan = p,
                installer::Ev::Step { i, frac, detail } => {
                    self.inst.step = i;
                    self.inst.frac = frac;
                    if !detail.is_empty() || frac == 0.0 {
                        self.inst.detail = detail;
                    }
                    let total: f32 = self.inst.plan.iter().map(|s| s.weight).sum();
                    if total > 0.0 && i < self.inst.plan.len() {
                        let before: f32 = self.inst.plan[..i].iter().map(|s| s.weight).sum();
                        let t = (before + self.inst.plan[i].weight * frac) / total;
                        self.inst.target = self.inst.target.max(t.min(1.0));
                    }
                }
                installer::Ev::Done => finished = Some(Ok(())),
                installer::Ev::Cancelled => finished = Some(Err(None)),
                installer::Ev::Failed(e) => finished = Some(Err(Some(e))),
            }
        }
        let Some(r) = finished else { return };
        let name = inst.engine.clone();
        let title = installer::spec(&name).map_or(name.clone(), |s| s.title.to_string());
        self.install = None;
        self.leftover = leftover_engines();
        match r {
            Ok(()) if installer::is_installed(&name) => {
                self.inst.target = 1.0;
                self.installed.insert(name.clone());
                // first engine, or the selected one wasn't installed: switch to it
                if !self.installed.contains(&self.cfg.engine) || self.installed.len() == 1 {
                    self.cfg.engine = name.clone();
                    self.worker_dirty = true;
                    self.apply_worker_settings();
                }
                if self.inst.visible {
                    self.inst.result = Some((name, InstallOutcome::Ok, Instant::now()));
                } else {
                    self.notify(format!("{title} is ready"));
                }
            }
            Ok(()) => {
                self.inst.visible = true;
                self.inst.result =
                    Some((name, InstallOutcome::Failed("The install finished but some files are missing.".into()), Instant::now()));
            }
            Err(None) => {
                self.inst.visible = false;
                self.notify("Installation cancelled — nothing was left behind");
            }
            Err(Some(e)) => {
                self.inst.visible = true;
                self.inst.result = Some((name, InstallOutcome::Failed(e), Instant::now()));
            }
        }
    }

    /// Restart the worker after the engine, voice or pacing changed.
    fn apply_worker_settings(&mut self) {
        if !self.worker_dirty {
            return;
        }
        self.worker_dirty = false;
        let had_jobs = !self.pending.is_empty();
        self.cancel_generation();
        self.tts = tts::Tts::start(self.cfg.worker_command(), self.cfg.normalize_key(), self.ctx.clone());
        self.cfg.save();
        self.refresh_have();
        if had_jobs {
            self.notify("Voice settings changed — the generation queue was cleared");
        }
    }

    // ---------- playback ----------

    /// Play a unit, generating it first if needed. `from_page` starts at that page of a chapter.
    fn play_unit(&mut self, u: Unit, from_page: Option<usize>) {
        let start = match (u.pg, from_page) {
            (None, Some(p)) if p > 0 => self.book(&u.book).and_then(|b| b.page_char_starts(u.ch).get(p).copied()),
            _ => None,
        };
        self.play_from_char(u, start);
    }

    /// Play a unit from the word at `start_char` (chars into the unit's text).
    fn play_from_char(&mut self, u: Unit, start_char: Option<usize>) {
        let Some(path) = self.unit_path(&u) else { return };
        if let Some(file) = self.existing_audio(&path) {
            self.play_path(&file, u, start_char);
        } else if self.engine_ready() {
            if let Some(p) = &mut self.player {
                p.stop();
            }
            self.feed = None;
            self.autoplay = self.enqueue_stream(&u, start_char.unwrap_or(0));
        }
    }

    /// Play an existing audio file (any voice profile).
    fn play_path(&mut self, path: &Path, u: Unit, start_char: Option<usize>) {
        let label = format!("{} — {}", self.book(&u.book).map(|b| b.title.as_str()).unwrap_or(""), self.unit_label(&u));
        let Some(player) = &mut self.player else {
            self.notify_err("No audio output device found");
            return;
        };
        match player.play(path, label, u.clone()) {
            Ok(()) => {
                if let Some(t) = start_char.and_then(|c| player.time_of_char(c)) {
                    player.seek(t);
                }
                self.follow_target = None;
                self.last_word = None;
                // prepare the next chapter/page while this one plays (current voice only)
                let same_unit_file = |a: &Path, b: &Path| {
                    let key = |p: &Path| p.parent().map(Path::to_path_buf).zip(p.file_stem().and_then(|s| s.to_str()).and_then(parse_unit_name));
                    key(a).is_some_and(|k| key(b) == Some(k))
                };
                if self.cfg.prefetch && self.unit_path(&u).as_deref().is_some_and(|p| same_unit_file(p, path)) {
                    if let Some(n) = self.next_unit(&u) {
                        self.enqueue(&n);
                    }
                }
            }
            Err(e) => self.notify_err(format!("Can't play audio: {e}")),
        }
    }

    /// Generate (once) and play a short sample of the selected voice.
    fn preview_voice(&mut self) {
        self.speak_sample(&self.cfg.profile(), PREVIEW_TEXT, false);
    }

    /// Speak `text` with the current engine and voice and play it. The sample is written to
    /// `<data>/previews/<stem>.wav` (the stem pins the profile and the text) and reused when it
    /// already exists. With no installed engine, `quiet` does nothing instead of opening Settings.
    fn speak_sample(&mut self, stem: &str, text: &str, quiet: bool) {
        if !self.installed.contains(&self.cfg.engine) {
            if !quiet {
                self.engine_ready(); // says why and opens Settings
            }
            return;
        }
        self.apply_worker_settings();
        let path = data_dir().join("previews").join(format!("{stem}.wav"));
        if path.exists() {
            if let Some(p) = &mut self.player {
                let _ = p.play(&path, "Voice preview".into(), Unit { book: String::new(), ch: 0, pg: None });
            }
            return;
        }
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        self.pending.insert(path.clone(), "voice preview".into());
        self.tts.submit(tts::Job { text: text.into(), out: path.clone(), stream: None }, true);
        self.preview = Some(path);
    }

    /// "Hear it" for one pronunciation entry: the stem pins the voice and a digest of the
    /// words, so every spoken form is generated once per voice.
    fn hear_form(&mut self, say: &str) {
        self.speak_sample(&format!("{}-{}", self.cfg.profile(), say_hash(say)), say, true);
    }

    // ---------- pronunciations ----------

    /// Add or update a pronunciation (the table file is saved at once; the worker picks it
    /// up without restarting). On success, chapters that already have audio and contain the
    /// word are collected for the regenerate offer. `reader` marks offers made in the
    /// reader's Set pronunciation window.
    fn pron_set(&mut self, word: &str, say: &str, reader: bool) -> Result<bool, String> {
        self.pron_sync();
        let (row, updated) = self.pron.set(word, say).map_err(|e| e.to_string())?;
        if let Err(e) = self.pron.save() {
            self.pron_reload(); // keep memory in step with what is on disk
            return Err(format!("Could not save pronunciations: {e}"));
        }
        self.pron_state = config::Pronounce::file_state();
        let word = self.pron.entries[row].0.clone();
        self.pron_err = None;
        self.regen_offer = Some(RegenOffer { chapters: self.chapters_with_word(&word), word, reader });
        Ok(updated)
    }

    fn pron_remove(&mut self, row: usize) {
        self.pron_sync();
        if row < self.pron.entries.len() {
            self.pron.remove(row);
            if let Err(e) = self.pron.save() {
                self.pron_reload();
                self.notify_err(format!("Could not save pronunciations: {e}"));
                return;
            }
            self.pron_state = config::Pronounce::file_state();
        }
        self.pron_err = None;
    }

    /// Re-read the table when the file changed since the last load (a hand edit, or another
    /// window of the app); one metadata call, a full parse only on a change.
    fn pron_sync(&mut self) {
        let st = config::Pronounce::file_state();
        if st != self.pron_state {
            self.pron_reload();
        }
    }

    fn pron_reload(&mut self) {
        self.pron = config::Pronounce::load();
        self.pron_state = config::Pronounce::file_state();
    }

    /// Chapters of the open book that contain `word` as a whole word (plural and possessive
    /// included, ignoring case) and already have audio for the current voice.
    fn chapters_with_word(&self, word: &str) -> Vec<usize> {
        let Some(b) = self.open_book() else { return Vec::new() };
        b.chapters
            .iter()
            .enumerate()
            .filter(|&(i, c)| {
                config::contains_word(&b.chapter_text(i), word)
                    && (if self.cfg.page_mode {
                        (0..c.pages.len()).any(|p| self.is_ready(&self.audio_path(b, i, Some(p))))
                    } else {
                        self.is_ready(&self.audio_path(b, i, None))
                    })
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Open the reader's Set pronunciation window for a clicked word, prefilled with the
    /// entry it has, if any.
    fn open_pron_win(&mut self, word: &str) {
        self.pron_sync();
        let say = self.pron.find(word).map(|i| self.pron.entries[i].1.clone()).unwrap_or_default();
        self.pron_win = Some(PronWin { word: word.to_string(), say, saved: false, err: None });
    }

    /// Regenerate the offered chapters and drop the offer.
    fn accept_regen_offer(&mut self) {
        let (offer, open) = (self.regen_offer.take(), self.open.clone());
        if let (Some(o), Some(id)) = (offer, open) {
            for ch in o.chapters {
                self.regenerate_chapter(&id, ch);
            }
        }
    }

    /// Close the Set pronunciation window and drop a regenerate offer that belongs to it.
    fn close_pron_win(&mut self) {
        self.pron_win = None;
        self.ctx.data_mut(|d| d.remove_temp::<String>(egui::Id::new("pron-win-say")));
        if self.regen_offer.as_ref().is_some_and(|o| o.reader) {
            self.regen_offer = None;
        }
    }

    fn toggle_play(&mut self) {
        if let Some(p) = &mut self.player {
            if p.now.is_some() {
                p.toggle();
                self.track_listen(true);
                return;
            }
        }
        if let Some(u) = self.reader_unit() {
            let page = self.pg;
            match self.resume_char(&u, Some(page)) {
                Some(c) => {
                    self.notify("Resuming where you stopped listening");
                    self.play_from_char(u, Some(c));
                }
                None => self.play_unit(u, Some(page)),
            }
        }
    }

    /// Play a chapter/page from its list button: from the word last heard if listening stopped
    /// inside it, otherwise from its start.
    fn play_unit_resuming(&mut self, u: Unit) {
        match self.resume_char(&u, None) {
            Some(c) => {
                self.notify("Resuming where you stopped listening");
                self.play_from_char(u, Some(c));
            }
            None => self.play_unit(u, None),
        }
    }

    /// Char offset to resume `u` at, if listening last stopped inside it. With `page`, the stop
    /// must also lie on that page of the chapter (so turning to another page plays that page).
    fn resume_char(&self, u: &Unit, page: Option<usize>) -> Option<usize> {
        let b = self.book(&u.book)?;
        let l = b.listen.as_ref()?;
        if l.ch != u.ch || l.pg != u.pg || l.chr == 0 {
            return None;
        }
        if let (None, Some(pg)) = (u.pg, page) {
            let starts = b.page_char_starts(u.ch);
            let lo = *starts.get(pg)?;
            let hi = starts.get(pg + 1).copied().unwrap_or(usize::MAX);
            if l.chr < lo || l.chr >= hi {
                return None;
            }
        }
        Some(l.chr)
    }

    /// Remember the word being spoken (per book, in progress.json) so listening can resume
    /// exactly there after a pause, a restart or a crash. Written at most every 2 s.
    fn track_listen(&mut self, force: bool) {
        if !force && self.listen_saved.elapsed().as_secs_f32() < 2.0 {
            return;
        }
        let Some(p) = self.player.as_ref() else { return };
        let Some(now) = p.now.as_ref() else { return };
        if now.unit.book.is_empty() {
            return; // voice preview
        }
        let Some(w) = p.current_word().and_then(|i| now.words.get(i)) else { return };
        let l = book::Listen { ch: now.unit.ch, pg: now.unit.pg, chr: w.0 };
        let id = now.unit.book.clone();
        // listening without the reader showing this book: move the reading position along too
        let in_reader = self.view == View::Reader && self.open.as_deref() == Some(id.as_str());
        self.listen_saved = Instant::now();
        let Some(b) = self.lib.iter_mut().find(|b| b.id == id) else { return };
        if b.listen.as_ref() == Some(&l) {
            return;
        }
        if !in_reader {
            let pg = l.pg.unwrap_or_else(|| b.page_char_starts(l.ch).partition_point(|&s| s <= l.chr).saturating_sub(1));
            b.pos = (l.ch, pg);
        }
        b.listen = Some(l);
        b.save_progress();
    }

    fn skip_unit(&mut self, forward: bool) {
        let Some(u) = self.player.as_ref().and_then(|p| p.now.as_ref()).map(|n| n.unit.clone()) else { return };
        if !forward && self.player.as_ref().is_some_and(|p| p.pos() > 3.0) {
            if let Some(p) = &mut self.player { p.seek(0.0); }
            return;
        }
        let n = if forward { self.next_unit(&u) } else { self.prev_unit(&u) };
        if let Some(n) = n {
            self.show_unit(&n);
            self.play_unit(n, None);
        }
    }

    /// Move the reader to a unit if it is in the open book.
    fn show_unit(&mut self, u: &Unit) {
        if self.open.as_deref() == Some(u.book.as_str()) && self.cfg.follow {
            self.go(u.ch, u.pg.unwrap_or(0));
        }
    }

    fn seek_by(&mut self, d: f32) {
        if let Some(p) = &mut self.player {
            let t = p.pos() + d;
            p.seek(t);
        }
    }

    // ---------- per-frame ----------

    fn poll(&mut self) {
        self.poll_install();
        let mut changed_books = HashSet::new();
        while let Ok(ev) = self.tts.rx.try_recv() {
            if std::env::var_os("INKVOICE_DEBUG").is_some() {
                let d = match &ev {
                    tts::Event::Started(p) => format!("started {}", p.display()),
                    tts::Event::StreamStart { out, .. } => format!("stream-start {}", out.display()),
                    tts::Event::Chunk { out, samples, .. } => format!("chunk {} {samples}", out.display()),
                    tts::Event::Progress(p, f) => format!("progress {} {f}", p.display()),
                    tts::Event::Done { out, .. } => format!("done {}", out.display()),
                    tts::Event::Failed(p, e) => format!("FAILED {} {e}", p.display()),
                    tts::Event::Interrupted(p) => format!("interrupted {}", p.display()),
                };
                eprintln!("EV {d}");
            }
            match ev {
                tts::Event::Started(p) => {
                    if self.pending.contains_key(&p) {
                        self.job = Some(Job { path: p, progress: 0.0, started: Instant::now() });
                    }
                }
                tts::Event::StreamStart { out, pcm, sr } => {
                    if self.autoplay.as_ref() == Some(&out) {
                        self.autoplay = None;
                        if let (Some(u), Some(player)) = (unit_of(&out), self.player.as_mut()) {
                            let label = format!(
                                "{} — {}",
                                self.lib.iter().find(|b| b.id == u.book).map(|b| b.title.as_str()).unwrap_or(""),
                                self.pending.get(&out).cloned().unwrap_or_default()
                            );
                            if player.play_stream(&out, sr, self.cfg.stream_buffer, label, u).is_ok() {
                                self.feed = Some(Feed { out, pcm, read: 0 });
                                self.follow_target = None;
                                self.last_word = None;
                            }
                        }
                    }
                }
                tts::Event::Chunk { out, samples, words } => {
                    self.feed_chunk(&out, samples, words);
                }
                tts::Event::Progress(p, f) => {
                    if let Some(j) = self.job.as_mut().filter(|j| j.path == p) {
                        j.progress = f;
                    }
                    if self.feed.as_ref().is_some_and(|fd| fd.out == p) {
                        if let Some(pl) = &mut self.player {
                            let b = pl.buffered();
                            if let Some(n) = pl.now.as_mut() {
                                n.est_total = (f > 0.02).then(|| b / f);
                            }
                        }
                    }
                }
                tts::Event::Done { out: p, stream_offset } => {
                    if self.feed.as_ref().is_some_and(|fd| fd.out == p) {
                        self.feed = None;
                        if let Some(pl) = &mut self.player {
                            if pl.now.as_ref().is_some_and(|n| n.path == p) {
                                pl.stream_finish(stream_offset);
                            }
                        }
                    }
                    self.pending.remove(&p);
                    if self.job.as_ref().is_some_and(|j| j.path == p) {
                        self.job = None;
                    }
                    self.have.insert(p.clone());
                    note_audio(&mut self.have_keys, &p);
                    if let Some(id) = book_id_of(&p) {
                        changed_books.insert(id);
                    }
                    if self.autoplay.as_ref() == Some(&p) {
                        self.autoplay = None;
                        if let Some(u) = unit_of(&p) {
                            self.play_path(&p, u, None);
                        }
                    }
                    if self.preview.as_ref() == Some(&p) {
                        self.preview = None;
                        if let Some(pl) = &mut self.player {
                            let _ = pl.play(&p, "Voice preview".into(), Unit { book: String::new(), ch: 0, pg: None });
                        }
                    }
                }
                tts::Event::Interrupted(p) => {
                    // paused for something the user wants to hear now; it is queued again
                    if self.job.as_ref().is_some_and(|j| j.path == p) {
                        self.job = None;
                    }
                }
                tts::Event::Failed(p, e) => {
                    if self.feed.as_ref().is_some_and(|fd| fd.out == p) {
                        self.feed = None;
                        if let Some(pl) = &mut self.player {
                            pl.stop();
                        }
                    }
                    let was_wanted = self.pending.remove(&p).is_some();
                    if self.job.as_ref().is_some_and(|j| j.path == p) {
                        self.job = None;
                    }
                    if self.autoplay.as_ref() == Some(&p) {
                        self.autoplay = None;
                    }
                    if was_wanted {
                        self.notify_err(format!("Voice generation failed: {e}"));
                    }
                }
            }
        }
        let busy: HashSet<PathBuf> = self.pending.keys().cloned().collect();
        for id in changed_books {
            if let Some(b) = self.book(&id) {
                let (a, j) = (scan_audio(b), scan_leftovers(b, &busy));
                self.lib_audio.insert(id.clone(), a);
                self.lib_junk.insert(id, j);
            }
        }

        while let Ok(r) = self.import_rx.try_recv() {
            self.importing = false;
            match r {
                Ok(b) => {
                    let id = b.id.clone();
                    self.notify(format!("Imported “{}”", b.title));
                    self.lib.retain(|x| x.id != id);
                    self.covers.remove(&id);
                    self.lib.push(b);
                    self.lib_audio.insert(id.clone(), scan_audio(self.book(&id).unwrap()));
                    self.open_reader(&id);
                    book::sort_library(&mut self.lib);
                }
                Err(e) if e.is_empty() => {}
                Err(e) => self.notify_err(format!("Import failed: {e}")),
            }
        }

        while let Ok(m) = self.export_rx.try_recv() {
            match m {
                export::Msg::Dest(d) => {
                    if let Some(d) = d {
                        self.export_dir = Some(d.clone());
                        if let Some(w) = self.export.as_mut() {
                            w.picking = false;
                            w.dest = Some(d);
                            w.dest_err = export::dest_refusal(w.dest.as_ref().unwrap(), &data_dir());
                        }
                    } else if let Some(w) = self.export.as_mut() {
                        w.picking = false;
                    }
                }
                export::Msg::Progress(f, l) => {
                    if let Some(w) = self.export.as_mut() {
                        if let Some(b) = &mut w.busy {
                            b.frac = f;
                            b.label = l;
                        }
                    }
                }
                export::Msg::Done(Ok(p)) => {
                    if let Some(w) = self.export.as_mut() {
                        let m4b = w.m4b; // the format cannot change while the run is going
                        w.busy = None;
                        w.result = Some(Ok((p, m4b)));
                    }
                }
                export::Msg::Done(Err(export::ExportErr::Cancelled)) => {
                    self.export = None;
                    self.notify("Export cancelled");
                }
                export::Msg::Done(Err(export::ExportErr::Msg(e))) => {
                    if let Some(w) = self.export.as_mut() {
                        w.busy = None;
                        w.result = Some(Err(e));
                    }
                }
            }
        }

        // audio ran out: continue with the next chapter/page
        if self.player.as_ref().is_some_and(|p| p.finished()) {
            let u = self.player.as_ref().unwrap().now.as_ref().unwrap().unit.clone();
            self.player.as_mut().unwrap().stop();
            if let Some(b) = self.lib.iter_mut().find(|b| b.id == u.book) {
                if b.listen.as_ref().is_some_and(|l| l.ch == u.ch && l.pg == u.pg) {
                    b.listen = None;
                    b.save_progress();
                }
            }
            if self.cfg.continuous {
                if let Some(n) = self.next_unit(&u) {
                    self.show_unit(&n);
                    self.play_unit(n, None);
                }
            }
        }

        if self.player.as_ref().is_some_and(|p| p.is_playing()) {
            self.track_listen(false);
        }

        // follow along: turn the page when the spoken word moves onto another page
        if let Some(target) = self.spoken_page() {
            if self.follow_target != Some(target) {
                self.follow_target = Some(target);
                if self.cfg.follow && self.view == View::Reader && (self.ch, self.pg) != target {
                    self.go(target.0, target.1);
                }
            }
        }
    }

    /// Append newly streamed audio to the player.
    fn feed_chunk(&mut self, out: &Path, samples: usize, words: Vec<player::Word>) {
        let Some(feed) = self.feed.as_mut().filter(|f| f.out == out) else { return };
        let Some(pl) = self.player.as_mut().filter(|p| p.now.as_ref().is_some_and(|n| n.path == out)) else {
            self.feed = None; // user switched to something else
            return;
        };
        use std::io::{Read, Seek, SeekFrom};
        let mut buf = vec![0u8; samples * 2];
        let ok = std::fs::File::open(&feed.pcm)
            .and_then(|mut f| f.seek(SeekFrom::Start(feed.read)).and_then(|_| f.read_exact(&mut buf)))
            .is_ok();
        if ok {
            feed.read += buf.len() as u64;
            pl.stream_append(player::pcm_to_f32(&buf), words);
        }
    }

    /// (chapter, page) of the word currently being spoken, if it is in the open book.
    fn spoken_page(&self) -> Option<(usize, usize)> {
        let p = self.player.as_ref()?;
        let now = p.now.as_ref()?;
        if self.open.as_deref() != Some(now.unit.book.as_str()) || !p.is_playing() {
            return None;
        }
        match now.unit.pg {
            Some(pg) => Some((now.unit.ch, pg)),
            None => {
                let w = now.words.get(p.current_word()?)?;
                let starts = self.open_book()?.page_char_starts(now.unit.ch);
                let pg = starts.partition_point(|&s| s <= w.0).saturating_sub(1);
                Some((now.unit.ch, pg))
            }
        }
    }

    fn cover(&mut self, b_id: &str) -> Option<egui::TextureHandle> {
        if let Some(t) = self.covers.get(b_id) {
            return t.clone();
        }
        let tex = self.book(b_id).and_then(|b| {
            let img = image::open(b.cover_path()).ok()?.to_rgba8();
            let size = [img.width() as usize, img.height() as usize];
            let ci = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
            Some(self.ctx.load_texture(format!("cover-{b_id}"), ci, egui::TextureOptions::LINEAR))
        });
        self.covers.insert(b_id.to_string(), tex.clone());
        tex
    }
}

/// Write the bundled worker scripts next to the data (kept in sync with this build).
fn install_workers() {
    let wd = data_dir().join("workers");
    let _ = std::fs::create_dir_all(&wd);
    for (name, src) in WORKERS {
        let p = wd.join(name);
        if std::fs::read_to_string(&p).ok().as_deref() != Some(*src) {
            let _ = std::fs::write(p, src);
        }
    }
}

/// Audio files by the folder they are in and their unit key, whichever name form they have.
type AudioKeys = HashMap<(PathBuf, (usize, Option<usize>)), PathBuf>;

fn audio_key(p: &Path) -> Option<(PathBuf, (usize, Option<usize>))> {
    Some((p.parent()?.to_path_buf(), parse_unit_name(p.file_stem()?.to_str()?)?))
}

fn known_audio(keys: &AudioKeys, path: &Path) -> Option<PathBuf> {
    keys.get(&audio_key(path)?).cloned()
}

fn note_audio(keys: &mut AudioKeys, p: &Path) {
    if let Some(k) = audio_key(p) {
        keys.insert(k, p.to_path_buf());
    }
}

/// .../books/<id>/audio/<profile>/c0001.wav -> id
fn book_id_of(p: &Path) -> Option<String> {
    Some(p.parent()?.parent()?.parent()?.file_name()?.to_string_lossy().into_owned())
}

/// Short digest of a spoken form, for "hear it" preview file names.
fn say_hash(say: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    say.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn parse_unit_name(stem: &str) -> Option<(usize, Option<usize>)> {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let key = |k: &str| -> Option<(usize, Option<usize>)> {
        Some(match k.split_once("_p") {
            Some((c, p)) if digits(c) && digits(p) => (c.parse().ok()?, Some(p.parse().ok()?)),
            None if digits(k) => (k.parse().ok()?, None),
            _ => return None,
        })
    };
    // the old bare form "c0006" starts with the key; the readable form "07_Title_c0006"
    // carries it at the end, after the last "_c"
    if let Some(u) = stem.strip_prefix('c').and_then(key) {
        return Some(u);
    }
    let (_, k) = stem.rsplit_once("_c")?;
    key(k)
}

/// Slug of a section title for audio file names: "Quiet Harbor - A. B. Carter" ->
/// "Quiet_Harbor_ABCarter", "Chapter 5" -> "Chapter_5"; words joined by "_", the author part
/// of a "Title - Author" title compacted to its letters and digits; ASCII letters, digits and
/// "_" only, at most 60 characters, empty when nothing is left.
fn title_slug(title: &str) -> String {
    let words = |s: &str| {
        s.split_whitespace()
            .map(|w| w.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>())
            .filter(|w| !w.is_empty())
            .collect::<Vec<_>>()
            .join("_")
    };
    let ascii = deunicode::deunicode(title);
    let mut slug = match ascii.split_once(" - ") {
        Some((head, by)) if !by.trim().is_empty() => {
            let author: String = by.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
            format!("{}_{}", words(head), author)
        }
        _ => words(&ascii),
    };
    slug.truncate(60);
    while slug.starts_with('_') {
        slug.remove(0);
    }
    while slug.ends_with('_') {
        slug.pop();
    }
    slug
}

/// Audio file stem of a unit: "06_Quiet_Harbor_c0006", page mode "06_Quiet_Harbor_c0006_p0003".
/// The section number takes two digits, three in a book with 100 or more sections, and the
/// chapter key ends the name so a later title change never orphans the lookup.
fn unit_stem(b: &Book, ch: usize, pg: Option<usize>) -> String {
    let w = if b.chapters.len() >= 100 { 3 } else { 2 };
    let key = match pg {
        None => format!("c{ch:04}"),
        Some(p) => format!("c{ch:04}_p{p:04}"),
    };
    let slug = b.chapters.get(ch).map(|c| title_slug(&c.title)).unwrap_or_default();
    if slug.is_empty() {
        format!("{:0w$}_{key}", ch, w = w)
    } else {
        format!("{:0w$}_{slug}_{key}", ch, w = w)
    }
}

/// Every audio and timing file of one unit key in a voice folder, any name form.
fn unit_files(dir: &Path, ch: usize, pg: Option<usize>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for p in rd.flatten().map(|e| e.path()) {
            let same = p.file_stem().and_then(|s| s.to_str()).and_then(parse_unit_name) == Some((ch, pg));
            if same && p.extension().is_some_and(|e| e == "wav") {
                out.push(p.clone());
                out.push(player::timing_path(&p));
            }
        }
    }
    out
}

/// Everything "delete this section's audio" covers in a voice folder: the wav each of its
/// units is generated to, present or still queued, and the unit's files in any name form.
fn section_audio_paths(b: &Book, dir: &Path, ch: usize, page_mode: bool) -> Vec<PathBuf> {
    let pages = b.chapters.get(ch).map_or(0, |c| c.pages.len());
    let units: Vec<Option<usize>> = if page_mode { (0..pages).map(Some).collect() } else { vec![None] };
    let mut out = Vec::new();
    for pg in units {
        out.push(dir.join(unit_stem(b, ch, pg)).with_extension("wav"));
        out.extend(unit_files(dir, ch, pg));
    }
    out
}

/// Delete every audio (and timing) file for one unit key in a voice folder, any name form.
fn remove_unit_files(dir: &Path, ch: usize, pg: Option<usize>) {
    for p in unit_files(dir, ch, pg) {
        let _ = std::fs::remove_file(p);
    }
}

/// Every file in `dir` and its subfolders (the voice folders of a book's audio).
fn folder_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(folder_files(&p));
        } else {
            out.push(p);
        }
    }
    out
}

/// Stop and forget everything the app keeps for one wav: playback of it, its stream feed,
/// and its queued or running generation. The worker reports a cancelled running job as
/// Interrupted, which the event handler ignores, so this must clear the state itself.
fn halt_unit_audio(
    player: &mut Option<Player>,
    feed: &mut Option<Feed>,
    pending: &mut HashMap<PathBuf, String>,
    job: &mut Option<Job>,
    autoplay: &mut Option<PathBuf>,
    wav: &Path,
    tts: &tts::Tts,
) {
    if player.as_ref().and_then(|p| p.now.as_ref()).is_some_and(|n| n.path == wav) {
        if let Some(p) = player.as_mut() {
            p.stop();
        }
    }
    if feed.as_ref().is_some_and(|f| f.out == wav) {
        *feed = None;
    }
    if pending.remove(wav).is_some() {
        tts.cancel_one(wav);
        if job.as_ref().is_some_and(|j| j.path == wav) {
            *job = None;
        }
    }
    if autoplay.as_deref() == Some(wav) {
        *autoplay = None;
    }
}

/// Halt everything under a folder: playback of any file in it, its stream feed, and every
/// queued or running generation whose wav lies in it (so a running stream's temp file is
/// never taken away while it is still being written).
fn halt_audio_under(
    player: &mut Option<Player>,
    feed: &mut Option<Feed>,
    pending: &mut HashMap<PathBuf, String>,
    job: &mut Option<Job>,
    autoplay: &mut Option<PathBuf>,
    tts: &tts::Tts,
    dir: &Path,
) {
    if player.as_ref().and_then(|p| p.now.as_ref()).is_some_and(|n| n.path.starts_with(dir)) {
        if let Some(p) = player.as_mut() {
            p.stop();
        }
    }
    if feed.as_ref().is_some_and(|f| f.out.starts_with(dir)) {
        *feed = None;
    }
    let jobs: Vec<PathBuf> = pending.keys().filter(|p| p.starts_with(dir)).cloned().collect();
    for p in jobs {
        pending.remove(&p);
        tts.cancel_one(&p);
        if job.as_ref().is_some_and(|j| j.path == p) {
            *job = None;
        }
    }
    if autoplay.as_ref().is_some_and(|a| a.starts_with(dir)) {
        *autoplay = None;
    }
}

/// The core of "delete all audio in this folder" (one voice, or a book's whole audio
/// folder): halt everything alive under `dir`, then send the folder's files to the trash.
/// `dir` and everything in it must sit under `audio_dir`.
fn clear_audio_dir(
    dir: &Path,
    audio_dir: &Path,
    player: &mut Option<Player>,
    feed: &mut Option<Feed>,
    pending: &mut HashMap<PathBuf, String>,
    job: &mut Option<Job>,
    autoplay: &mut Option<PathBuf>,
    tts: &tts::Tts,
    rm: &mut dyn Remover,
) -> Deleted {
    halt_audio_under(player, feed, pending, job, autoplay, tts, dir);
    let files = folder_files(dir);
    delete_audio_files(&files, audio_dir, rm)
}

/// Open a folder in the system's file manager (Explorer on Windows, xdg-open/open elsewhere).
fn open_folder(dir: &Path) {
    let prog = if cfg!(target_os = "windows") { "explorer" } else if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let _ = std::process::Command::new(prog).arg(dir).spawn();
}

fn leftover_engines() -> HashSet<String> {
    installer::SPECS.iter().filter(|s| installer::has_leftovers(s.name)).map(|s| s.name.to_string()).collect()
}

/// One voice folder's worth of the one-time audio renaming.
pub struct AudioMove {
    /// index into the library the migration ran on (ids are title-derived; keep them out of logs)
    pub book: usize,
    pub profile: String,
    pub renamed: usize,
    /// old-form files that could not be renamed and stayed
    pub left: usize,
}

/// One-time move to readable audio file names: in every voice folder of every book, rename
/// old bare-key files ("c0006.wav" plus its ".json") to the readable "06_Title_c0006.wav" form, and
/// remove leftover "*.stream.pcm" files of interrupted streams. A file whose target exists,
/// or that cannot be renamed, is left as it is; nothing else is touched, and a second run
/// finds nothing to do.
pub fn migrate_audio_names(lib: &[Book]) -> Vec<AudioMove> {
    migrate_audio_in(lib, &book::books_dir())
}

fn migrate_audio_in(lib: &[Book], books: &Path) -> Vec<AudioMove> {
    let mut out = Vec::new();
    for (bi, b) in lib.iter().enumerate() {
        let Ok(profiles) = std::fs::read_dir(books.join(&b.id).join("audio")) else { continue };
        for dir in profiles.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
            let profile = dir.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let (mut renamed, mut left) = (0, 0);
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name.ends_with(".stream.pcm") {
                    let _ = std::fs::remove_file(&p);
                    continue;
                }
                let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else { continue };
                if !stem.starts_with('c') || p.extension().is_some_and(|e| e != "wav") {
                    continue; // new-form name, or not audio
                }
                let Some((ch, pg)) = parse_unit_name(stem) else { continue };
                if b.chapters.get(ch).is_none() {
                    left += 1; // audio without its section; leave it
                    continue;
                }
                let want = dir.join(unit_stem(b, ch, pg)).with_extension("wav");
                if want == p || want.exists() {
                    left += 1; // never rename onto an existing file
                    continue;
                }
                // the timing json moves with its wav, or not at all: it goes first, and
                // comes back if the wav cannot follow
                let json = p.with_extension("json");
                let json_to = want.with_extension("json");
                let has_json = json.exists();
                if has_json && (json_to.exists() || std::fs::rename(&json, &json_to).is_err()) {
                    left += 1;
                    continue;
                }
                if std::fs::rename(&p, &want).is_err() {
                    if has_json {
                        let _ = std::fs::rename(&json_to, &json);
                    }
                    left += 1;
                    continue;
                }
                renamed += 1;
            }
            out.push(AudioMove { book: bi, profile, renamed, left });
        }
    }
    out
}

/// Everything the app fixes once at start-up, before the library is used: short book folder
/// names, then classification while loading, then readable audio file names.
fn migrate_startup() -> Vec<Book> {
    book::migrate_library();
    let lib = book::load_library();
    migrate_audio_names(&lib);
    lib
}

fn unit_of(p: &Path) -> Option<Unit> {
    let (ch, pg) = parse_unit_name(p.file_stem()?.to_str()?)?;
    Some(Unit { book: book_id_of(p)?, ch, pg })
}

/// The neighbouring unit, stepping over sections that are left out of listening.
fn step_unit(b: &Book, u: &Unit, forward: bool) -> Option<Unit> {
    let mut n = u.clone();
    loop {
        if forward {
            match n.pg {
                None => n.ch += 1,
                Some(p) if p + 1 < b.chapters.get(n.ch)?.pages.len() => n.pg = Some(p + 1),
                Some(_) => {
                    n.ch += 1;
                    n.pg = Some(0);
                }
            }
        } else {
            match n.pg {
                None => n.ch = n.ch.checked_sub(1)?,
                Some(p) if p > 0 => n.pg = Some(p - 1),
                Some(_) => {
                    n.ch = n.ch.checked_sub(1)?;
                    n.pg = Some(b.chapters[n.ch].pages.len().saturating_sub(1));
                }
            }
        }
        if !b.chapters.get(n.ch)?.skipped() {
            return Some(n);
        }
    }
}
fn scan_audio(b: &Book) -> Vec<AudioItem> {
    scan_audio_in(b, &b.dir().join("audio"))
}

/// The same scan against an explicit audio folder (tests point it at a scratch tree).
fn scan_audio_in(b: &Book, audio: &Path) -> Vec<AudioItem> {
    let mut out = Vec::new();
    let Ok(profiles) = std::fs::read_dir(audio) else { return out };
    let mut profiles: Vec<PathBuf> = profiles.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    profiles.sort();
    for dir in profiles {
        let profile = dir.file_name().unwrap().to_string_lossy().into_owned();
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "wav")).collect();
        paths.sort();
        for path in paths {
            let Some((ch, pg)) = path.file_stem().and_then(|s| s.to_str()).and_then(parse_unit_name) else { continue };
            let Some(c) = b.chapters.get(ch) else { continue };
            let label = match pg {
                None => c.title.clone(),
                Some(p) => format!("{} · page {}", c.title, p + 1),
            };
            let (size, modified) = std::fs::metadata(&path).map_or((0, 0), |m| {
                (m.len(), m.modified().map_or(0, |t| unix_secs(t)))
            });
            out.push(AudioItem {
                profile: profile.clone(),
                duration: player::wav_duration(&path).unwrap_or(0.0),
                unit: Unit { book: b.id.clone(), ch, pg },
                path,
                label,
                size,
                modified,
            });
        }
    }
    // rows follow the book, whatever name form or voice folder the files sit in
    out.sort_by(|a, b| (&a.profile, a.unit.ch, a.unit.pg).cmp(&(&b.profile, b.unit.ch, b.unit.pg)));
    out
}

/// Files under a book's audio folder that no generation owns any more: interrupted stream
/// temp files, part-written files, estimated timings, timing files with no wav beside them,
/// and audio whose key points at a section the book does not have (their timing files too).
/// `busy` are the wav paths queued or being made right now; their temp and timing files are
/// left alone.
fn scan_leftovers(b: &Book, busy: &HashSet<PathBuf>) -> Vec<JunkFile> {
    scan_leftovers_in(b, &b.dir().join("audio"), busy)
}

/// The same scan against an explicit audio folder (tests point it at a scratch tree).
fn scan_leftovers_in(b: &Book, audio: &Path, busy: &HashSet<PathBuf>) -> Vec<JunkFile> {
    let mut out: Vec<JunkFile> = Vec::new();
    let Ok(profiles) = std::fs::read_dir(audio) else { return out };
    for dir in profiles.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        paths.sort();
        let wavs: HashSet<PathBuf> = paths.iter().filter(|p| p.extension().is_some_and(|e| e == "wav")).cloned().collect();
        for p in &paths {
            let Some(name) = p.file_name().and_then(|s| s.to_str()) else { continue };
            // the wav a temp file belongs to, if it is one
            let temp_of = name
                .strip_suffix(".part")
                .or_else(|| name.strip_suffix(".stream.pcm"))
                .map(|base| dir.join(base.strip_suffix(".json").map(|j| format!("{j}.wav")).unwrap_or_else(|| base.to_string())));
            let size = |p: &Path| std::fs::metadata(p).map_or(0, |m| m.len());
            if let Some(wav) = temp_of {
                if !busy.contains(&wav) {
                    out.push(JunkFile { path: p.clone(), size: size(p) });
                }
            } else if name.ends_with(".json.estimated") {
                out.push(JunkFile { path: p.clone(), size: size(p) });
            } else if p.extension().is_some_and(|e| e == "json")
                && !wavs.contains(&p.with_extension("wav"))
                && !busy.contains(&p.with_extension("wav"))
            {
                out.push(JunkFile { path: p.clone(), size: size(p) });
            } else if p.extension().is_some_and(|e| e == "wav") {
                // audio whose key points at a section the book does not have, with its timing
                let gone = |p: &Path| {
                    p.file_stem()
                        .and_then(|s| s.to_str())
                        .and_then(parse_unit_name)
                        .is_some_and(|(ch, _)| b.chapters.get(ch).is_none())
                };
                if gone(p) && !busy.contains(p) {
                    out.push(JunkFile { path: p.clone(), size: size(p) });
                    let json = player::timing_path(p);
                    let js = size(&json);
                    if json.exists() && !out.iter().any(|j| j.path == json) {
                        out.push(JunkFile { path: json, size: js });
                    }
                }
            }
        }
    }
    out
}

/// How a deletion went: how many files moved, and the first one that could not, with its
/// reason (a refusal to delete outside the audio folder, or the trash's complaint).
struct Deleted {
    moved: usize,
    failed: Option<(PathBuf, String)>,
}

/// Delete `paths` (plus each wav's timing file) through `rm`, refusing anything outside
/// `audio_dir`: a refused or failed file is left where it is. Missing files are skipped;
/// the run carries on past a file that cannot be moved, so `moved` counts what went and
/// `failed` names the first hold-out.
fn delete_audio_files(paths: &[PathBuf], audio_dir: &Path, rm: &mut dyn Remover) -> Deleted {
    let refuse = |p: &Path, why: String| Deleted { moved: 0, failed: Some((p.to_path_buf(), why)) };
    let Ok(root) = audio_dir.canonicalize() else {
        return refuse(audio_dir, format!("{}: no such folder", audio_dir.display()));
    };
    let (mut todo, mut seen) = (Vec::new(), Vec::new());
    for p in paths {
        for f in [p, &player::timing_path(p)] {
            // canonicalize also skips files that are already gone; the resolved form is
            // only for the inside-the-folder check, because the shell trash API on Windows
            // rejects verbatim (\\?\) paths, so the remover gets the path as it was built
            let Ok(c) = f.canonicalize() else { continue };
            if !c.starts_with(&root) {
                return refuse(f, "it is outside the book's audio folder".into());
            }
            if !seen.contains(&c) {
                seen.push(c);
                todo.push(f.clone());
            }
        }
    }
    if todo.is_empty() {
        return Deleted { moved: 0, failed: None };
    }
    // one trash operation for the whole group; if it fails, work out file by file what held out
    if rm.remove_all(&todo).is_ok() {
        return Deleted { moved: todo.len(), failed: None };
    }
    let (mut moved, mut failed) = (0, None);
    for p in &todo {
        if !p.exists() {
            moved += 1; // it went with the group after all
            continue;
        }
        match rm.remove(p) {
            Ok(()) => moved += 1,
            Err(e) if failed.is_none() => failed = Some((p.clone(), e)),
            Err(_) => {}
        }
    }
    Deleted { moved, failed }
}

/// Unix seconds of a file timestamp (0 when before the epoch).
fn unix_secs(t: std::time::SystemTime) -> u64 {
    t.duration_since(std::time::SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.poll();
        if self.job.is_some() || self.install.is_some() || self.inst.result.is_some() || self.player.as_ref().is_some_and(|p| p.now.is_some()) || self.status.is_some() || self.export.as_ref().is_some_and(|w| w.busy.is_some() || w.picking) {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        self.shortcuts(ctx);
        self.draw(ctx);
    }
}

fn main() -> eframe::Result {
    book::migrate_legacy();
    let args: Vec<String> = std::env::args().skip(1).collect();
    // headless import: `inkvoice --import book.epub`
    if args.first().map(String::as_str) == Some("--import") {
        book::migrate_library(); // imports land next to the short ids
        for p in &args[1..] {
            match book::import(Path::new(p)) {
                Ok(b) => println!("imported: {} ({} chapters)", b.title, b.chapters.len()),
                Err(e) => eprintln!("{p}: {e}"),
            }
        }
        return Ok(());
    }
    let open_file = args.into_iter().find(|a| !a.starts_with("--")).map(PathBuf::from);

    let icon = image::load_from_memory(include_bytes!("../assets/inkvoice.png")).ok().map(|i| {
        let i = i.to_rgba8();
        egui::IconData { width: i.width(), height: i.height(), rgba: i.into_raw() }
    });
    let mut viewport = egui::ViewportBuilder::default();
    if let Some(icon) = icon {
        viewport = viewport.with_icon(std::sync::Arc::new(icon));
    }
    let opts = eframe::NativeOptions {
        viewport: viewport
            .with_inner_size([1180.0, 780.0])
            .with_min_inner_size([760.0, 480.0])
            .with_app_id("inkvoice"),
        // With vsync on, eglSwapBuffers blocks forever on Wayland while the window is hidden
        // (other workspace / covered), freezing the event loop -> Hyprland "not responding".
        vsync: false,
        ..Default::default()
    };
    eframe::run_native("InkVoice", opts, Box::new(move |cc| Ok(Box::new(App::new(cc, open_file)))))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the rule layer in python3 with a stand-in for the inflect package whose number
    /// wording refuses large numbers, as the real one does above its largest named scale.
    const RULES_PROBE: &str = r#"
import json, sys, types
class Engine:
    def number_to_words(self, n, andword=""):
        if n >= 10 ** 36:
            raise ValueError("number out of range")
        return {5: "five", 12: "twelve"}[n]
fake = types.ModuleType("inflect")
fake.engine = Engine
sys.modules["inflect"] = fake
sys.path.insert(0, sys.argv[1])
import spoken_rules
print(json.dumps(spoken_rules.apply(sys.argv[2:])))
"#;

    /// A number the wording library refuses is spoken as written; it does not fail the piece,
    /// and the words around it still get their spoken forms.
    #[test]
    fn rule_layer_keeps_a_number_it_cannot_word() {
        let huge = "1".repeat(40);
        let grouped = format!("1{}", ",000".repeat(13));
        let out = std::process::Command::new("python3")
            .args(["-B", "-c", RULES_PROBE, concat!(env!("CARGO_MANIFEST_DIR"), "/workers")])
            .args(["5%", &huge, &grouped, &format!("${huge}"), &format!("{huge}%"), "12%"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let got: Vec<(String, Option<String>)> = serde_json::from_slice(&out.stdout).unwrap();
        let settled = |s: &str| ("settled".to_string(), Some(s.to_string()));
        let keep = ("keep".to_string(), None);
        assert_eq!(got, vec![settled("five percent"), keep.clone(), keep.clone(), keep.clone(), keep, settled("twelve percent")]);
    }

    #[test]
    fn parses_audio_names() {
        assert_eq!(parse_unit_name("c0003"), Some((3, None)));
        assert_eq!(parse_unit_name("c0001_p0012"), Some((1, Some(12))));
        assert_eq!(parse_unit_name("junk"), None);
        let u = unit_of(Path::new("/x/books/my_book/audio/phonon-everly/c0002_p0001.wav")).unwrap();
        assert_eq!(u, Unit { book: "my_book".into(), ch: 2, pg: Some(1) });
    }

    /// A finished job of another book never makes a section of the open book count as ready,
    /// and does not displace the open book's own file for the same section.
    #[test]
    fn finished_audio_of_another_book_is_not_ready_here() {
        let open_dir = Path::new("/x/books/book_b/audio/phonon-everly");
        let old_form = open_dir.join("c0002.wav");
        let mut keys = AudioKeys::new();
        note_audio(&mut keys, &old_form);
        note_audio(&mut keys, Path::new("/x/books/book_a/audio/phonon-everly/02_Other_c0002.wav"));
        note_audio(&mut keys, Path::new("/x/books/book_a/audio/phonon-everly/03_Other_c0003.wav"));
        assert_eq!(known_audio(&keys, &open_dir.join("03_Mine_c0003.wav")), None);
        assert_eq!(known_audio(&keys, &open_dir.join("02_Mine_c0002.wav")), Some(old_form));
        // another voice of the same book is a different folder too
        assert_eq!(known_audio(&keys, Path::new("/x/books/book_b/audio/kokoro-sky/02_Mine_c0002.wav")), None);
    }

    #[test]
    fn audio_names_round_trip() {
        // "Title - Author" titles: the author part compacted; plain titles: words joined
        assert_eq!(title_slug("Quiet Harbor - A. B. Carter"), "Quiet_Harbor_ABCarter");
        assert_eq!(title_slug("Chapter 5"), "Chapter_5");
        assert_eq!(title_slug("  "), "");
        assert_eq!(title_slug("Salt & Ember"), "Salt_Ember");
        let mk = |n: usize, title: &str| {
            let chapters: Vec<book::Chapter> =
                (0..n).map(|i| book::Chapter::new(if i == 1 { title.into() } else { format!("Section {i}") }, vec!["text".into()])).collect();
            book::Book { id: "b".into(), title: "T".into(), author: String::new(), source: String::new(), chapters, pos: (0, 0), opened: 0, listen: None }
        };
        let b = mk(7, "Quiet Harbor - A. B. Carter");
        assert_eq!(unit_stem(&b, 1, None), "01_Quiet_Harbor_ABCarter_c0001");
        assert_eq!(unit_stem(&b, 1, Some(3)), "01_Quiet_Harbor_ABCarter_c0001_p0003");
        let plain = mk(7, "Chapter 5");
        assert_eq!(unit_stem(&plain, 1, None), "01_Chapter_5_c0001");
        // no slug left: the key alone
        let mute = mk(2, "«»");
        assert_eq!(unit_stem(&mute, 1, None), "01_c0001");
        // three digits once the book has 100 or more sections
        let big = mk(101, "Quiet Harbor");
        assert_eq!(unit_stem(&big, 100, None), "100_Section_100_c0100");
        // every name the builder makes parses back to its key, and so do the old bare forms
        for (b, ch, pg) in [(&b, 1, None), (&b, 1, Some(3)), (&plain, 1, None), (&big, 100, None)] {
            let stem = unit_stem(b, ch, pg);
            assert_eq!(parse_unit_name(&stem), Some((ch, pg)), "{stem}");
        }
        assert_eq!(parse_unit_name("06_The_Salt_Road_c0006"), Some((6, None)));
        assert_eq!(parse_unit_name("06_The_Salt_Road_c0006_p0002"), Some((6, Some(2))));
        assert_eq!(parse_unit_name("06_c0006"), Some((6, None)));
        assert_eq!(parse_unit_name("06_Title_with_c3_inside_c0006"), Some((6, None)));
        assert_eq!(parse_unit_name("not_a_unit"), None);
        assert_eq!(parse_unit_name("01_Title_12_pX"), None);
    }

    /// Previous/next step over sections left out of listening, in both voice modes.
    #[test]
    fn stepping_skips_skipped_sections() {
        let chapters: Vec<book::Chapter> = (0..=4)
            .map(|i| book::Chapter::new(format!("Section {i}"), vec![format!("page {i} a"), format!("page {i} b")]))
            .collect();
        let b = book::Book {
            id: "b".into(),
            title: "T".into(),
            author: String::new(),
            source: String::new(),
            chapters,
            pos: (0, 0),
            opened: 0,
            listen: None,
        };
        let mut b = b;
        b.chapters[1].skip = Some(false); // kept on purpose
        b.chapters[3].skip = Some(true); // skipped on purpose
        let u = |ch: usize, pg: Option<usize>| Unit { book: "b".into(), ch, pg };
        assert_eq!(step_unit(&b, &u(0, None), true), Some(u(1, None)));
        assert_eq!(step_unit(&b, &u(2, None), true), Some(u(4, None)));
        assert_eq!(step_unit(&b, &u(4, None), true), None);
        assert_eq!(step_unit(&b, &u(4, None), false), Some(u(2, None)));
        assert_eq!(step_unit(&b, &u(0, None), false), None);
        // page mode crosses skipped chapters whole
        assert_eq!(step_unit(&b, &u(2, Some(1)), true), Some(u(4, Some(0))));
        assert_eq!(step_unit(&b, &u(4, Some(0)), false), Some(u(2, Some(1))));
        assert_eq!(step_unit(&b, &u(2, Some(0)), false), Some(u(1, Some(1))));
    }

    /// The one-time audio renaming on a scratch tree: old names and their timing jsons move
    /// together, stream leftovers go, blocked targets stay put, and a second run does nothing.
    #[test]
    fn migrates_audio_file_names() {
        let root = std::env::temp_dir().join("inkvoice-test-migrate-audio");
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("books/quiet_harbor-9f6689e3/audio/phonon-everly");
        std::fs::create_dir_all(&dir).unwrap();
        let b = book::Book {
            id: "quiet_harbor-9f6689e3".into(),
            title: "Quiet Harbor".into(),
            author: String::new(),
            source: String::new(),
            chapters: vec![
                book::Chapter::new("Title Page".into(), vec!["t".into()]),
                book::Chapter::new("The Quiet Harbor".into(), vec!["a".into()]),
            ],
            pos: (0, 0),
            opened: 0,
            listen: None,
        };
        let f = |name: &str| dir.join(name);
        for (wav, json) in [("c0000.wav", "c0000.json"), ("c0001.wav", "c0001.json"), ("c0001_p0000.wav", "c0001_p0000.json")] {
            std::fs::write(f(wav), b"wav").unwrap();
            std::fs::write(f(json), b"{}").unwrap();
        }
        std::fs::write(f("c0001.wav.stream.pcm"), b"pcm").unwrap(); // interrupted stream
        std::fs::write(f("06_The_Salt_Road_c0006.wav"), b"wav").unwrap(); // already readable form
        // chapter 1's timing target is taken, so that pair has to stay together
        std::fs::write(f("01_The_Quiet_Harbor_c0001.json"), b"{}").unwrap();

        let moves = migrate_audio_in(std::slice::from_ref(&b), &root.join("books"));
        assert_eq!(moves.len(), 1);
        // two pairs moved; the blocked pair is the one old-form pair that stayed
        assert_eq!((moves[0].renamed, moves[0].left), (2, 1));
        assert!(f("06_The_Salt_Road_c0006.wav").exists(), "a file with no section is left alone");
        assert!(f("00_Title_Page_c0000.wav").exists() && f("00_Title_Page_c0000.json").exists());
        assert!(f("c0001.wav").exists() && f("c0001.json").exists(), "the blocked pair stayed together");
        assert!(!f("01_The_Quiet_Harbor_c0001.wav").exists(), "the wav did not move without its json");
        assert!(f("01_The_Quiet_Harbor_c0001_p0000.wav").exists() && f("01_The_Quiet_Harbor_c0001_p0000.json").exists());
        assert!(!f("c0001_p0000.wav").exists() && !f("c0001_p0000.json").exists());
        assert!(!f("c0001.wav.stream.pcm").exists(), "stream leftover removed");
        // idempotent: nothing left to rename
        let again = migrate_audio_in(std::slice::from_ref(&b), &root.join("books"));
        assert_eq!((again[0].renamed, again[0].left), (0, 1));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A wav that another program holds open cannot be renamed; its timing file has to stay
    /// beside it under the old name, where the player looks for it.
    #[test]
    #[cfg(windows)]
    fn audio_rename_keeps_the_timing_file_with_a_wav_that_cannot_move() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = std::env::temp_dir().join("inkvoice-test-migrate-audio-held");
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("books/quiet_harbor-9f6689e3/audio/phonon-everly");
        std::fs::create_dir_all(&dir).unwrap();
        let b = book::Book {
            id: "quiet_harbor-9f6689e3".into(),
            title: "Quiet Harbor".into(),
            author: String::new(),
            source: String::new(),
            chapters: vec![book::Chapter::new("Title Page".into(), vec!["t".into()])],
            pos: (0, 0),
            opened: 0,
            listen: None,
        };
        std::fs::write(dir.join("c0000.wav"), b"wav").unwrap();
        std::fs::write(dir.join("c0000.json"), b"{}").unwrap();
        let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(dir.join("c0000.wav")).unwrap();

        let moves = migrate_audio_in(std::slice::from_ref(&b), &root.join("books"));
        assert_eq!((moves[0].renamed, moves[0].left), (0, 1));
        assert!(dir.join("c0000.wav").exists() && dir.join("c0000.json").exists(), "the pair stayed together");
        assert!(!dir.join("00_Title_Page_c0000.json").exists(), "the timing file came back");

        // once the wav is free, the next start moves both
        drop(held);
        let again = migrate_audio_in(std::slice::from_ref(&b), &root.join("books"));
        assert_eq!((again[0].renamed, again[0].left), (1, 0));
        assert!(dir.join("00_Title_Page_c0000.wav").exists() && dir.join("00_Title_Page_c0000.json").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Runs the start-up migration and classification against a copy of a real library:
    ///   MIGRATE_DIR=<folder> cargo test migrates_library_copy -- --ignored --nocapture
    /// <folder> holds data/ (the copied books) and config/; nothing outside it is touched.
    /// Prints folder name lengths, per voice folder renames, and per section kinds; no titles.
    #[test]
    #[ignore]
    fn migrates_library_copy() {
        let Ok(dir) = std::env::var("MIGRATE_DIR") else { return };
        let root = PathBuf::from(dir).canonicalize().expect("MIGRATE_DIR must name an existing folder");
        let (data, config) = (root.join("data"), root.join("config"));
        let _ = std::fs::create_dir_all(&config);
        std::env::set_var("INKVOICE_DATA", &data);
        std::env::set_var("INKVOICE_CONFIG", &config);
        let inside = |p: &Path| p.canonicalize().is_ok_and(|c| c.starts_with(&root));
        if !data.is_dir() || !inside(&data) || !inside(&config) {
            eprintln!("migrates_library_copy: refusing to run, data is {} and config is {}", data.display(), config.display());
            return;
        }
        // exactly what the app runs at start-up, in the same order
        for (from, to) in book::migrate_library() {
            println!("folder length {} -> {}", from.chars().count(), to.chars().count());
        }
        let lib = book::load_library();
        for m in migrate_audio_names(&lib) {
            println!("book {} voice {}: renamed {}, left {}", m.book, m.profile, m.renamed, m.left);
        }
        for (bi, b) in lib.iter().enumerate() {
            for (i, c) in b.chapters.iter().enumerate() {
                println!("book {bi} section {i} kind {} skipped {}", c.kind.map(|k| k.label()).unwrap_or("unclassified"), c.skipped());
            }
        }
    }
    // ---- audio management ----

    /// Plain removal inside the test's own tree; tests never touch the Recycle Bin.
    struct PlainRemove;
    impl Remover for PlainRemove {
        fn remove(&mut self, path: &Path) -> Result<(), String> {
            std::fs::remove_file(path).map_err(|e| e.to_string())
        }
    }

    /// A minimal 16-bit mono wav of `bytes` bytes (24000 Hz), so scans see a real size.
    pub(crate) fn wav_bytes(bytes: usize) -> Vec<u8> {
        // the data chunk holds whole samples, like real audio (an odd tail would be a
        // corrupt file), so the file may come out one byte shorter than asked for
        let data = ((bytes.max(44) - 44) / 2 * 2) as u32;
        let mut w = vec![0u8; 44 + data as usize];
        w[0..4].copy_from_slice(b"RIFF");
        w[4..8].copy_from_slice(&(36 + data).to_le_bytes());
        w[8..12].copy_from_slice(b"WAVE");
        w[12..16].copy_from_slice(b"fmt ");
        w[16..20].copy_from_slice(&16u32.to_le_bytes());
        w[20..22].copy_from_slice(&1u16.to_le_bytes());
        w[22..24].copy_from_slice(&1u16.to_le_bytes());
        w[24..28].copy_from_slice(&24000u32.to_le_bytes());
        w[28..32].copy_from_slice(&48000u32.to_le_bytes());
        w[32..34].copy_from_slice(&2u16.to_le_bytes());
        w[34..36].copy_from_slice(&16u16.to_le_bytes());
        w[36..40].copy_from_slice(b"data");
        w[40..44].copy_from_slice(&data.to_le_bytes());
        w
    }

    /// A scratch book folder with an audio tree, returned as (book dir, book, audio dir).
    /// Nothing here goes through INKVOICE_DATA; every scan and delete takes the folder.
    fn audio_tree(tag: &str) -> (PathBuf, book::Book, PathBuf) {
        let dir = std::env::temp_dir().join(format!("inkvoice-test-audio-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        let b = book::Book {
            id: "quiet_harbor-9f6689e3".into(),
            title: "Quiet Harbor".into(),
            author: String::new(),
            source: String::new(),
            chapters: vec![
                book::Chapter::new("Title Page".into(), vec!["t".into()]),
                book::Chapter::new("The Quiet Harbor".into(), vec!["a".into(), "b".into()]),
            ],
            pos: (0, 0),
            opened: 0,
            listen: None,
        };
        let audio = dir.join("audio");
        (dir, b, audio)
    }

    fn put(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn scans_audio_with_sizes_and_book_order() {
        let (_dir, b, audio) = audio_tree("scan");
        let kokoro = audio.join("kokoro-af_heart");
        let phonon = audio.join("phonon-everly");
        put(&kokoro, "00_Title_Page_c0000.wav", &wav_bytes(4096));
        put(&kokoro, "01_The_Quiet_Harbor_c0001_p0001.wav", &wav_bytes(8192));
        put(&kokoro, "01_The_Quiet_Harbor_c0001_p0000.wav", &wav_bytes(8192));
        // the old bare name form still reads, and sorts where it belongs
        put(&phonon, "c0001.wav", &wav_bytes(2048));
        // a wav of a section the book does not have is not listed (it is a leftover)
        put(&kokoro, "09_Ghost_c0009.wav", &wav_bytes(4096));
        put(&kokoro, "notes.txt", b"x");

        let items = scan_audio_in(&b, &audio);
        let rows: Vec<(&str, usize, Option<usize>, u64)> =
            items.iter().map(|i| (i.profile.as_str(), i.unit.ch, i.unit.pg, i.size)).collect();
        assert_eq!(
            rows,
            vec![
                ("kokoro-af_heart", 0, None, 4096),
                ("kokoro-af_heart", 1, Some(0), 8192),
                ("kokoro-af_heart", 1, Some(1), 8192),
                ("phonon-everly", 1, None, 2048),
            ]
        );
        assert_eq!(items[1].label, "The Quiet Harbor · page 1");
        assert!(items.iter().all(|i| i.modified > 0), "the scan gathers modification times");
        assert!(items.iter().all(|i| i.duration > 0.0), "the scan reads durations from the headers");
    }

    #[test]
    fn finds_leftovers_of_each_kind() {
        let (_dir, b, audio) = audio_tree("leftovers");
        let v = audio.join("kokoro-af_heart");
        // real audio of section 0, and its timing
        put(&v, "00_Title_Page_c0000.wav", &wav_bytes(4096));
        put(&v, "00_Title_Page_c0000.json", b"{}");
        // a stream of a running generation: not a leftover, its part file neither
        put(&v, "01_The_Quiet_Harbor_c0001.wav.stream.pcm", b"pcm");
        put(&v, "01_The_Quiet_Harbor_c0001.json.part", b"{}");
        // a job that is finishing writes its timing file before its wav
        put(&v, "01_The_Quiet_Harbor_c0001.json", b"{}");
        // an interrupted first generation: a stream temp with no wav at all
        put(&v, "c0001_p0000.wav.stream.pcm", b"pcm");
        // every other leftover kind
        put(&v, "00_Title_Page_c0000.wav.part", b"wav");
        put(&v, "00_Title_Page_c0000.json.estimated", b"{}");
        put(&v, "09_Ghost_c0009.json", b"{}"); // timing with no wav beside it
        put(&v, "09_Ghost_c0009.wav", &wav_bytes(4096));

        let busy: HashSet<PathBuf> = [audio.join("kokoro-af_heart/01_The_Quiet_Harbor_c0001.wav")]
            .into_iter()
            .collect();
        let junk: Vec<PathBuf> = scan_leftovers_in(&b, &audio, &busy).into_iter().map(|j| j.path).collect();
        let names: Vec<String> = junk.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(
            names,
            vec![
                "00_Title_Page_c0000.json.estimated",
                "00_Title_Page_c0000.wav.part",
                "09_Ghost_c0009.wav",
                "09_Ghost_c0009.json",
                "c0001_p0000.wav.stream.pcm",
            ]
        );
        assert!(!names.contains(&"00_Title_Page_c0000.wav".to_string()) && !names.contains(&"00_Title_Page_c0000.json".to_string()), "real audio stays");
        assert!(!names.contains(&"01_The_Quiet_Harbor_c0001.json.part".to_string()), "nor is the part file of a running generation");
        assert!(!names.contains(&"01_The_Quiet_Harbor_c0001.json".to_string()), "nor the timing file its wav is about to join");
        // once that job is over and left no wav, the same timing file is a leftover
        let later: Vec<PathBuf> = scan_leftovers_in(&b, &audio, &HashSet::new()).into_iter().map(|j| j.path).collect();
        assert!(later.contains(&v.join("01_The_Quiet_Harbor_c0001.json")));
        let _ = std::fs::remove_dir_all(audio.parent().unwrap());
    }


    #[test]
    fn deleting_one_unit_takes_wav_and_json_only() {
        let (_dir, _b, audio) = audio_tree("one");
        let v = audio.join("kokoro-af_heart");
        let wav0 = put(&v, "00_Title_Page_c0000.wav", &wav_bytes(4096));
        put(&v, "00_Title_Page_c0000.json", b"{}");
        let wav1 = put(&v, "01_The_Quiet_Harbor_c0001.wav", &wav_bytes(4096));
        put(&v, "01_The_Quiet_Harbor_c0001.json", b"{}");
        let other = put(&audio.join("phonon-everly"), "01_The_Quiet_Harbor_c0001.wav", &wav_bytes(4096));

        let r = delete_audio_files(&[wav0], &audio, &mut PlainRemove);
        assert_eq!((r.moved, r.failed.is_none()), (2, true), "the wav and its timing file");
        assert!(!v.join("00_Title_Page_c0000.wav").exists() && !v.join("00_Title_Page_c0000.json").exists());
        assert!(wav1.exists() && other.exists(), "other files are untouched");
        let _ = std::fs::remove_dir_all(audio.parent().unwrap());
    }

    #[test]
    fn deleting_a_voice_stops_its_generation_and_takes_its_temp_files() {
        let (_dir, _b, audio) = audio_tree("voice");
        let v = audio.join("kokoro-af_heart");
        put(&v, "00_Title_Page_c0000.wav", &wav_bytes(4096));
        put(&v, "00_Title_Page_c0000.json", b"{}");
        let queued = v.join("01_The_Quiet_Harbor_c0001.wav"); // queued, its stream temp on disk
        put(&v, "01_The_Quiet_Harbor_c0001.wav.stream.pcm", b"pcm");
        let other = put(&audio.join("phonon-everly"), "01_The_Quiet_Harbor_c0001.wav", &wav_bytes(4096));
        let tts = tts::Tts::start(vec!["no-such-engine".into()], None, eframe::egui::Context::default());
        let mut pending = HashMap::from([(queued.clone(), "The Quiet Harbor".into())]);
        let mut job = Some(Job { path: queued.clone(), progress: 0.4, started: Instant::now() });

        let mut autoplay = Some(queued.clone());
        let r = clear_audio_dir(&v, &audio, &mut None, &mut None, &mut pending, &mut job, &mut autoplay, &tts, &mut PlainRemove);
        assert_eq!((r.moved, r.failed.is_none()), (3, true), "two files and the stream temp");
        assert!(pending.is_empty() && job.is_none(), "the queued unit was cancelled and cleared");
        assert!(autoplay.is_none(), "and it is no longer waiting to be played");
        assert_eq!(std::fs::read_dir(&v).unwrap().count(), 0, "the voice folder is empty");
        assert!(other.exists(), "other voices are untouched");
        let _ = std::fs::remove_dir_all(audio.parent().unwrap());
    }

    /// Page mode with the first page done and the second still queued: deleting the section's
    /// audio takes the finished page and cancels the queued one, so it cannot come back.
    #[test]
    fn deleting_a_section_cancels_its_queued_pages() {
        let (_dir, b, audio) = audio_tree("section");
        let v = audio.join("kokoro-af_heart");
        let done = put(&v, "01_The_Quiet_Harbor_c0001_p0000.wav", &wav_bytes(4096));
        put(&v, "01_The_Quiet_Harbor_c0001_p0000.json", b"{}");
        let queued = v.join("01_The_Quiet_Harbor_c0001_p0001.wav");
        let other = v.join("00_Title_Page_c0000_p0000.wav"); // another section, also queued
        let tts = tts::Tts::start(vec!["no-such-engine".into()], None, eframe::egui::Context::default());
        let mut pending: HashMap<PathBuf, String> =
            HashMap::from([(queued.clone(), "The Quiet Harbor".into()), (other.clone(), "Title Page".into())]);
        let mut job = Some(Job { path: queued.clone(), progress: 0.4, started: Instant::now() });

        let mut autoplay = Some(queued.clone());
        let paths = section_audio_paths(&b, &v, 1, true);
        for p in &paths {
            halt_unit_audio(&mut None, &mut None, &mut pending, &mut job, &mut autoplay, p, &tts);
        }
        let r = delete_audio_files(&paths, &audio, &mut PlainRemove);
        assert_eq!((r.moved, r.failed.is_none()), (2, true), "the finished page and its timing file");
        assert!(!done.exists());
        assert!(!pending.contains_key(&queued) && job.is_none(), "the queued page was cancelled");
        assert!(autoplay.is_none(), "and it is no longer waiting to be played");
        assert!(pending.contains_key(&other), "other sections stay queued");
        let _ = std::fs::remove_dir_all(audio.parent().unwrap());
    }

    #[test]
    fn deleting_all_book_audio_keeps_the_book_itself() {
        let (dir, _b, audio) = audio_tree("book");
        put(&audio.join("kokoro-af_heart"), "00_Title_Page_c0000.wav", &wav_bytes(4096));
        put(&audio.join("kokoro-af_heart"), "00_Title_Page_c0000.json", b"{}");
        put(&audio.join("phonon-everly"), "c0001.wav", &wav_bytes(4096));
        put(&audio.join("phonon-everly"), "c0001.json", b"{}");
        let keep = put(&dir, "book.json", b"{}");
        put(&dir, "progress.json", br#"{"ch":0,"pg":0,"opened":0}"#);
        put(&dir, "cover.png", b"png");

        let tts = tts::Tts::start(vec!["no-such-engine".into()], None, eframe::egui::Context::default());
        let mut pending = HashMap::new();
        let mut autoplay = Some(PathBuf::from("books/another_book/audio/kokoro-af_heart/c0000.wav"));
        let r = clear_audio_dir(&audio, &audio, &mut None, &mut None, &mut pending, &mut None, &mut autoplay, &tts, &mut PlainRemove);
        assert_eq!((r.moved, r.failed.is_none()), (4, true), "every voice's files");
        assert!(keep.exists() && dir.join("progress.json").exists() && dir.join("cover.png").exists());
        assert!(!audio.join("kokoro-af_heart").join("00_Title_Page_c0000.wav").exists());
        assert!(autoplay.is_some(), "a unit of another book that waits to be played still waits");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cleaning_up_removes_only_leftovers() {
        let (_dir, b, audio) = audio_tree("clean");
        let v = audio.join("kokoro-af_heart");
        put(&v, "00_Title_Page_c0000.wav", &wav_bytes(4096));
        put(&v, "00_Title_Page_c0000.json", b"{}");
        put(&v, "00_Title_Page_c0000.wav.stream.pcm", b"pcm"); // interrupted stream
        put(&v, "09_Ghost_c0009.json", b"{}"); // timing with no wav

        let junk: Vec<PathBuf> = scan_leftovers_in(&b, &audio, &HashSet::new()).into_iter().map(|j| j.path).collect();
        let r = delete_audio_files(&junk, &audio, &mut PlainRemove);
        assert_eq!((r.moved, r.failed.is_none()), (2, true));
        assert!(v.join("00_Title_Page_c0000.wav").exists() && v.join("00_Title_Page_c0000.json").exists());
        assert!(!v.join("00_Title_Page_c0000.wav.stream.pcm").exists() && !v.join("09_Ghost_c0009.json").exists());
        let _ = std::fs::remove_dir_all(audio.parent().unwrap());
    }

    #[test]
    fn refuses_to_delete_outside_the_audio_folder() {
        let (dir, _b, audio) = audio_tree("refuse");
        let v = audio.join("kokoro-af_heart");
        let wav = put(&v, "00_Title_Page_c0000.wav", &wav_bytes(4096));
        let cover = put(&dir, "cover.png", b"png");

        // the cover sits outside the audio folder, reached directly and through ".."
        for escape in [dir.join("cover.png"), audio.join("..").join("cover.png")] {
            let r = delete_audio_files(&[escape], &audio, &mut PlainRemove);
            assert_eq!(r.moved, 0, "nothing is deleted");
            assert!(r.failed.is_some(), "the attempt is refused");
        }
        assert!(cover.exists(), "the outside file is untouched");
        assert!(wav.exists(), "and so is everything inside");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// After deleting a unit whose generation was queued or running, nothing marks it as
    /// wanted any more, so it can be queued again.
    #[test]
    fn deleting_a_queued_unit_can_be_queued_again() {
        let tts = tts::Tts::start(vec!["no-such-engine".into()], None, eframe::egui::Context::default());
        let wav = PathBuf::from("books/quiet_harbor-9f6689e3/audio/kokoro-af_heart/01_The_Quiet_Harbor_c0001.wav");
        let mut pending = HashMap::from([(wav.clone(), "The Quiet Harbor".into())]);
        let mut job = Some(Job { path: wav.clone(), progress: 0.4, started: Instant::now() });
        let mut feed = Some(Feed { out: wav.clone(), pcm: wav.clone(), read: 6 });
        let mut autoplay = Some(wav.clone());
        halt_unit_audio(&mut None, &mut feed, &mut pending, &mut job, &mut autoplay, &wav, &tts);
        assert!(pending.is_empty() && job.is_none() && feed.is_none() && autoplay.is_none());
    }

    #[test]
    fn profile_labels() {
        assert_eq!(config::profile_label("phonon-everly"), "Phonon · Everly");
        assert_eq!(config::profile_label("phonon-elodie-rose"), "Phonon · Elodie-rose");
        assert_eq!(config::profile_label("phonon"), "Phonon (original voice)");
        assert_eq!(config::profile_label("kokoro-af_heart"), "Kokoro · Heart (US ♀)");
        assert_eq!(config::profile_label("kokoro-bm_george"), "Kokoro · George (UK ♂)");
    }
}

/// Renders the real UI to PNGs (no window). Run with:
///   SHOTS=dir cargo test --release screenshots -- --ignored
/// The app's data and config folders are redirected to <dir>/data and <dir>/config (Linux
/// also honours XDG_DATA_HOME/XDG_CONFIG_HOME), and the run refuses to start unless the
/// redirect took effect, so the real library is never read or written.
#[cfg(test)]
mod screenshots {
    use super::*;
    use egui_kittest::Harness;

    /// Redirect the app to scratch folders inside the SHOTS dir and return false when the
    /// redirect did not take (on Windows the Known Folder API ignores LOCALAPPDATA and
    /// APPDATA, so only INKVOICE_DATA/INKVOICE_CONFIG actually work there). These tests
    /// clear the scratch library before they start, so the data folder must be one this
    /// function created: it leaves a marker file in a folder that was empty or absent, and
    /// refuses any other folder, such as a real library that happens to be at <SHOTS>/data.
    fn scratch(dir: &str) -> bool {
        let root = PathBuf::from(dir);
        let (data, config) = (root.join("data"), root.join("config"));
        let marker = data.join(".inkvoice-test-scratch");
        let fresh = std::fs::read_dir(&data).map_or(true, |mut d| d.next().is_none());
        let _ = std::fs::create_dir_all(&data);
        let _ = std::fs::create_dir_all(&config);
        if fresh {
            let _ = std::fs::write(&marker, b"");
        }
        if !marker.is_file() {
            eprintln!("screenshots: refusing to run, {} holds files and is not a test folder", data.display());
            return false;
        }
        std::env::set_var("INKVOICE_DATA", &data);
        std::env::set_var("INKVOICE_CONFIG", &config);
        let ok = book::data_dir().starts_with(&root) && book::config_dir().starts_with(&root);
        if !ok {
            eprintln!(
                "screenshots: refusing to run, data is {} and config is {}",
                book::data_dir().display(),
                book::config_dir().display()
            );
        }
        ok
    }

    /// A tiny original book (four sections of four short pages, plus short front and back
    /// matter) inside the scratch data, so no real library is ever read. Nguyen appears so
    /// the regenerate offer has a hit.
    fn fixture_book() -> book::Book {
        let line = |sec: usize, p: usize, n: usize| {
            format!("Nguyen {sec} counted {n} lanterns on page {p}, and the tide answered none of it.")
        };
        let body = (1..=4).map(|sec| {
            book::Chapter::new(
                format!("Section {sec}"),
                (1..=4).map(|p| (0..6).map(|n| line(sec, p, n)).collect::<Vec<_>>().join(" ")).collect(),
            )
        });
        let extra = |title: &str, text: &str| book::Chapter::new(title.into(), vec![text.into()]);
        let mut chapters = vec![
            extra("Title Page", "The Salt Road"),
            extra("Praise for The Salt Road", "A bright, patient novel of salt and weather. Nguyen writes harbors the way others write cities."),
        ];
        chapters.extend(body);
        chapters.push(extra("About the Author", "Nguyen grew up between two ferry lines and writes about both."));
        book::Book {
            id: "shots_book".into(),
            title: "The Salt Road".into(),
            author: String::new(),
            source: String::new(),
            chapters,
            pos: (0, 0),
            opened: 0,
            listen: None,
        }
    }

    /// The two screenshot tests share one scratch data folder; this keeps them from
    /// running at the same time and stepping on each other's book.
    static SHOTS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    #[ignore]
    fn screenshots() {
        let Ok(dir) = std::env::var("SHOTS") else { return };
        if !scratch(&dir) {
            return;
        }
        let _lock = SHOTS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // exactly one book per run: a leftover of an earlier run would crowd the library
        // and the details panel (the fixture id is legacy-form, so the app renames the
        // folder at start-up and a second run would add a second folder)
        let _ = std::fs::remove_dir_all(book::books_dir());
        let mut book = fixture_book();
        book.chapters[4].title = "Section 3 - The Long Crossing Home from the Islands".into();
        book.save().unwrap();
        // audio of an earlier run would make every page "ready"; start from scratch
        let _ = std::fs::remove_dir_all(book::books_dir().join("shots_book").join("audio"));
        // the audio manager needs something to show: a few small wavs in two voice folders
        // (the default voice first, so its rows offer regenerate), one long title, and one
        let wav = |bytes: usize| crate::tests::wav_bytes(bytes);
        let audio = book::books_dir().join("shots_book/audio");
        for (profile, ch, bytes) in [
            ("kokoro-af_heart", 2usize, 1_258_291usize),
            ("kokoro-af_heart", 3, 943_718),
            ("kokoro-af_heart", 4, 1_677_721),
            ("phonon-everly", 2, 1_153_433),
        ] {
            let dir = audio.join(profile);
            std::fs::create_dir_all(&dir).unwrap();
            let wav_p = dir.join(format!("{}.wav", unit_stem(&book, ch, None)));
            std::fs::write(&wav_p, wav(bytes)).unwrap();
            std::fs::write(wav_p.with_extension("json"), br#"{"duration": 12.3, "words": []}"#).unwrap();
        }
        std::fs::write(
            audio.join("kokoro-af_heart").join(format!("{}.wav.stream.pcm", unit_stem(&book, 5, None))),
            vec![0u8; 209_715],
        )
        .unwrap();
        let save = |h: &mut Harness<App>, name: &str| {
            h.run_steps(4);
            h.render().unwrap().save(format!("{dir}/{name}.png")).unwrap();
        };
        let mut h = Harness::builder().with_size([1280.0, 800.0]).build_eframe(|cc| App::new(cc, None));
        let id = h.state().lib[0].id.clone();
        let shot_ch: usize = std::env::var("SHOT_CH").ok().and_then(|c| c.parse().ok()).unwrap_or(3); // a body section; the fixture now opens with two skipped sections
        h.state_mut().sel = Some(id.clone());
        save(&mut h, "1_library");

        // the audio manager: rows with sizes and dates, then its delete confirmation
        save(&mut h, "audio_manager");
        h.state_mut().ask_del_all(&id, DelAll::Voice("kokoro-af_heart".into()));
        save(&mut h, "audio_delete_confirm");
        h.state_mut().confirm_del_audio = None;

        // the export window: M4B offered (ffmpeg on the PATH), mid-run, without ffmpeg, and done
        h.state_mut().open_export(&id, "kokoro-af_heart");
        let long_dest = PathBuf::from(&dir).join("Audiobooks to take along on the long ferry ride home");
        if let Some(w) = h.state_mut().export.as_mut() {
            w.dest = Some(long_dest.clone());
        }
        save(&mut h, "export_window");
        let s = h.state_mut();
        if let Some(w) = &mut s.export {
            w.busy = Some(export::Busy {
                cancel: Arc::new(AtomicBool::new(false)),
                frac: 0.62,
                label: "Salt and Weather · 2 of 3".into(),
                cancelling: false,
            });
        }
        save(&mut h, "export_running");
        if let Some(w) = h.state_mut().export.as_mut() {
            w.busy = None;
            w.ffmpeg = None;
        }
        save(&mut h, "export_no_ffmpeg");
        let s = h.state_mut();
        if let Some(w) = &mut s.export {
            w.ffmpeg = export::find_ffmpeg();
            w.m4b = true;
            w.result = Some(Ok((long_dest.join("The Salt Road.m4b"), true)));
        }
        save(&mut h, "export_done");
        h.state_mut().export = None;

        let s = h.state_mut();
        s.open_reader(&id);
        s.cfg.page_mode = true;
        s.go(shot_ch, 0);
        s.play_unit(Unit { book: id.clone(), ch: shot_ch, pg: Some(0) }, None);
        if let Some(p) = &mut s.player {
            p.seek(14.0);
        }
        save(&mut h, "2_reader_playing");

        let s = h.state_mut();
        let fake = std::path::PathBuf::from("/tmp/fake.wav");
        s.pending.insert(fake.clone(), "CHAPTER TWO".into());
        s.pending.insert("/tmp/fake2.wav".into(), "CHAPTER THREE".into());
        s.job = Some(Job { path: fake, progress: 0.42, started: Instant::now() - std::time::Duration::from_secs(40) });
        save(&mut h, "3_generating");

        h.state_mut().show_settings = true;
        let engine = h.state().cfg.engine.clone();
        h.state_mut().cfg.engine = "woof".into();
        h.state_mut().cfg.set_adv("temp", Some("0.650".into()));
        // pronunciation helper on, with two example entries in the table
        let s = h.state_mut();
        s.cfg.normalize.enabled = true;
        s.pron.set("doppelgänger", "doppel ganger").unwrap();
        s.pron.set("Nguyen", "nwin").unwrap();
        s.pron.save().unwrap();
        s.pron_state = config::Pronounce::file_state();
        save(&mut h, "4_settings");
        h.state_mut().cfg.set_adv("temp", None);
        h.state_mut().cfg.engine = engine;

        // streaming: play a page that has no audio yet; it must start before generation ends
        let s = h.state_mut();
        s.show_settings = false;
        s.pending.clear();
        s.job = None;
        s.cfg.speed = 1.25;
        let stream_pg = s.book(&id).map_or(3, |b| b.chapters[shot_ch].pages.len().saturating_sub(1).clamp(1, 3));
        // a previous run of this test may have left the page's audio behind; drop it so
        // this really streams
        let stream_unit = Unit { book: id.clone(), ch: shot_ch, pg: Some(stream_pg) };
        if let Some(p) = s.unit_path(&stream_unit) {
            let _ = std::fs::remove_file(&p);
            let _ = std::fs::remove_file(player::timing_path(&p));
            s.have.remove(&p);
        }
        s.go(shot_ch, stream_pg);
        let t0 = Instant::now();
        s.play_unit(stream_unit, None);
        let mut started = None;
        while t0.elapsed().as_secs() < 90 {
            h.run_steps(1);
            std::thread::sleep(std::time::Duration::from_millis(100));
            let s = h.state();
            let p = s.player.as_ref().unwrap();
            if started.is_none() && p.now.as_ref().is_some_and(|n| n.streaming) && !p.is_buffering() && p.pos() > 0.5 {
                started = Some(t0.elapsed().as_secs_f32());
                eprintln!("STREAM: playing after {:.1}s, buffered {:.1}s, still generating: {}", started.unwrap(), p.buffered(), s.job.is_some());
                save(&mut h, "5_streaming");
            }
            if started.is_some() && h.state().job.is_none() {
                let p = h.state().player.as_ref().unwrap();
                eprintln!("STREAM: generation done after {:.1}s; switched to final file: {}; pos {:.1}s; words {}",
                    t0.elapsed().as_secs_f32(), !p.now.as_ref().unwrap().streaming, p.pos(), p.now.as_ref().unwrap().words.len());
                break;
            }
        }
        assert!(started.is_some(), "streaming never started");


        // install window: working, success (confetti mid-air), failure
        let step = |l: &str, w: f32| installer::StepInfo { label: l.into(), weight: w };
        let s = h.state_mut();
        if let Some(p) = &mut s.player {
            p.stop();
        }
        s.install = Some(installer::dummy("kokoro"));
        s.inst.plan = vec![
            step("Download the installer tool (uv)", 18.0),
            step("Set up a private Python", 33.0),
            step("Install Kokoro software", 50.0),
            step("Download the Kokoro voice model", 120.0),
        ];
        s.inst.step = 2;
        s.inst.frac = 0.55;
        s.inst.detail = "27.4 of 48.3 MB".into();
        s.inst.target = (18.0 + 33.0 + 50.0 * 0.55) / 221.0;
        s.inst.shown = s.inst.target;
        s.inst.started = Instant::now() - std::time::Duration::from_secs(40);
        s.inst.visible = true;
        save(&mut h, "6_installing");
        let s = h.state_mut();
        s.install = None;
        s.inst.result = Some(("kokoro".into(), InstallOutcome::Ok, Instant::now() - std::time::Duration::from_millis(650)));
        h.run_steps(2);
        h.render().unwrap().save(format!("{dir}/7_installed.png")).unwrap();
        let s = h.state_mut();
        s.inst.result = Some(("kokoro".into(), InstallOutcome::Failed("curl: (6) Could not resolve host: github.com".into()), Instant::now()));
        save(&mut h, "8_install_failed");

        // the reader's Set pronunciation window over the page, saved, offering regeneration
        let s = h.state_mut();
        s.inst.result = None;
        s.show_settings = false;
        s.cfg.normalize.enabled = true;
        s.pron_set("Nguyen", "nwin", true).unwrap();
        s.pron_win = Some(PronWin { word: "Nguyen".into(), say: "nwin".into(), saved: true, err: None });
        save(&mut h, "9_set_pronunciation");
    }

    /// Asking to listen while another chapter is being generated must start right away
    /// (the background chapter is paused and resumed), and a double-click must not restart it.
    #[test]
    #[ignore]
    fn listen_now_during_background_generation() {
        let Ok(dir) = std::env::var("SHOTS") else { return };
        if !scratch(&dir) {
            return;
        }
        let _lock = SHOTS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ = std::fs::remove_dir_all(book::books_dir());
        fixture_book().save().unwrap();
        let mut h = Harness::builder().with_size([1280.0, 800.0]).build_eframe(|cc| App::new(cc, None));
        let id = h.state().lib[0].id.clone();
        let shot_ch: usize = std::env::var("SHOT_CH").ok().and_then(|c| c.parse().ok()).unwrap_or(3);
        h.state_mut().open_reader(&id);
        h.state_mut().cfg.page_mode = true;
        if let Ok(e) = std::env::var("SHOT_ENGINE") {
            h.state_mut().cfg.engine = e;
            h.state_mut().worker_dirty = true;
            h.state_mut().apply_worker_settings();
        }
        let bg = Unit { book: id.clone(), ch: shot_ch + 1, pg: Some(1) };
        let target = Unit { book: id.clone(), ch: shot_ch + 2, pg: Some(0) };
        for u in [&bg, &target] {
            let p = h.state().unit_path(u).unwrap();
            let _ = std::fs::remove_file(&p);
            h.state_mut().have.remove(&p);
        }
        h.state_mut().enqueue(&bg);
        let t_bg = Instant::now();
        while h.state().job.is_none() && t_bg.elapsed().as_secs() < 30 {
            h.run_steps(1);
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        std::thread::sleep(std::time::Duration::from_millis(800));
        let t1 = Instant::now();
        h.state_mut().play_from_char(target.clone(), Some(0));
        h.state_mut().play_from_char(target.clone(), Some(0)); // double-click
        let mut heard = None;
        while t1.elapsed().as_secs() < 60 {
            h.run_steps(1);
            std::thread::sleep(std::time::Duration::from_millis(100));
            let p = h.state().player.as_ref().unwrap();
            if p.now.as_ref().is_some_and(|n| n.unit == target) && !p.is_buffering() && p.pos() > 0.3 {
                heard = Some(t1.elapsed().as_secs_f32());
                break;
            }
        }
        let bg_path = h.state().unit_path(&bg).unwrap();
        eprintln!("PLAY-NOW: listening after {heard:?}s; background chapter still queued: {}", h.state().pending.contains_key(&bg_path));
        assert!(heard.is_some_and(|t| t < 20.0), "listening should start while background work is paused");
        assert!(h.state().pending.contains_key(&bg_path), "background chapter kept in the queue");
        if let Some(p) = &mut h.state_mut().player {
            p.stop();
        }
        h.state_mut().cancel_generation();
    }
}
