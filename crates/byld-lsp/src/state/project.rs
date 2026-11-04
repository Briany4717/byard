//! The project a document belongs to (RFC-0008): the package root, its
//! sources held in memory, and the analysis that tells what is wrong with one
//! file of it.
//!
//! `byard check` answers for a whole project; the editor asks about one file
//! of it, many times a second. A file is only an error or not in the context
//! of its siblings (a view defined next door, a function, a record type, a
//! theme token the manifest declares, a native view the Rust half draws), so
//! this runs the same resolve and the same check as the command, over the same
//! set of files, and keeps only what concerns the file in front of the person.
//!
//! The project is read from disk when it is first needed and again only when
//! something says it changed (a save, a watched file, a file opening): never
//! on a keystroke. What a keystroke changes is the text of the open buffers,
//! and those win over the disk wherever they differ.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use byard_compiler::CompileError;
use byard_compiler::interp::theme::Theme;
use byard_compiler::parser::ast::ViewDecl;
use byard_compiler::resolve::{PackageProvider, SourceFile, resolve_program};
use byard_project::check::check_views_where;
use byard_project::deps::{FsProvider, Lockfile, declare_native_views, project_source_paths};
use byard_project::manifest::Manifest;

use super::document::{Document, DocumentStore};

/// What the file system says about a file without reading it: when it was
/// last written, and how long it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    modified: Option<SystemTime>,
    len: u64,
}

fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some(Stamp {
        modified: meta.modified().ok(),
        len: meta.len(),
    })
}

/// Whether a file that carries the stamp `then` and now carries `now` can be
/// taken to be the same file. A file written within the clock's granularity
/// of the stamp could have been written again, to the same length, without
/// the stamp moving, so a stamp that young proves nothing and the file is read.
fn unchanged(then: Option<Stamp>, now: Option<Stamp>) -> bool {
    const SETTLED: std::time::Duration = std::time::Duration::from_secs(2);
    match (then, now) {
        // A file that was not there and is still not.
        (None, None) => true,
        (Some(a), Some(b)) => {
            a == b
                && a.modified
                    .and_then(|m| m.elapsed().ok())
                    .is_some_and(|age| age >= SETTLED)
        }
        _ => false,
    }
}

/// One `.byd` file of the project as last read from disk.
struct SourceEntry {
    /// The name the resolver shows for it (`main.byd`, `ui/card.byd`).
    name: String,
    path: PathBuf,
    stamp: Option<Stamp>,
    text: String,
}

/// Hands the resolver the packages a project depends on, asking the file
/// system once for each and remembering the answer. A package is hundreds of
/// kilobytes, a git or registry one is verified by a checksum of all of it,
/// and neither changes between two keystrokes.
struct MemoPackages {
    inner: FsProvider,
    memo: HashMap<(String, String), Result<Vec<SourceFile>, String>>,
}

impl PackageProvider for MemoPackages {
    fn package_files(&mut self, dependent: &str, package: &str) -> Result<Vec<SourceFile>, String> {
        let key = (dependent.to_string(), package.to_string());
        if let Some(answer) = self.memo.get(&key) {
            return answer.clone();
        }
        let answer = self.inner.package_files(dependent, package);
        self.memo.insert(key, answer.clone());
        answer
    }
}

/// A file with no `byard.toml` has no dependencies to give, the same answer
/// `byard check file.byd` gives.
struct NoPackages;

impl PackageProvider for NoPackages {
    fn package_files(
        &mut self,
        _dependent: &str,
        _package: &str,
    ) -> Result<Vec<SourceFile>, String> {
        Err("a bare `.byd` file has no `[dependencies]`; create a byard.toml".to_string())
    }
}

/// A package on disk: its manifest and every `.byd` file `byard check` would
/// read for it.
pub struct Project {
    manifest: Arc<Manifest>,
    /// Stamps of `byard.toml` and `byard.lock` when the manifest was read.
    config_stamps: [Option<Stamp>; 2],
    entries: Vec<SourceEntry>,
    packages: Mutex<MemoPackages>,
}

impl Project {
    /// Reads the project whose manifest is `manifest_path`. What `previous`
    /// still has right (a manifest whose files did not change, the text of a
    /// file whose stamp did not) is kept rather than read again.
    fn load(manifest_path: &Path, previous: Option<&Project>) -> Result<Self, String> {
        let root = manifest_path.parent().unwrap_or(Path::new("."));
        let config_stamps = [stamp(manifest_path), stamp(&root.join("byard.lock"))];

        let manifest = match previous {
            Some(p)
                if unchanged(p.config_stamps[0], config_stamps[0])
                    && unchanged(p.config_stamps[1], config_stamps[1]) =>
            {
                Arc::clone(&p.manifest)
            }
            _ => {
                let manifest =
                    Manifest::discover(Some(manifest_path)).map_err(|e| e.to_string())?;
                // A tool that cannot link the app's Rust still knows the
                // elements it draws.
                declare_native_views(&manifest);
                Arc::new(manifest)
            }
        };

        let mut entries = Vec::new();
        for (name, path) in project_source_paths(&manifest).map_err(|e| e.to_string())? {
            let now = stamp(&path);
            let kept = previous.and_then(|p| {
                p.entries
                    .iter()
                    .find(|e| e.path == path && e.stamp.is_some() && unchanged(e.stamp, now))
            });
            let text = match kept {
                Some(e) => e.text.clone(),
                // A file that vanished between the walk and the read is a
                // file with nothing in it; the next refresh sees it gone.
                None => std::fs::read_to_string(&path).unwrap_or_default(),
            };
            entries.push(SourceEntry {
                name,
                path,
                stamp: now,
                text,
            });
        }

        let lock = Lockfile::read(&manifest.project_root).map_err(|e| e.to_string())?;
        let packages = Mutex::new(MemoPackages {
            inner: FsProvider::new(&manifest, lock),
            memo: HashMap::new(),
        });
        Ok(Self {
            manifest,
            config_stamps,
            entries,
            packages,
        })
    }

    /// Where `path` is in the project's file list, if it is one of the files
    /// `byard check` reads.
    fn index_of(&self, path: &Path) -> Option<usize> {
        self.entries.iter().position(|e| e.path == path)
    }

    /// The files of the program, a buffer that is open standing for its file.
    fn sources(&self, doc: &Document, open: &[Arc<Document>]) -> Vec<SourceFile> {
        self.entries
            .iter()
            .map(|entry| {
                let source = if doc.path.as_deref() == Some(entry.path.as_path()) {
                    doc.content.clone()
                } else if let Some(buffer) = open
                    .iter()
                    .find(|d| d.path.as_deref() == Some(entry.path.as_path()))
                {
                    buffer.content.clone()
                } else {
                    entry.text.clone()
                };
                SourceFile {
                    name: entry.name.clone(),
                    source,
                }
            })
            .collect()
    }
}

/// How the cache stands on a project: not loaded yet, or read and still good,
/// or read and due to be read again.
struct Slot {
    loaded: Result<Arc<Project>, String>,
    stale: bool,
}

#[derive(Default)]
struct Inner {
    /// By the path of the project's `byard.toml`.
    projects: HashMap<PathBuf, Slot>,
    /// The `byard.toml` above a directory, if any: the walk up is made once
    /// per directory, not once per keystroke.
    manifests: HashMap<PathBuf, Option<PathBuf>>,
}

/// The projects the server has read, by package root.
#[derive(Default)]
pub struct ProjectCache {
    inner: Mutex<Inner>,
}

/// Something to tell the person that is not an error of the file's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// A warning rather than information.
    pub warning: bool,
    /// What to say.
    pub message: String,
}

/// What is wrong with one file.
#[derive(Debug, Default)]
pub struct Analysis {
    /// The compiler's diagnostics for the file, their spans in the file's own
    /// text.
    pub errors: Vec<CompileError>,
    /// What the person should know about how those were reached.
    pub notices: Vec<Notice>,
}

impl ProjectCache {
    /// An empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Says that something on disk may have changed (a file was saved, a
    /// watched file was created or removed): every project is read again the
    /// next time it is asked for, and only the files that changed are re-read.
    pub fn invalidate(&self) {
        let mut inner = self.lock();
        for slot in inner.projects.values_mut() {
            slot.stale = true;
        }
        inner.manifests.clear();
    }

    /// The path of the `byard.toml` the document at `path` is governed by:
    /// the nearest one above it, which is where `byard` itself looks.
    #[must_use]
    pub fn manifest_of(&self, path: &Path) -> Option<PathBuf> {
        let dir = path.parent()?;
        let mut inner = self.lock();
        if let Some(found) = inner.manifests.get(dir) {
            return found.clone();
        }
        let found = dir
            .ancestors()
            .map(|d| d.join("byard.toml"))
            .find(|candidate| candidate.is_file());
        inner.manifests.insert(dir.to_path_buf(), found.clone());
        found
    }

    fn project(&self, manifest: &Path) -> Result<Arc<Project>, String> {
        let mut inner = self.lock();
        let previous = match inner.projects.get(manifest) {
            Some(slot) if !slot.stale => return slot.loaded.clone(),
            Some(slot) => slot.loaded.as_ref().ok().cloned(),
            None => None,
        };
        let loaded = Project::load(manifest, previous.as_deref()).map(Arc::new);
        inner.projects.insert(
            manifest.to_path_buf(),
            Slot {
                loaded: loaded.clone(),
                stale: false,
            },
        );
        loaded
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// What is wrong with `doc`, in the context of the project it belongs to
    /// and of the other buffers that are open.
    ///
    /// The same answer `byard check` gives for the project, narrowed to this
    /// file. A file that is not part of a project, or whose project cannot be
    /// read, is checked as `byard check file.byd` would, or, when its project
    /// is broken, for syntax alone: with its siblings out of reach, anything
    /// else it said would be a guess.
    #[must_use]
    pub fn analyze(&self, doc: &Document, open: &DocumentStore) -> Analysis {
        guarded(doc, || self.analyze_unguarded(doc, open))
    }

    fn analyze_unguarded(&self, doc: &Document, open: &DocumentStore) -> Analysis {
        let Some(path) = doc.path.as_deref() else {
            return analyze_alone(doc);
        };
        let Some(manifest) = self.manifest_of(path) else {
            return analyze_alone(doc);
        };
        match self.project(&manifest) {
            Ok(project) => match project.index_of(path) {
                Some(index) => analyze_in(&project, index, doc, &open.all()),
                None => analyze_alone(doc),
            },
            Err(message) => Analysis {
                errors: doc.parsed.errors.clone(),
                notices: vec![Notice {
                    warning: true,
                    message: format!(
                        "{} could not be read, so only the syntax of this file is checked: \
                         {message}",
                        manifest.display()
                    ),
                }],
            },
        }
    }
}

/// Runs `analysis`, and if the checker panics on this text, which is a bug
/// in it and never the person's doing, says so and keeps the server alive:
/// one file the checker cannot take must not end the session of every file.
fn guarded(doc: &Document, analysis: impl FnOnce() -> Analysis) -> Analysis {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(analysis)).unwrap_or_else(|_| Analysis {
        errors: doc.parsed.errors.clone(),
        notices: vec![Notice {
            warning: true,
            message: "the checker stopped on this file, so only its syntax is checked; \
                      that is a bug in byard, not in the file"
                .to_string(),
        }],
    })
}

/// `byard check file.byd`: the file on its own, against the base theme.
#[must_use]
pub fn analyze_alone(doc: &Document) -> Analysis {
    let name = doc.path.as_deref().and_then(Path::file_name).map_or_else(
        || "main.byd".to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let program = resolve_program(
        vec![SourceFile {
            name,
            source: doc.content.clone(),
        }],
        &mut NoPackages,
    );
    let errors = check_views_where(&program, Theme::byard_base(), |_| true);
    // One file, at offset zero: its spans are already its own.
    Analysis {
        errors,
        notices: Vec::new(),
    }
}

/// File number `index` of `project`, as `byard check` reads the project, with
/// the open buffers in place of their files.
fn analyze_in(project: &Project, index: usize, doc: &Document, open: &[Arc<Document>]) -> Analysis {
    let program = {
        let mut packages = project
            .packages
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        resolve_program(project.sources(doc, open), &mut *packages)
    };
    // The root project's files come first in the program's source map, in the
    // order they were given.
    let Some(entry) = program.source_map.files().nth(index) else {
        return Analysis::default();
    };
    let (base, end) = (entry.base, entry.base + entry.source.len() as u32);
    let inside = |start: u32| start >= base && start <= end;

    let errors = check_views_where(
        &program,
        project.manifest.theme.clone(),
        |view: &ViewDecl| inside(view.span.start),
    );
    let mut notices = Vec::new();
    let mut mine = Vec::new();
    for mut error in errors {
        if inside(error.span().start) {
            error.shift_span(-i64::from(base));
            mine.push(error);
        }
    }
    // The program held back its semantic checks because of an error in
    // another file: say so, or the file looks fine when it is only unchecked.
    if mine.is_empty() {
        if let Some(other) = program.errors.first() {
            notices.push(Notice {
                warning: false,
                message: format!(
                    "the checks of this file wait on an error elsewhere in the project: {}",
                    program.source_map.render_line(other)
                ),
            });
        }
    }
    Analysis {
        errors: mine,
        notices,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::Uri;

    /// A project on disk, removed when the test ends.
    pub(crate) struct Dir(pub PathBuf);

    impl Dir {
        pub(crate) fn new(tag: &str) -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("byld-lsp-{tag}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir.canonicalize().unwrap())
        }

        /// Writes a file and dates it `age` seconds ago, old enough for the
        /// cache to trust its stamp.
        pub(crate) fn write_aged(&self, rel: &str, text: &str, age: u64) -> PathBuf {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
            let then = SystemTime::now() - std::time::Duration::from_secs(age);
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(then)
                .unwrap();
            path
        }

        pub(crate) fn write(&self, rel: &str, text: &str) -> PathBuf {
            self.write_aged(rel, text, 60)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub(crate) fn uri_of(path: &Path) -> Uri {
        url::Url::from_file_path(path)
            .unwrap()
            .as_str()
            .parse()
            .unwrap()
    }

    fn open(store: &DocumentStore, path: &Path, text: &str) -> Arc<Document> {
        store.insert(uri_of(path), Some(1), text.to_string())
    }

    const MAIN: &str = "View Main() {\n  Card()\n}\n";
    const CARD: &str = "View Card() {\n  Text(\"hi\")\n}\n";

    fn project() -> (Dir, PathBuf, PathBuf) {
        let dir = Dir::new("project");
        dir.write(
            "byard.toml",
            "[project]\nname = \"p\"\nentry = \"main.byd\"\n",
        );
        let main = dir.write("main.byd", MAIN);
        let card = dir.write("card.byd", CARD);
        (dir, main, card)
    }

    #[test]
    fn a_view_defined_in_a_sibling_file_is_known() {
        let (_dir, main, _card) = project();
        let store = DocumentStore::new();
        let doc = open(&store, &main, MAIN);
        let cache = ProjectCache::new();
        let analysis = cache.analyze(&doc, &store);
        assert!(analysis.errors.is_empty(), "{:?}", analysis.errors);
    }

    #[test]
    fn the_same_file_on_its_own_does_not_know_the_sibling() {
        let (_dir, main, _card) = project();
        let doc = open(&DocumentStore::new(), &main, MAIN);
        let analysis = analyze_alone(&doc);
        assert_eq!(analysis.errors.len(), 1, "{:?}", analysis.errors);
        assert_eq!(analysis.errors[0].kind(), "UnknownView");
    }

    #[test]
    fn an_open_buffer_wins_over_the_disk() {
        let (_dir, main, card) = project();
        let store = DocumentStore::new();
        let doc = open(&store, &main, MAIN);
        let cache = ProjectCache::new();
        assert_eq!(cache.analyze(&doc, &store).errors, []);

        // The sibling is renamed in its buffer, unsaved: the disk still says
        // `Card`, the person's text says `Tile`, and the person's text is
        // what the program means.
        open(&store, &card, "View Tile() {\n  Text(\"hi\")\n}\n");
        let analysis = cache.analyze(&doc, &store);
        assert_eq!(analysis.errors.len(), 1, "{:?}", analysis.errors);
        assert_eq!(analysis.errors[0].kind(), "UnknownView");
    }

    #[test]
    fn a_keystroke_does_not_touch_the_disk() {
        let (_dir, main, card) = project();
        let store = DocumentStore::new();
        let cache = ProjectCache::new();
        let doc = open(&store, &main, MAIN);
        assert_eq!(cache.analyze(&doc, &store).errors, []);

        // The sibling changes on disk and nobody says so (no save, no watched
        // file): the cache does not look. That is the point, a keystroke reads
        // nothing.
        std::fs::write(&card, "View Tilee() {\n  Text(\"hi\")\n}\n").unwrap();
        let doc = open(&store, &main, &format!("{MAIN}\n"));
        assert!(
            cache.analyze(&doc, &store).errors.is_empty(),
            "the keystroke must have been answered from memory"
        );

        // A save is how it hears.
        cache.invalidate();
        let analysis = cache.analyze(&doc, &store);
        assert_eq!(analysis.errors.len(), 1, "{:?}", analysis.errors);
    }

    #[test]
    fn errors_are_those_of_the_file_asked_about() {
        let (dir, main, card) = project();
        let bad = "View Card() {\n  Text(\"hi\") #[nope: 1]\n}\n";
        dir.write("card.byd", bad);
        let store = DocumentStore::new();
        let cache = ProjectCache::new();

        let card_doc = open(&store, &card, bad);
        let in_card = cache.analyze(&card_doc, &store);
        assert_eq!(in_card.errors.len(), 1, "{:?}", in_card.errors);
        assert_eq!(in_card.errors[0].kind(), "UnknownAttribute");
        // Its span is in the card's own text.
        let span = in_card.errors[0].span();
        assert!(bad[span.start as usize..].starts_with("nope"), "{span:?}");

        let main_doc = open(&store, &main, MAIN);
        let in_main = cache.analyze(&main_doc, &store);
        assert!(in_main.errors.is_empty(), "{:?}", in_main.errors);
    }

    #[test]
    fn a_syntax_error_next_door_is_said_not_swallowed() {
        let (dir, main, card) = project();
        dir.write("card.byd", "View Card( {\n");
        let store = DocumentStore::new();
        let doc = open(&store, &main, MAIN);
        let _ = card;
        let analysis = ProjectCache::new().analyze(&doc, &store);
        assert!(analysis.errors.is_empty(), "{:?}", analysis.errors);
        assert_eq!(analysis.notices.len(), 1, "{:?}", analysis.notices);
        assert!(
            analysis.notices[0].message.contains("card.byd"),
            "{:?}",
            analysis.notices
        );
    }

    #[test]
    fn a_manifest_that_cannot_be_read_leaves_the_syntax_and_a_warning() {
        let dir = Dir::new("broken");
        dir.write("byard.toml", "[project\n");
        let main = dir.write("main.byd", "View Main() {\n  Nope()\n}\n");
        let store = DocumentStore::new();
        let doc = open(&store, &main, "View Main() {\n  Nope()\n}\n");
        let analysis = ProjectCache::new().analyze(&doc, &store);
        // `Nope` may well be defined in a sibling; saying it is not would be
        // a guess.
        assert!(analysis.errors.is_empty(), "{:?}", analysis.errors);
        assert_eq!(analysis.notices.len(), 1);
        assert!(analysis.notices[0].warning);
    }

    #[test]
    fn a_file_outside_the_programs_files_is_checked_alone() {
        let (dir, _main, _card) = project();
        // The entry's directory is the program; a `.byd` under a hidden
        // directory is not part of it, and `byard check` would not read it.
        let hidden = dir.write(".scratch/x.byd", "View X() {\n  Card()\n}\n");
        let store = DocumentStore::new();
        let doc = open(&store, &hidden, "View X() {\n  Card()\n}\n");
        let analysis = ProjectCache::new().analyze(&doc, &store);
        assert_eq!(analysis.errors.len(), 1, "{:?}", analysis.errors);
    }

    #[test]
    fn only_files_that_changed_are_read_again() {
        let (dir, main, card) = project();
        let cache = ProjectCache::new();
        let manifest = cache.manifest_of(&main).unwrap();
        let first = cache.project(&manifest).unwrap();

        // `card.byd` is rewritten with other text of the same length, and
        // dated as it was: the stamp cannot tell, so the cache keeps what it
        // read, which shows it did not read the file again.
        let before = stamp(&card).unwrap();
        dir.write_aged("card.byd", &CARD.replace("hi", "yo"), 60);
        let after = stamp(&card).unwrap();
        assert_eq!(before.len, after.len);
        std::fs::File::options()
            .write(true)
            .open(&card)
            .unwrap()
            .set_modified(before.modified.unwrap())
            .unwrap();

        // `main.byd` really changes: new length, new date.
        dir.write_aged("main.byd", &format!("{MAIN}\n"), 30);

        cache.invalidate();
        let second = cache.project(&manifest).unwrap();
        assert!(
            !Arc::ptr_eq(&first, &second),
            "a stale project is read again"
        );
        assert!(
            Arc::ptr_eq(&first.manifest, &second.manifest),
            "a manifest whose files did not change is kept"
        );
        let text = |p: &Project, name: &str| {
            p.entries
                .iter()
                .find(|e| e.name == name)
                .map(|e| e.text.clone())
                .unwrap()
        };
        assert_eq!(text(&second, "card.byd"), CARD, "unchanged stamp, not read");
        assert_eq!(text(&second, "main.byd"), format!("{MAIN}\n"), "read again");
    }

    #[test]
    fn a_checker_that_panics_does_not_take_the_server_with_it() {
        let doc = Document::new(
            "file:///t.byd".parse().unwrap(),
            Some(1),
            "View Main( {\n".to_string(),
        );
        let analysis = guarded(&doc, || panic!("a bug in the checker"));
        assert_eq!(analysis.errors.len(), doc.parsed.errors.len());
        assert!(!analysis.errors.is_empty(), "the syntax is still reported");
        assert_eq!(analysis.notices.len(), 1);
        assert!(analysis.notices[0].warning);
    }

    #[test]
    fn a_file_created_since_is_part_of_the_project_after_a_refresh() {
        let (dir, main, _card) = project();
        let cache = ProjectCache::new();
        let manifest = cache.manifest_of(&main).unwrap();
        assert_eq!(cache.project(&manifest).unwrap().entries.len(), 2);
        dir.write("extra.byd", "View Extra() {\n  Text(\"x\")\n}\n");
        assert_eq!(
            cache.project(&manifest).unwrap().entries.len(),
            2,
            "nothing said it changed"
        );
        cache.invalidate();
        assert_eq!(cache.project(&manifest).unwrap().entries.len(), 3);
    }
}
