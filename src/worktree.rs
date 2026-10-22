//! Worktrees created by `aisc start --path`: the registry under the Git common dir, removal after
//! `finish`, and `aisc gc`. Only registered worktrees whose Git admin dir carries a matching AISC
//! session marker are ever removed, always with `git worktree remove` (never forced). The task
//! branch is kept, and a HEAD no ref contains is saved under `refs/aisc/rescue/` first, so no
//! commit becomes unreachable.
use crate::git::{Repo, run_git, text};
use crate::task::{self, Session};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod gc;
pub(crate) use gc::gc;

/// Wall-clock budget for one size walk; larger trees are reported as a lower bound.
const SIZE_BUDGET: Duration = Duration::from_secs(10);
const OPERATION_MARKERS: &[&str] = &[
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "rebase-merge",
    "rebase-apply",
    "sequencer",
];
/// Ignored paths named in a refusal before the list is shortened.
const SAMPLE_SHOWN: usize = 3;
/// Ignored entries inspected before the scan gives up and keeps the worktree (fail closed).
const IGNORED_ENTRY_BUDGET: usize = 2_000_000;
const SCAN_BUDGET: Duration = Duration::from_secs(30);

#[derive(Serialize, Deserialize, Clone, Default)]
pub(crate) struct Registration {
    pub name: String,
    pub path: String,
    pub branch: String,
    pub base: String,
    pub base_branch: Option<String>,
    pub created_at: u64,
    pub aisc_version: String,
    pub command: Vec<String>,
    pub sparse: Option<Vec<String>>,
    /// The worktree's Git admin dir; its `.git` file points here.
    #[serde(default)]
    pub gitdir: Option<String>,
    /// Set by AISC when `git worktree remove` failed partway and left files behind.
    #[serde(default)]
    pub partially_removed: bool,
}

/// Directories never treated as a worktree, whatever the registry says.
const SYSTEM_DIRS: &[&str] = &[
    "/Users", "/home", "/Volumes", "/private", "/var", "/tmp", "/System", "/opt", "/usr",
];

/// Registry names become file names: `[A-Za-z0-9._-]`, no `..`, never a path.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.contains("..")
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

pub(crate) fn registry_path(common: &Path, name: &str) -> Result<PathBuf> {
    if !valid_name(name) {
        bail!("invalid AISC worktree registry name {name:?}");
    }
    Ok(common
        .join("aisc")
        .join("worktrees")
        .join(format!("{name}.json")))
}

pub(crate) fn register(common: &Path, registration: &Registration) -> Result<()> {
    let path = registry_path(common, &registration.name)?;
    fs::create_dir_all(path.parent().context("invalid registry path")?)?;
    task::write_json(&path, registration)
}

pub(crate) fn unregister(common: &Path, name: &str) -> Result<()> {
    match fs::remove_file(registry_path(common, name)?) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}

/// A registry file and, when it cannot be trusted, why. Invalid entries are never acted on.
struct Entry {
    file: PathBuf,
    registration: Registration,
    invalid: Option<String>,
}

fn registrations(common: &Path) -> Vec<Entry> {
    let Ok(entries) = fs::read_dir(common.join("aisc").join("worktrees")) else {
        return Vec::new();
    };
    let mut found = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .map(|path| {
            let stem = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default();
            match task::read_json::<Registration>(&path) {
                Err(_) => Entry {
                    file: path,
                    registration: Registration {
                        name: stem,
                        ..Registration::default()
                    },
                    invalid: Some("unreadable registry file".into()),
                },
                Ok(registration) => {
                    let invalid = if !valid_name(&registration.name) {
                        Some("unsafe registry name".to_owned())
                    } else if registration.name != stem {
                        Some(format!("registry file {stem}.json names another task"))
                    } else {
                        None
                    };
                    Entry {
                        file: path,
                        registration,
                        invalid,
                    }
                }
            }
        })
        .collect::<Vec<_>>();
    found.sort_by(|left, right| left.registration.name.cmp(&right.registration.name));
    found
}

/// Registered paths that must never be walked or removed: relative ones, `/`, `$HOME`, and the
/// main worktree, the Git dir or any of their ancestors.
fn unsafe_path(path: &str, main: &Path, common: &Path) -> Option<&'static str> {
    use std::os::unix::fs::MetadataExt;
    let path = Path::new(path);
    let normalized = path
        .components()
        .all(|component| matches!(component, Component::RootDir | Component::Normal(_)));
    if !path.is_absolute() || !normalized || path.parent().is_none() {
        return Some("registered path is not a normalized absolute worktree location");
    }
    let resolved = resolve_parent(path);
    // Same file identity catches case variants on case-insensitive filesystems and symlinks.
    let identity = |path: &Path| {
        fs::metadata(path)
            .ok()
            .map(|metadata| (metadata.dev(), metadata.ino()))
    };
    let target = identity(path);
    let same = |other: &Path| target.is_some() && identity(other) == target;
    // A path covers `protected` when it is that path or one of its ancestors.
    let covers = |protected: &Path| {
        let canonical = fs::canonicalize(protected).unwrap_or_else(|_| protected.to_path_buf());
        protected.starts_with(path)
            || resolved
                .as_deref()
                .is_some_and(|resolved| canonical.starts_with(resolved))
            || canonical.ancestors().any(same)
    };
    if std::env::var_os("HOME").is_some_and(|home| covers(Path::new(&home))) {
        return Some("registered path is the home directory or one of its ancestors");
    }
    if covers(main) || covers(common) {
        return Some("registered path contains the main worktree or the Git dir");
    }
    let system = SYSTEM_DIRS.iter().any(|directory| {
        let directory = Path::new(directory);
        path == directory
            || resolved.as_deref() == fs::canonicalize(directory).ok().as_deref()
            || same(directory)
    });
    if system {
        return Some("registered path is a system directory");
    }
    None
}

/// Whether an unlisted registered directory still carries AISC's worktree marker: a `.git`
/// file pointing at the admin dir recorded at start (or, for older entries, any admin dir).
fn has_aisc_marker(registration: &Registration, common: &Path) -> bool {
    if registration.partially_removed {
        return true;
    }
    let marker = Path::new(&registration.path).join(".git");
    if !fs::symlink_metadata(&marker).is_ok_and(|metadata| metadata.is_file()) {
        return false;
    }
    let Ok(content) = fs::read_to_string(&marker) else {
        return false;
    };
    let Some(target) = content.trim_end().strip_prefix("gitdir: ") else {
        return false;
    };
    match &registration.gitdir {
        Some(gitdir) => target == gitdir,
        None => Path::new(target).parent() == Some(common.join("worktrees").as_path()),
    }
}

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// One record of `git worktree list --porcelain -z`.
struct Listed {
    path: String,
    head: Option<String>,
    branch: Option<String>,
    locked: bool,
    prunable: bool,
}

fn list(root: &Path) -> Result<Vec<Listed>> {
    let output = run_git(root, &["worktree", "list", "--porcelain", "-z"], None)?;
    let mut records = Vec::new();
    let mut current: Option<Listed> = None;
    for field in output.split(|byte| *byte == 0) {
        let field = text(field)?;
        if field.is_empty() {
            records.extend(current.take());
            continue;
        }
        let (key, value) = field.split_once(' ').unwrap_or((field, ""));
        if key == "worktree" {
            records.extend(current.take());
            current = Some(Listed {
                path: value.to_owned(),
                head: None,
                branch: None,
                locked: false,
                prunable: false,
            });
            continue;
        }
        let Some(record) = current.as_mut() else {
            continue;
        };
        match key {
            "HEAD" => record.head = Some(value.to_owned()),
            "branch" => {
                record.branch = Some(
                    value
                        .strip_prefix("refs/heads/")
                        .unwrap_or(value)
                        .to_owned(),
                )
            }
            "locked" => record.locked = true,
            "prunable" => record.prunable = true,
            _ => {}
        }
    }
    records.extend(current);
    Ok(records)
}

/// Linked worktree path → its admin dir (`<common>/worktrees/<id>`), from each `gitdir` file.
fn admin_dirs(common: &Path) -> HashMap<String, PathBuf> {
    let Ok(entries) = fs::read_dir(common.join("worktrees")) else {
        return HashMap::new();
    };
    entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter_map(|admin| {
            let gitdir = fs::read_to_string(admin.join("gitdir")).ok()?;
            let path = gitdir
                .trim_end_matches('\n')
                .strip_suffix("/.git")?
                .to_owned();
            Some((path, admin))
        })
        .collect()
}

/// The path with its parent resolved but its final component kept as is, so a worktree
/// directory swapped for a symlink never matches the symlink's target.
fn resolve_parent(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?;
    Some(fs::canonicalize(path.parent()?).ok()?.join(name))
}

fn same_path(left: &str, right: &Path) -> bool {
    Path::new(left) == right
        || resolve_parent(Path::new(left))
            .zip(resolve_parent(right))
            .is_some_and(|(left, right)| left == right)
}

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
}

fn session_at(admin: &Path) -> Option<Session> {
    task::read_json::<Session>(&admin.join("aisc-session.json")).ok()
}

#[derive(Clone, Copy, Default)]
struct Size {
    apparent: u64,
    allocated: u64,
    partial: bool,
}

/// Bytes under `root` without following symlinks: apparent length and allocated blocks.
fn measure(root: &Path) -> Size {
    use std::os::unix::fs::MetadataExt;
    let started = Instant::now();
    let mut size = Size::default();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        if started.elapsed() > SIZE_BUDGET {
            size.partial = true;
            break;
        }
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            size.partial |= path != root;
            continue;
        };
        size.apparent += metadata.len();
        size.allocated += metadata.blocks() * 512;
        if !metadata.is_dir() {
            continue;
        }
        match fs::read_dir(&path) {
            Ok(entries) => pending.extend(entries.filter_map(|entry| entry.ok().map(|e| e.path()))),
            Err(_) => size.partial = true,
        }
    }
    size
}

/// Tool-owned directories, regenerable wherever they appear.
const TOOL_DIRS: &[&str] = &[
    "node_modules",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".tox",
    ".venv",
    ".gradle",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".turbo",
    ".parcel-cache",
    "DerivedData",
    ".dart_tool",
];
/// Common names that are build output only at the worktree root or next to a project manifest;
/// elsewhere (`src/build/`, `docs/dist/`) they may hold the user's own files.
const GENERIC_DIRS: &[&str] = &[
    "build", "dist", "out", "target", "coverage", ".cache", "bin", "obj",
];
const MANIFESTS: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "setup.py",
    "setup.cfg",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
    "Package.swift",
    "Gemfile",
    "composer.json",
    "mix.exs",
    "CMakeLists.txt",
];
const MANIFEST_SUFFIXES: &[&str] = &[".csproj", ".sln"];
const REGENERABLE_SUFFIXES: &[&str] = &[".pyc", ".pyo", ".o"];
/// OS and package-manager junk, regenerable anywhere. Other `*.log` files are not.
const JUNK_FILES: &[&str] = &[".DS_Store", "Thumbs.db", "desktop.ini"];
const JUNK_PREFIXES: &[&str] = &[
    "npm-debug.log",
    "yarn-debug.log",
    "yarn-error.log",
    "pnpm-debug.log",
    "lerna-debug.log",
];
/// Directories that hold credentials wherever they appear (compared case-insensitively).
const SECRET_DIRS: &[&str] = &[
    ".aws", ".kube", ".ssh", ".gnupg", ".docker", "gcloud", "secrets", "secret",
];

/// A few example paths plus the total count.
#[derive(Default)]
struct Sample {
    paths: Vec<String>,
    count: usize,
}

impl Sample {
    fn push(&mut self, path: &str) {
        self.count += 1;
        if self.paths.len() < SAMPLE_SHOWN {
            self.paths.push(path.to_owned());
        }
    }

    fn describe(&self) -> String {
        let mut shown = self.paths.join(", ");
        if self.count > self.paths.len() {
            shown.push_str(&format!(" and {} more", self.count - self.paths.len()));
        }
        shown
    }
}

/// Ignored content that `git worktree remove` would delete. Removal is automatic only when all
/// of it is regenerable (an allowlist), with no secret-like names and no nested repositories.
#[derive(Default)]
struct Ignored {
    files: u64,
    bytes: u64,
    secrets: Sample,
    nested: Sample,
    unknown: Sample,
    truncated: bool,
}

/// Whether worktree directories hold a project manifest, cached per directory.
struct Manifests<'a> {
    worktree: &'a Path,
    known: HashMap<String, bool>,
}

impl Manifests<'_> {
    fn beside(&mut self, directory: &str) -> bool {
        if directory.is_empty() {
            return true;
        }
        if let Some(known) = self.known.get(directory) {
            return *known;
        }
        let path = self.worktree.join(directory);
        let found = MANIFESTS.iter().any(|name| path.join(name).is_file())
            || fs::read_dir(&path).is_ok_and(|entries| {
                entries.filter_map(|entry| entry.ok()).any(|entry| {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    MANIFEST_SUFFIXES
                        .iter()
                        .any(|suffix| name.ends_with(suffix))
                })
            });
        self.known.insert(directory.to_owned(), found);
        found
    }
}

/// Classification inherited from the directories above a path.
#[derive(Clone, Copy, Default)]
struct Lineage {
    regenerable: bool,
    node_modules: bool,
    venv: bool,
    /// Installed dependencies: their file names come from lockfiles, so names are not checked.
    third_party: bool,
    secret: bool,
}

impl Lineage {
    /// The lineage of directory `name` inside `parent` (relative to the worktree root).
    fn enter(self, parent: &str, name: &str, manifests: &mut Manifests) -> Self {
        let anchored = GENERIC_DIRS.contains(&name) && manifests.beside(parent);
        let third_party = self.third_party
            || matches!(name, "node_modules" | ".gradle" | ".dart_tool")
            || (name == "target" && anchored)
            || (name == "site-packages" && self.venv);
        Self {
            regenerable: self.regenerable || TOOL_DIRS.contains(&name) || anchored,
            node_modules: self.node_modules || name == "node_modules",
            venv: self.venv || name == ".venv",
            third_party,
            secret: self.secret
                || (!third_party && SECRET_DIRS.contains(&name.to_ascii_lowercase().as_str())),
        }
    }
}

fn scan_limit() -> usize {
    std::env::var("AISC_IGNORED_SCAN_LIMIT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(IGNORED_ENTRY_BUDGET)
}

fn parent_of(relative: &str) -> &str {
    relative.rsplit_once('/').map_or("", |(parent, _)| parent)
}

impl Ignored {
    fn scan(worktree: &Path) -> Result<Self> {
        // `--directory` reports a wholly ignored directory once instead of every file in it.
        let output = run_git(
            worktree,
            &[
                "ls-files",
                "-z",
                "--others",
                "--ignored",
                "--exclude-standard",
                "--directory",
            ],
            None,
        )?;
        let started = Instant::now();
        let limit = scan_limit();
        let mut manifests = Manifests {
            worktree,
            known: HashMap::new(),
        };
        let mut ignored = Self::default();
        let mut pending = Vec::new();
        let mut roots = output
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .map(|path| Ok(text(path)?.trim_end_matches('/').to_owned()))
            .collect::<Result<Vec<_>>>()?;
        // Git may list both `data/` and `data/.cache/`; walk each tree once.
        roots.sort();
        let mut kept: Vec<String> = Vec::new();
        roots.retain(|root| {
            let inside = kept
                .iter()
                .any(|outer| root.starts_with(&format!("{outer}/")));
            if !inside {
                kept.push(root.clone());
            }
            !inside
        });
        for relative in roots {
            let mut lineage = Lineage::default();
            let mut parent = String::new();
            let mut parts = relative.split('/').collect::<Vec<_>>();
            parts.pop();
            for part in parts {
                lineage = lineage.enter(&parent, part, &mut manifests);
                if !parent.is_empty() {
                    parent.push('/');
                }
                parent.push_str(part);
            }
            pending.push((relative, lineage));
        }
        let mut visited = 0_usize;
        while let Some((relative, lineage)) = pending.pop() {
            visited += 1;
            if visited > limit || started.elapsed() > SCAN_BUDGET {
                ignored.truncated = true;
                break;
            }
            let full = worktree.join(&relative);
            let Ok(metadata) = fs::symlink_metadata(&full) else {
                continue;
            };
            let name = relative.rsplit('/').next().unwrap_or(&relative);
            if !metadata.is_dir() {
                ignored.file(&relative, name, metadata.len(), lineage);
                continue;
            }
            let inner = lineage.enter(parent_of(&relative), name, &mut manifests);
            if !inner.node_modules && (name == ".git" || is_bare_repository(&full)) {
                ignored.nested.push(if name == ".git" {
                    parent_of(&relative)
                } else {
                    &relative
                });
            }
            let Ok(entries) = fs::read_dir(&full) else {
                ignored.truncated = true;
                continue;
            };
            for entry in entries.filter_map(|entry| entry.ok()) {
                let child = entry.file_name().to_string_lossy().into_owned();
                pending.push((format!("{relative}/{child}"), inner));
            }
        }
        Ok(ignored)
    }

    fn file(&mut self, relative: &str, name: &str, size: u64, lineage: Lineage) {
        self.files += 1;
        self.bytes += size;
        if name == ".git" && !lineage.node_modules {
            // A `.git` file: a linked worktree or a --separate-git-dir checkout.
            self.nested.push(parent_of(relative));
        }
        let junk = JUNK_FILES.contains(&name)
            || JUNK_PREFIXES.iter().any(|prefix| name.starts_with(prefix));
        if !lineage.third_party && !junk && (lineage.secret || secret_like(name)) {
            self.secrets.push(relative);
        } else if !(lineage.regenerable
            || junk
            || REGENERABLE_SUFFIXES
                .iter()
                .any(|suffix| name.ends_with(suffix)))
        {
            self.unknown.push(relative);
        }
    }

    /// Why removal is refused, unless the caller passed `--delete-ignored`.
    fn refusal(&self, delete_ignored: bool) -> Option<String> {
        let reason = if delete_ignored {
            return None;
        } else if self.truncated {
            format!(
                "ignored file scan truncated after {} entries or {} s",
                scan_limit(),
                SCAN_BUDGET.as_secs()
            )
        } else if self.nested.count > 0 {
            format!("nested git repository: {}", self.nested.describe())
        } else if self.secrets.count > 0 {
            format!("ignored secret-like files: {}", self.secrets.describe())
        } else if self.unknown.count > 0 {
            format!(
                "ignored files not known to be regenerable: {}",
                self.unknown.describe()
            )
        } else {
            return None;
        };
        Some(format!("{reason} (pass --delete-ignored to remove anyway)"))
    }
}

/// A bare repository: `HEAD` plus `objects/` and `refs/`.
fn is_bare_repository(directory: &Path) -> bool {
    directory.join("HEAD").is_file()
        && directory.join("objects").is_dir()
        && directory.join("refs").is_dir()
}

/// Credential-looking file names, compared case-insensitively like gitignore on macOS/Windows.
fn secret_like(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    const EXACT: &[&str] = &[
        ".env",
        ".envrc",
        ".npmrc",
        ".netrc",
        ".pgpass",
        ".git-credentials",
    ];
    const PREFIXES: &[&str] = &[
        ".env.",
        "id_rsa",
        "id_ed25519",
        "id_ecdsa",
        "id_dsa",
        "credentials",
        "terraform.tfstate",
        "service-account",
        "secrets.",
    ];
    const SUFFIXES: &[&str] = &[
        ".env",
        ".pem",
        ".key",
        ".p12",
        ".pfx",
        ".jks",
        ".keystore",
        ".ovpn",
        ".kdbx",
    ];
    EXACT.contains(&name.as_str())
        || PREFIXES.iter().any(|prefix| name.starts_with(prefix))
        || SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
        || name.contains("credential")
}

/// Saves `head` as `refs/aisc/rescue/<name>` unless a branch, tag, remote or AISC ref already
/// contains it; returns the ref it wrote.
fn rescue(main: &Path, name: &str, head: &str) -> Result<Option<String>> {
    if head.is_empty() {
        return Ok(None);
    }
    let containing = run_git(
        main,
        &[
            "for-each-ref",
            "--count=1",
            "--format=%(refname)",
            "--contains",
            head,
            "refs/heads",
            "refs/tags",
            "refs/remotes",
            "refs/aisc",
        ],
        None,
    )?;
    if !containing.is_empty() {
        return Ok(None);
    }
    for attempt in 0..100 {
        let reference = match attempt {
            0 => format!("refs/aisc/rescue/{name}"),
            _ => format!("refs/aisc/rescue/{name}-{attempt}"),
        };
        // An empty old value makes the update fail if the ref already exists.
        let message = "aisc: save worktree HEAD before removal";
        if run_git(
            main,
            &["update-ref", "-m", message, &reference, head, ""],
            None,
        )
        .is_ok()
        {
            return Ok(Some(reference));
        }
    }
    bail!("could not create a rescue ref for {head}; worktree kept")
}

/// Removes a verified AISC worktree (Git refuses dirty or locked ones), then its registry entry.
fn remove(main: &Path, common: &Path, registration: &Registration) -> Result<()> {
    if let Err(error) = run_git(
        main,
        &["worktree", "remove", "--", &registration.path],
        None,
    ) {
        // Git drops its admin dir (and maybe the `.git` file) even when deleting the files
        // fails partway; record that so gc still recognizes the directory as AISC's leftover.
        let path = Path::new(&registration.path);
        let listed = list(main).map_or(true, |listed| {
            listed.iter().any(|entry| same_path(&entry.path, path))
        });
        if !listed && fs::symlink_metadata(path).is_ok() {
            let mut partial = registration.clone();
            partial.partially_removed = true;
            let _ = register(common, &partial);
        }
        return Err(error);
    }
    unregister(common, &registration.name).context(
        "worktree removed but its registry entry could not be deleted; `aisc gc --apply` forgets it",
    )
}

fn current_directory_inside(root: &Path) -> bool {
    let canonical = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    std::env::current_dir()
        .ok()
        .and_then(|cwd| fs::canonicalize(cwd).ok())
        .is_some_and(|cwd| cwd.starts_with(&canonical))
}

/// After a successful `finish` in a worktree session: removes the worktree unless it is not
/// AISC's, locked, holds ignored secret-like files, or Git refuses. Never fails the finish;
/// problems become `kept_reason` or `warning` fields.
pub(crate) fn remove_after_finish(
    repo: &Repo,
    name: &str,
    delete_ignored: bool,
) -> Map<String, Value> {
    let mut fields = Map::new();
    fields.insert("removed_worktree".into(), Value::Null);
    match try_remove_after_finish(repo, name, delete_ignored, &mut fields) {
        Ok(Some(reason)) => {
            fields.insert("kept_reason".into(), json!(reason));
        }
        Ok(None) => {}
        Err(error) => {
            fields.insert(
                "warning".into(),
                json!(format!(
                    "finished, but the worktree was not removed: {error:#}; it is kept and `aisc gc` lists it"
                )),
            );
        }
    }
    fields
}

fn try_remove_after_finish(
    repo: &Repo,
    name: &str,
    delete_ignored: bool,
    fields: &mut Map<String, Value>,
) -> Result<Option<String>> {
    let common = repo.common_dir()?;
    let Ok(registration) = task::read_json::<Registration>(&registry_path(&common, name)?) else {
        return Ok(Some(
            "worktree not registered by `aisc start --path`; kept".into(),
        ));
    };
    if registration.name != name || !same_path(&registration.path, &repo.root) {
        return Ok(Some(
            "registry entry points at a different path; kept".into(),
        ));
    }
    let listed = list(&repo.root)?;
    let main = PathBuf::from(&listed.first().context("Git listed no worktrees")?.path);
    fields.insert("main_checkout".into(), json!(main));
    if let Some(reason) = unsafe_path(&registration.path, &main, &common) {
        return Ok(Some(format!("{reason}; kept")));
    }
    if listed
        .iter()
        .any(|entry| entry.locked && same_path(&entry.path, &repo.root))
    {
        return Ok(Some(
            "worktree is locked (`git worktree lock`); kept".into(),
        ));
    }
    let ignored = Ignored::scan(&repo.root)?;
    fields.insert("ignored_files".into(), json!(ignored.files));
    fields.insert("ignored_bytes".into(), json!(ignored.bytes));
    fields.insert("ignored_scan_truncated".into(), json!(ignored.truncated));
    if let Some(reason) = ignored.refusal(delete_ignored) {
        return Ok(Some(reason));
    }
    if let Some(reference) = rescue(&main, name, &repo.head()?)? {
        fields.insert("rescue_ref".into(), json!(reference));
    }
    let size = measure(&repo.root);
    let inside = current_directory_inside(&repo.root);
    if let Err(error) = remove(&main, &common, &registration) {
        let still_listed = list(&main)
            .map(|listed| {
                listed
                    .iter()
                    .any(|entry| same_path(&entry.path, &repo.root))
            })
            .unwrap_or(true);
        if still_listed {
            return Err(error);
        }
        // Git drops its admin dir even when deleting the files fails partway.
        fields.insert("partially_removed".into(), json!(registration.path));
        fields.insert(
            "warning".into(),
            json!(format!(
                "finished, but the worktree was only partially removed ({error:#}); remaining files left at {}; delete by hand (branch {} keeps the commits)",
                registration.path, registration.branch
            )),
        );
        return Ok(None);
    }
    fields.insert("removed_worktree".into(), json!(registration.path));
    fields.insert("freed_bytes".into(), json!(size.allocated));
    if inside {
        fields.insert("cd".into(), json!(main));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_names_cannot_escape_the_registry() {
        for good in ["fix-a", "task_1", "v1.2"] {
            assert!(valid_name(good), "{good}");
        }
        for bad in ["", "..", "../x", "a/b", ".hidden", "a..b", "a\\b", "x y"] {
            assert!(!valid_name(bad), "{bad}");
        }
        assert!(registry_path(Path::new("/c"), "../../victim").is_err());
    }

    #[test]
    fn secret_like_names_match_common_credential_files() {
        for secret in [
            ".env",
            "app/.env.local",
            "tls/server.pem",
            "deploy.key",
            "id_rsa",
            "id_ed25519.pub",
            "cert.p12",
            ".npmrc",
            ".netrc",
            "credentials.json",
            ".ENV",
            "conf/Prod.Env",
            ".envrc",
            "id_ecdsa",
            "id_dsa.pub",
            ".pgpass",
            "store.PFX",
            "app.jks",
            "release.keystore",
            ".git-credentials",
            "vpn.ovpn",
            "terraform.tfstate.backup",
            "vault.kdbx",
        ] {
            assert!(secret_like(secret), "{secret}");
        }
        for ordinary in ["build/app.o", "target/debug/aisc", "env.txt", "keys.md"] {
            assert!(!secret_like(ordinary), "{ordinary}");
        }
    }

    #[test]
    fn unsafe_paths_reject_parent_components_and_identity_matches() {
        let main = std::env::temp_dir();
        let common = main.join("never-exists-aisc-common");
        let home = std::env::var("HOME").unwrap();
        for path in [
            format!("{home}/.."),
            format!("{home}/x/../.."),
            "/tmp/./..".to_owned(),
            "relative/path".to_owned(),
            "/".to_owned(),
        ] {
            assert!(unsafe_path(&path, &main, &common).is_some(), "{path}");
        }
        let fine = main.join("aisc-unsafe-path-probe-missing");
        assert!(unsafe_path(fine.to_str().unwrap(), &main.join("repo"), &common).is_none());
    }
}
