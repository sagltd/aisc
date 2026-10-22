//! Git plumbing wrappers: blobs, trees, commits, merges and reference transactions.
use super::{Code, fail, fail_hint};
use crate::git::{Repo, run_git_raw, text};
use crate::output::escape_block;
use crate::process::Output;
use crate::store::{self, Store};
use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

const LIMIT: usize = 64 * 1_024 * 1_024;

pub(crate) type Entry = (String, String); // (mode, oid)

#[derive(Clone)]
pub(crate) struct Git {
    pub root: PathBuf,
    pub dir: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CommitMeta {
    /// The raw `author` header value: `Name <email> <unix> <tz>`, byte for byte.
    pub author: String,
    /// The raw `committer` header value; `None` means "use the current identity".
    pub committer: Option<String>,
    /// The raw message bytes, exactly as stored.
    pub message: Vec<u8>,
}

impl CommitMeta {
    /// Keeps the author but commits as the current identity (every verb except a pure re-parent).
    pub fn with_current_committer(mut self) -> Self {
        self.committer = None;
        self
    }
}

#[derive(Debug)]
pub(crate) struct HeadState {
    /// `refs/heads/NAME` when attached.
    pub symbolic: Option<String>,
    pub oid: String,
}

pub(crate) struct TreeEdit {
    pub path: String,
    pub entry: Option<Entry>,
}

#[derive(Debug)]
pub(crate) struct MergeConflict {
    pub path: String,
    pub stages: [Option<Entry>; 3],
}

#[derive(Debug)]
pub(crate) struct MergeResult {
    pub tree: String,
    pub conflicts: Vec<MergeConflict>,
    pub messages: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Favor {
    None,
    Ours,
    Theirs,
    Union,
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// What this process learned about a repository that later steps can reuse.
#[derive(Default)]
struct Notes {
    /// Commits this process wrote.
    created: HashSet<String>,
    /// Every step runs as a Git subprocess (see [`Git::keep_subprocesses`]).
    subprocesses: bool,
}

fn notes() -> &'static Mutex<HashMap<PathBuf, Notes>> {
    static NOTES: OnceLock<Mutex<HashMap<PathBuf, Notes>>> = OnceLock::new();
    NOTES.get_or_init(|| Mutex::new(HashMap::new()))
}

type PatchCache = Mutex<HashMap<(PathBuf, (String, String), Vec<String>), Vec<u8>>>;

fn patches() -> &'static PatchCache {
    static PATCHES: OnceLock<PatchCache> = OnceLock::new();
    PATCHES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn is_oid(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(crate) fn note_created(dir: &Path, oid: &str) {
    notes()
        .lock()
        .expect("notes")
        .entry(dir.to_owned())
        .or_default()
        .created
        .insert(oid.to_owned());
}

/// Commits this process wrote in `dir`'s repository.
pub(crate) fn created(dir: &Path) -> HashSet<String> {
    notes()
        .lock()
        .expect("notes")
        .get(dir)
        .map(|notes| notes.created.clone())
        .unwrap_or_default()
}

struct TempIndex(PathBuf);
impl Drop for TempIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

impl Git {
    pub fn from_repo(repo: &Repo) -> Self {
        Self {
            root: repo.root.clone(),
            dir: repo.directory.clone(),
        }
    }

    pub fn raw(
        &self,
        args: &[&str],
        env: &[(&str, &OsStr)],
        input: Option<Vec<u8>>,
    ) -> Result<Output> {
        run_git_raw(&self.root, args, env, input, LIMIT)
    }

    pub fn failure(args: &[&str], output: &Output) -> anyhow::Error {
        let stderr = escape_block(&String::from_utf8_lossy(
            &output.stderr[..output.stderr.len().min(2_048)],
        ));
        fail(
            Code::Git,
            format!(
                "git {} failed (exit {:?}): {}",
                args.iter()
                    .find(|argument| !argument.starts_with('-') && !argument.contains('='))
                    .copied()
                    .unwrap_or(""),
                output.code,
                stderr.trim()
            ),
        )
    }

    pub fn run_with(
        &self,
        args: &[&str],
        env: &[(&str, &OsStr)],
        input: Option<Vec<u8>>,
    ) -> Result<Vec<u8>> {
        let output = self.raw(args, env, input)?;
        if !output.success() {
            return Err(Self::failure(args, &output));
        }
        if output.stdout_omitted != 0 {
            bail!("Git output exceeds the safety limit; narrow the operation");
        }
        Ok(output.stdout)
    }

    pub fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        self.run_with(args, &[], None)
    }

    /// Output with one trailing newline removed.
    pub fn line(&self, args: &[&str]) -> Result<String> {
        let output = self.run(args)?;
        let value = text(&output)?;
        Ok(value.strip_suffix('\n').unwrap_or(value).to_owned())
    }

    /// `Some(stdout)` on exit 0, `None` on exit 1 (for probes such as `rev-parse --quiet`).
    pub fn probe(&self, args: &[&str]) -> Result<Option<String>> {
        let output = self.raw(args, &[], None)?;
        match output.code {
            Some(0) => Ok(Some(text(&output.stdout)?.trim().to_owned())),
            Some(1) | Some(128) => Ok(None),
            _ => Err(Self::failure(args, &output)),
        }
    }

    /// Runs `body` on the in-process store; `None` means "ask Git" (see `crate::store`).
    pub fn fast<T>(&self, body: impl FnOnce(&Store) -> Result<T>) -> Option<T> {
        let subprocesses = notes()
            .lock()
            .expect("notes")
            .get(&self.dir)
            .is_some_and(|notes| notes.subprocesses);
        if subprocesses {
            return None;
        }
        store::with(&self.root, &self.dir, body)
    }

    /// Runs every later step of this process as a Git subprocess, as before the in-process
    /// store existed. `discard` uses it: it is not on a hot path, and its stale checks are
    /// verified by tests that change files between its individual Git steps.
    pub fn keep_subprocesses(&self) {
        notes()
            .lock()
            .expect("notes")
            .entry(self.dir.clone())
            .or_default()
            .subprocesses = true;
    }

    /// Whether loose objects may be written in process: Git applies `core.sharedRepository`
    /// permissions that the in-process writer does not.
    fn writes_in_process(&self) -> bool {
        self.fast(|_| Ok(())).is_some()
            && crate::runner::config(&self.root)
                .is_ok_and(|config| config.get("core.sharedrepository").is_none())
    }

    /// Stores an object exactly as `git hash-object -t KIND -w --stdin --no-filters` would.
    pub fn write_object(&self, kind: store::Kind, bytes: &[u8]) -> Result<String> {
        if self.writes_in_process()
            && let Some(oid) = self.fast(|store| store.write(kind, bytes))
        {
            return Ok(oid);
        }
        let kind = kind.to_string();
        let output = self.run_with(
            &["hash-object", "-t", &kind, "-w", "--stdin", "--no-filters"],
            &[],
            Some(bytes.to_vec()),
        )?;
        Ok(text(&output)?.trim().to_owned())
    }

    pub fn head_state(&self) -> Result<HeadState> {
        let no_commits = || {
            fail_hint(
                Code::State,
                "the repository has no commits yet",
                "create an initial commit with git first",
            )
        };
        if let Some((symbolic, oid)) = self.fast(|store| store.head()) {
            return Ok(HeadState {
                symbolic,
                oid: oid.ok_or_else(no_commits)?,
            });
        }
        let oid = self
            .probe(&["rev-parse", "--verify", "--quiet", "HEAD^{commit}"])?
            .ok_or_else(no_commits)?;
        let symbolic = self.probe(&["symbolic-ref", "-q", "HEAD"])?;
        Ok(HeadState { symbolic, oid })
    }

    /// The index entries at `paths` (a path or a directory prefix, as `ls-files -s -- PATHS`
    /// matches literal pathspecs) by stage, and which are `git add -N` placeholders; read in
    /// process. `None` means "ask Git".
    #[allow(clippy::type_complexity)]
    pub fn index_state(
        &self,
        paths: &[&str],
    ) -> Option<(
        BTreeMap<String, Vec<(u8, Entry)>>,
        std::collections::BTreeSet<String>,
    )> {
        self.fast(|_| store::read_index(&self.dir)).map(|entries| {
            let mut current: BTreeMap<String, Vec<(u8, Entry)>> = BTreeMap::new();
            let mut intent_to_add = std::collections::BTreeSet::new();
            for entry in entries {
                let selected = paths.iter().any(|path| {
                    entry.path == *path
                        || entry
                            .path
                            .strip_prefix(*path)
                            .is_some_and(|rest| rest.starts_with('/'))
                });
                if !selected {
                    continue;
                }
                if entry.intent_to_add {
                    intent_to_add.insert(entry.path.clone());
                }
                current
                    .entry(entry.path)
                    .or_default()
                    .push((entry.stage, (entry.mode, entry.oid)));
            }
            (current, intent_to_add)
        })
    }

    /// `diff-tree -p FROM TO [-- PATHS]` with the history flags (`diff::commit_diff_args`), for
    /// every pair at once in one `git diff-tree --stdin` process (the pathspec applies to each
    /// pair). Trees never change, so the patches are kept for the rest of the process and
    /// [`Git::tree_patch`] answers from them. Best effort: on any surprise nothing is cached.
    pub fn prefetch_patches(&self, pairs: &[(String, String)], paths: &[String]) {
        let _ = self.try_prefetch(pairs, paths);
    }

    fn try_prefetch(&self, pairs: &[(String, String)], paths: &[String]) -> Result<()> {
        let key = |pair: &(String, String)| (self.dir.clone(), pair.clone(), paths.to_vec());
        let missing: Vec<&(String, String)> = {
            let cache = patches().lock().expect("patch cache");
            let mut seen = HashSet::new();
            pairs
                .iter()
                .filter(|pair| !cache.contains_key(&key(pair)) && seen.insert(*pair))
                .collect()
        };
        if missing.len() < 2
            || missing
                .iter()
                .any(|(from, to)| !is_oid(from) || !is_oid(to))
        {
            return Ok(());
        }
        let mut input = String::new();
        for (from, to) in &missing {
            input.push_str(&format!("{from} {to}\n"));
        }
        let mut args = super::diff::commit_diff_args(false);
        args.insert(
            args.iter().position(|arg| *arg == "-r").unwrap_or(4),
            "--stdin",
        );
        if !paths.is_empty() {
            args.push("--");
            args.extend(paths.iter().map(String::as_str));
        }
        let output = self.run_with(&args, &[], Some(input.into_bytes()))?;
        // Each pair's patch follows the echoed "FROM TO" line. Patch lines never start with a hex
        // digit (they start with "diff", "index", "---", "@@", " ", "+", "-", ...), so every
        // line of two ids is a header.
        let header_line = |line: &[u8]| {
            line.len() == 82 && line[40] == b' ' && line[..40].iter().all(u8::is_ascii_hexdigit)
        };
        let mut found: Vec<Vec<u8>> = Vec::with_capacity(missing.len());
        let mut lines = output.split_inclusive(|byte| *byte == b'\n').peekable();
        for (from, to) in &missing {
            let header = format!("{from} {to}\n");
            if lines.next() != Some(header.as_bytes()) {
                bail!("unexpected diff-tree --stdin output");
            }
            let mut patch = Vec::new();
            while let Some(line) = lines.peek() {
                if header_line(line) {
                    break;
                }
                patch.extend_from_slice(line);
                lines.next();
            }
            found.push(patch);
        }
        if lines.next().is_some() {
            bail!("unexpected diff-tree --stdin output");
        }
        let mut cache = patches().lock().expect("patch cache");
        for (pair, patch) in missing.into_iter().zip(found) {
            cache.insert(key(pair), patch);
        }
        Ok(())
    }

    /// `git diff-tree -p FROM TO [-- PATHS]` with the history flags, prefetched or run now.
    pub fn tree_patch(&self, from: &str, to: &str, paths: &[String]) -> Result<Vec<u8>> {
        let key = (
            self.dir.clone(),
            (from.to_owned(), to.to_owned()),
            paths.to_vec(),
        );
        if let Some(patch) = patches().lock().expect("patch cache").get(&key) {
            return Ok(patch.clone());
        }
        let mut args = super::diff::commit_diff_args(false);
        args.push(from);
        args.push(to);
        if !paths.is_empty() {
            args.push("--");
            args.extend(paths.iter().map(String::as_str));
        }
        let patch = self.run(&args)?;
        patches()
            .lock()
            .expect("patch cache")
            .insert(key, patch.clone());
        Ok(patch)
    }

    /// `git log --no-walk=unsorted --abbrev=4 --format=%h OIDS`: the shortest unique prefix of
    /// each commit (at least four digits), repeats listed once.
    pub fn short_ids(&self, oids: &[String]) -> Result<Vec<String>> {
        if oids.is_empty() {
            return Ok(Vec::new());
        }
        let mut seen = HashSet::new();
        let unique: Vec<&String> = oids.iter().filter(|oid| seen.insert(*oid)).collect();
        if let Some(ids) = self.fast(|store| {
            unique
                .iter()
                .map(|oid| store.abbrev(oid, 4))
                .collect::<Result<Vec<_>>>()
        }) {
            return Ok(ids);
        }
        let mut arguments = vec![
            "log".to_owned(),
            "--no-walk=unsorted".to_owned(),
            "--abbrev=4".to_owned(),
            "--format=%h".to_owned(),
        ];
        arguments.extend(oids.iter().cloned());
        let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
        Ok(self.line(&arguments)?.lines().map(str::to_owned).collect())
    }

    /// `diff-tree -r --no-renames FROM TO` in process: every changed non-tree path in Git's
    /// order with its entry on each side (`None`: absent). `None` means "ask Git".
    #[allow(clippy::type_complexity)]
    pub fn tree_changes(
        &self,
        from: &str,
        to: &str,
    ) -> Option<Vec<(String, Option<Entry>, Option<Entry>)>> {
        self.fast(|store| {
            let mut changed = Vec::new();
            store.diff_trees(Some(from), Some(to), "", &mut changed)?;
            Ok(changed)
        })
    }

    /// `git remote`: configured remotes in configuration order. Legacy `remotes/` and `branches/`
    /// files are left to Git.
    pub fn remotes(&self) -> Result<Vec<String>> {
        let legacy = self.fast(|store| {
            Ok(["remotes", "branches"].iter().any(|name| {
                std::fs::read_dir(store.common_dir.join(name))
                    .is_ok_and(|mut entries| entries.next().is_some())
            }))
        });
        if legacy == Some(false) {
            let config = crate::runner::config(&self.root)?;
            let mut names: Vec<String> = Vec::new();
            for key in config.names() {
                if let Some(rest) = key.strip_prefix("remote.")
                    && let Some((name, _)) = rest.rsplit_once('.')
                    && !names.iter().any(|known| known == name)
                {
                    names.push(name.to_owned());
                }
            }
            names.sort();
            return Ok(names);
        }
        Ok(self.line(&["remote"])?.lines().map(str::to_owned).collect())
    }

    /// The result `git merge-tree --write-tree --merge-base=BASE OURS THEIRS` reaches when the
    /// merge is trivial, computed in process: one side unchanged, or both sides changing disjoint
    /// paths, no side both deleting and adding paths, and no path of one side above a path of the
    /// other. A rename (and so a directory rename) pairs a deletion with an addition on the same
    /// side, and without a shared path there is no content merge, so Git's merge takes each
    /// changed entry from its side. `None` sends everything else to Git.
    fn trivial_merge(&self, base: &str, ours: &str, theirs: &str) -> Option<String> {
        if !self.writes_in_process() {
            return None;
        }
        self.fast(|store| {
            if theirs == base || ours == theirs {
                return Ok(Some(ours.to_owned()));
            }
            if ours == base {
                return Ok(Some(theirs.to_owned()));
            }
            let (mut left, mut right) = (Vec::new(), Vec::new());
            store.diff_trees(Some(base), Some(ours), "", &mut left)?;
            store.diff_trees(Some(base), Some(theirs), "", &mut right)?;
            // A rename needs a deletion and an addition on the same side.
            let renames_possible = |side: &[(String, Option<Entry>, Option<Entry>)]| {
                side.iter().any(|(_, _, new)| new.is_none())
                    && side.iter().any(|(_, old, _)| old.is_none())
            };
            if renames_possible(&left) || renames_possible(&right) {
                return Ok(None);
            }
            let ours_paths: std::collections::BTreeSet<&str> =
                left.iter().map(|(path, _, _)| path.as_str()).collect();
            let above = |path: &str, set: &std::collections::BTreeSet<&str>| {
                let mut prefix = String::new();
                for component in path.split('/') {
                    if !prefix.is_empty() {
                        if set.contains(prefix.as_str()) {
                            return true;
                        }
                        prefix.push('/');
                    }
                    prefix.push_str(component);
                }
                false
            };
            let theirs_paths: std::collections::BTreeSet<&str> =
                right.iter().map(|(path, _, _)| path.as_str()).collect();
            for (path, _, _) in &right {
                if ours_paths.contains(path.as_str())
                    || above(path, &ours_paths)
                    || ours_paths.iter().any(|ours| above(ours, &theirs_paths))
                {
                    return Ok(None);
                }
            }
            let edits: Vec<TreeEdit> = right
                .into_iter()
                .map(|(path, _, new)| TreeEdit { path, entry: new })
                .collect();
            if !edits.iter().all(|edit| safe_path(&edit.path)) {
                return Ok(None);
            }
            edit_tree(store, ours, &edits).map(Some)
        })
        .flatten()
    }

    /// `GIT_INDEX_FILE=INDEX git ls-files -s -- PATHS` (path to blob id), read in process.
    pub fn index_file_entries(
        &self,
        index: &Path,
        paths: &[String],
    ) -> Option<Vec<(String, String)>> {
        self.fast(|_| store::read_index_file(index)).map(|entries| {
            entries
                .into_iter()
                .filter(|entry| {
                    paths.iter().any(|path| {
                        entry.path == *path
                            || entry
                                .path
                                .strip_prefix(path.as_str())
                                .is_some_and(|rest| rest.starts_with('/'))
                    })
                })
                .map(|entry| (entry.path, entry.oid))
                .collect()
        })
    }

    /// Whether Git stores the worktree bytes of `path` unchanged: no `core.autocrlf` and no
    /// attributes file with any rule (repository, `info/attributes`, global, system), so no clean
    /// filter, end-of-line or encoding conversion can apply. Only then may raw bytes stand in for
    /// Git's filtered hash.
    pub fn no_conversion(&self, path: &str) -> bool {
        if self.fast(|_| Ok(())).is_none() {
            return false;
        }
        let Ok(config) = crate::runner::config(&self.root) else {
            return false;
        };
        let autocrlf_off = match config.get("core.autocrlf") {
            None => true,
            Some(None) => false,
            Some(Some(value)) => crate::runner::parse_bool(value) == Some(false),
        };
        if !autocrlf_off || config.get("core.attributesfile").is_some() {
            return false;
        }
        let rules = |file: &Path| -> bool {
            match std::fs::read(file) {
                Ok(bytes) => String::from_utf8_lossy(&bytes).lines().any(|line| {
                    let line = line.trim();
                    !line.is_empty() && !line.starts_with('#')
                }),
                Err(error) => error.kind() != std::io::ErrorKind::NotFound,
            }
        };
        let mut files = vec![self.root.join(".gitattributes")];
        let mut directory = PathBuf::new();
        let components: Vec<&str> = path.split('/').collect();
        for component in &components[..components.len().saturating_sub(1)] {
            directory.push(component);
            files.push(self.root.join(&directory).join(".gitattributes"));
        }
        let Ok(common) = self.common_dir() else {
            return false;
        };
        files.push(common.join("info").join("attributes"));
        match std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
            Some(xdg) => files.push(PathBuf::from(xdg).join("git").join("attributes")),
            None => {
                if let Some(home) = std::env::var_os("HOME") {
                    files.push(PathBuf::from(home).join(".config/git/attributes"));
                }
            }
        }
        if std::env::var_os("GIT_ATTR_NOSYSTEM").is_none() {
            match crate::runner::system_attributes_path(&self.root) {
                Some(system) => files.push(system),
                None => return false,
            }
        }
        !files.iter().any(|file| rules(file))
    }

    /// `symbolic-ref -q HEAD`: the branch HEAD names, even an unborn one (`None`: detached).
    pub fn head_symbolic(&self) -> Result<Option<String>> {
        if let Some((symbolic, _)) = self.fast(|store| store.head()) {
            return Ok(symbolic);
        }
        self.probe(&["symbolic-ref", "-q", "HEAD"])
    }

    pub fn common_dir(&self) -> Result<PathBuf> {
        if let Some(common) = self.fast(|store| Ok(store.common_dir.clone())) {
            return Ok(common);
        }
        Ok(PathBuf::from(self.line(&[
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
        ])?))
    }

    pub fn empty_tree(&self) -> Result<String> {
        self.write_object(store::Kind::Tree, b"")
    }

    pub fn zero_oid(&self, like: &str) -> String {
        "0".repeat(like.len())
    }

    /// `rev-parse --verify --quiet SPEC` without peeling, in process for full object ids and
    /// reference names; `None` means "ask Git".
    fn fast_rev(&self, spec: &str) -> Option<Option<String>> {
        self.fast(|store| {
            if spec.len() == 40 && spec.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                // A full id verifies without an existence check, as in Git.
                return Ok(Some(Some(spec.to_ascii_lowercase())));
            }
            store.dwim(spec)
        })
        .flatten()
    }

    pub fn resolve_commit(&self, spec: &str) -> Result<Option<String>> {
        if spec.is_empty() || spec.starts_with('-') {
            return Ok(None);
        }
        if let Some(found) = self.fast_rev(spec) {
            let peeled = match found {
                Some(oid) => self.fast(|store| store.peel_to_commit(&oid)),
                None => Some(None),
            };
            if let Some(peeled) = peeled {
                return Ok(peeled);
            }
        }
        let argument = format!("{spec}^{{commit}}");
        self.probe(&["rev-parse", "--verify", "--quiet", &argument])
    }

    pub fn ref_oid(&self, name: &str) -> Result<Option<String>> {
        if let Some(found) = self.fast_rev(name) {
            return Ok(found);
        }
        self.probe(&["rev-parse", "--verify", "--quiet", name])
    }

    pub fn commit_tree_of(&self, commit: &str) -> Result<String> {
        if let Some(Some(tree)) = self.fast(|store| store.peel_to_tree(commit)) {
            return Ok(tree);
        }
        let argument = format!("{commit}^{{tree}}");
        self.line(&["rev-parse", "--verify", &argument])
    }

    /// Branches (`refs/heads/..`) checked out in any worktree of this repository.
    pub fn checked_out(&self) -> Result<Vec<String>> {
        if let Some(branches) = self.fast(|store| store.checked_out()) {
            return Ok(branches);
        }
        let output = self.run(&["worktree", "list", "--porcelain"])?;
        Ok(text(&output)?
            .lines()
            .filter_map(|line| line.strip_prefix("branch "))
            .map(str::to_owned)
            .collect())
    }

    /// `git check-ref-format --branch NAME`, in process (the rules are Git's, see the store): a
    /// valid `refs/heads/NAME` that is not `HEAD` and does not start with `-`.
    pub fn valid_branch_name(&self, name: &str) -> Result<()> {
        if name.is_empty() || name.starts_with('-') || name == "HEAD" {
            return Err(fail(Code::Usage, format!("invalid branch name {name:?}")));
        }
        if store::check_ref_format(&format!("refs/heads/{name}"), false) {
            Ok(())
        } else {
            Err(fail(Code::Usage, format!("invalid branch name {name:?}")))
        }
    }

    /// The blob at `path` in commit (or tree) `rev`: `git cat-file blob REV:PATH`.
    pub fn blob_at(&self, rev: &str, path: &str) -> Result<Vec<u8>> {
        if let Some(Some(bytes)) = self.fast(|store| {
            let Some(tree) = store.peel_to_tree(rev)? else {
                return Ok(None);
            };
            match store.entry_at(&tree, path)? {
                Some((mode, oid)) if mode != "040000" && mode != "160000" => {
                    Ok(Some(store.object_of(&oid, store::Kind::Blob)?))
                }
                _ => Ok(None),
            }
        }) {
            return Ok(bytes);
        }
        self.run(&["cat-file", "blob", &format!("{rev}:{path}")])
    }

    pub fn read_blobs(&self, oids: &[String]) -> Result<Vec<Vec<u8>>> {
        if oids.is_empty() {
            return Ok(Vec::new());
        }
        if let Some(blobs) = self.fast(|store| {
            oids.iter()
                .map(|oid| {
                    store
                        .object_of(oid, store::Kind::Blob)
                        .with_context(|| format!("object {oid} is not a readable blob"))
                })
                .collect::<Result<Vec<_>>>()
        }) {
            return Ok(blobs);
        }
        let mut input = oids.join("\n").into_bytes();
        input.push(b'\n');
        let output = self.run_with(&["cat-file", "--batch"], &[], Some(input))?;
        let mut blobs = Vec::with_capacity(oids.len());
        let mut position = 0;
        for oid in oids {
            let newline = output[position..]
                .iter()
                .position(|byte| *byte == b'\n')
                .context("truncated cat-file response")?
                + position;
            let header = text(&output[position..newline])?;
            let mut fields = header.split(' ');
            let (_, kind, size) = (fields.next(), fields.next(), fields.next());
            let size: usize = match (kind, size) {
                (Some("blob"), Some(size)) => size.parse()?,
                _ => bail!("object {oid} is not a readable blob"),
            };
            let start = newline + 1;
            blobs.push(
                output
                    .get(start..start + size)
                    .context("truncated cat-file object")?
                    .to_vec(),
            );
            position = start + size + 1;
        }
        Ok(blobs)
    }

    /// Stores several blobs (no filters) with one `git hash-object --stdin-paths` process. The
    /// operation log's own records go through Git this way, so every mutation writes through Git
    /// at least once before its reference transaction.
    pub fn store_blobs(&self, blobs: &[&[u8]]) -> Result<Vec<String>> {
        if blobs.is_empty() {
            return Ok(Vec::new());
        }
        let directory = self.dir.join("aisc").join("tmp");
        let listed = directory.to_string_lossy();
        if listed.contains(['\n', '\r']) || listed.starts_with('"') {
            // --stdin-paths would split or unquote such names: one process per blob instead.
            return blobs
                .iter()
                .map(|bytes| self.write_blob(bytes, true))
                .collect();
        }
        std::fs::create_dir_all(&directory)?;
        let mut files = Vec::with_capacity(blobs.len());
        for bytes in blobs {
            let sequence = COUNTER.fetch_add(1, Ordering::SeqCst);
            // Named like the scratch indexes, so recovery sweeps them after a crash.
            let file =
                TempIndex(directory.join(format!("index-{}-{sequence}-blob", std::process::id())));
            std::fs::write(&file.0, bytes)?;
            files.push(file);
        }
        let mut input = String::new();
        for file in &files {
            input.push_str(&file.0.to_string_lossy());
            input.push('\n');
        }
        let output = self.run_with(
            &["hash-object", "-w", "--no-filters", "--stdin-paths"],
            &[],
            Some(input.into_bytes()),
        )?;
        let ids: Vec<String> = text(&output)?.lines().map(str::to_owned).collect();
        if ids.len() != blobs.len() {
            bail!("hash-object returned an unexpected number of ids");
        }
        Ok(ids)
    }

    /// The blob id of `bytes` without storing anything (`hash-object --stdin --no-filters`).
    /// In process only where the fast path reads this repository (SHA-1 storage); otherwise
    /// Git hashes it with the repository's own object format.
    pub fn blob_id(&self, bytes: &[u8]) -> Result<String> {
        if let Some(oid) = self.fast(|_| store::hash_object(store::Kind::Blob, bytes)) {
            return Ok(oid);
        }
        let output = self.run_with(
            &["hash-object", "--stdin", "--no-filters"],
            &[],
            Some(bytes.to_vec()),
        )?;
        Ok(text(&output)?.trim().to_owned())
    }

    /// Stores blob content taken from the worktree (whole files or spliced hunks) through
    /// `git hash-object`, as `git add` would.
    pub fn write_blob(&self, bytes: &[u8], no_filters: bool) -> Result<String> {
        let mut args = vec!["hash-object", "-w", "--stdin"];
        if no_filters {
            args.push("--no-filters");
        }
        let output = self.run_with(&args, &[], Some(bytes.to_vec()))?;
        Ok(text(&output)?.trim().to_owned())
    }

    /// Hashes worktree files with Git's clean filters (like `git add`) and stores the blobs.
    /// Symlinks are stored as their target text.
    pub fn hash_worktree(&self, paths: &[String]) -> Result<Vec<String>> {
        let mut oids = vec![String::new(); paths.len()];
        let mut regular = Vec::new();
        for (index, path) in paths.iter().enumerate() {
            let full = self.root.join(path);
            let metadata = std::fs::symlink_metadata(&full)
                .with_context(|| format!("{path}: cannot read file"))?;
            if metadata.file_type().is_symlink() {
                let target = std::fs::read_link(&full)?;
                oids[index] = self.write_blob(target.as_os_str().as_encoded_bytes(), true)?;
            } else if path.contains(['\n', '\r']) || path.starts_with('"') {
                // --stdin-paths C-unquotes lines starting with `"` and splits on newlines, so
                // such names are passed as an argument, which Git never interprets.
                oids[index] = self.line(&["hash-object", "-w", "--", path])?;
            } else {
                regular.push(index);
            }
        }
        if !regular.is_empty() {
            let input = regular
                .iter()
                .map(|index| paths[*index].as_str())
                .collect::<Vec<_>>()
                .join("\n")
                + "\n";
            let output = self.run_with(
                &["hash-object", "-w", "--stdin-paths"],
                &[],
                Some(input.into_bytes()),
            )?;
            let hashes = text(&output)?.lines().collect::<Vec<_>>();
            if hashes.len() != regular.len() {
                bail!("hash-object returned an unexpected number of ids");
            }
            for (index, hash) in regular.iter().zip(hashes) {
                oids[*index] = hash.to_owned();
            }
        }
        Ok(oids)
    }

    /// `ls-tree -r --full-tree TREE -- PATHS` (literal pathspecs: a path or a directory prefix).
    pub fn ls_tree(&self, tree: &str, paths: &[String]) -> Result<BTreeMap<String, Entry>> {
        if let Some(found) = self.fast(|store| {
            let mut found = BTreeMap::new();
            for path in paths {
                let path = path.trim_end_matches('/');
                if path.is_empty() {
                    store.flatten(tree, "", &mut found)?;
                    continue;
                }
                match store.entry_at(tree, path)? {
                    Some((mode, oid)) if mode == "040000" => {
                        store.flatten(&oid, path, &mut found)?
                    }
                    Some(entry) => {
                        found.insert(path.to_owned(), entry);
                    }
                    None => {}
                }
            }
            Ok(found)
        }) {
            return Ok(found);
        }
        let mut entries = BTreeMap::new();
        for chunk in paths.chunks(400) {
            let mut args = vec!["ls-tree", "-r", "-z", "--full-tree", tree, "--"];
            args.extend(chunk.iter().map(String::as_str));
            let output = self.run(&args)?;
            for record in output.split(|byte| *byte == 0).filter(|r| !r.is_empty()) {
                let (meta, path) = split_tab(record)?;
                let mut fields = meta.split(' ');
                let mode = fields.next().context("ls-tree mode")?;
                let _kind = fields.next();
                let oid = fields.next().context("ls-tree oid")?;
                entries.insert(path, (mode.to_owned(), oid.to_owned()));
            }
        }
        Ok(entries)
    }

    /// Builds `base_tree` plus `edits` with the semantics of `read-tree BASE`, `update-index
    /// --index-info EDITS`, `write-tree` (an entry replaces whatever file or directory was in its
    /// way; a removal only drops a file). In process when possible; the real index is never
    /// touched either way.
    pub fn tree_with(&self, base_tree: &str, edits: &[TreeEdit]) -> Result<String> {
        // The scratch directory exists after a tree build either way.
        std::fs::create_dir_all(self.dir.join("aisc").join("tmp"))?;
        if edits.iter().all(|edit| safe_path(&edit.path))
            && self.writes_in_process()
            && let Some(tree) = self.fast(|store| edit_tree(store, base_tree, edits))
        {
            return Ok(tree);
        }
        self.tree_with_git(base_tree, edits)
    }

    /// `git write-tree` of an index holding exactly `entries` (`(path, mode, oid)`), which aisc
    /// writes itself: one Git process per tree.
    pub fn tree_of_entries(&self, entries: &[(String, String, String)]) -> Result<String> {
        let directory = self.dir.join("aisc").join("tmp");
        std::fs::create_dir_all(&directory)?;
        let sequence = COUNTER.fetch_add(1, Ordering::SeqCst);
        let index = TempIndex(directory.join(format!("index-{}-{sequence}", std::process::id())));
        // The index aisc writes uses SHA-1 ids and checksum: only where the fast path applies.
        if self.fast(|_| Ok(())).is_some()
            && entries.iter().all(|(path, _, _)| safe_path(path))
            && let Ok(bytes) = store::write_index(entries)
        {
            std::fs::write(&index.0, bytes)?;
            let env = [("GIT_INDEX_FILE", index.0.as_os_str())];
            let output = self.run_with(&["write-tree"], &env, None)?;
            return Ok(text(&output)?.trim().to_owned());
        }
        let edits: Vec<TreeEdit> = entries
            .iter()
            .map(|(path, mode, oid)| TreeEdit {
                path: path.clone(),
                entry: Some((mode.clone(), oid.clone())),
            })
            .collect();
        let empty = self.empty_tree()?;
        self.tree_with_git(&empty, &edits)
    }

    fn tree_with_git(&self, base_tree: &str, edits: &[TreeEdit]) -> Result<String> {
        let directory = self.dir.join("aisc").join("tmp");
        std::fs::create_dir_all(&directory)?;
        let sequence = COUNTER.fetch_add(1, Ordering::SeqCst);
        let index = TempIndex(directory.join(format!("index-{}-{sequence}", std::process::id())));
        let env = [("GIT_INDEX_FILE", index.0.as_os_str())];
        self.run_with(&["read-tree", base_tree], &env, None)?;
        if !edits.is_empty() {
            let zero = self.zero_oid(base_tree);
            let mut input = Vec::new();
            for edit in edits {
                match &edit.entry {
                    Some((mode, oid)) => {
                        input.extend_from_slice(format!("{mode} {oid}\t{}", edit.path).as_bytes());
                    }
                    None => {
                        input.extend_from_slice(format!("0 {zero}\t{}", edit.path).as_bytes());
                    }
                }
                input.push(0);
            }
            self.run_with(&["update-index", "-z", "--index-info"], &env, Some(input))?;
        }
        let output = self.run_with(&["write-tree"], &env, None)?;
        Ok(text(&output)?.trim().to_owned())
    }

    pub fn read_commit_meta(&self, commit: &str) -> Result<CommitMeta> {
        let raw = match self.fast(|store| store.object_of(commit, store::Kind::Commit)) {
            Some(raw) => raw,
            None => self.run(&["cat-file", "commit", commit])?,
        };
        let split = raw
            .windows(2)
            .position(|pair| pair == b"\n\n")
            .context("commit without message separator")?;
        let header = text(&raw[..split])?;
        let mut author = None;
        let mut committer = None;
        for line in header.split('\n') {
            if line.starts_with("encoding ") {
                return Err(fail(
                    Code::Unsupported,
                    format!("commit {commit} declares a non-default encoding"),
                ));
            }
            if let Some(value) = line.strip_prefix("author ") {
                author = Some(value.to_owned());
            }
            if let Some(value) = line.strip_prefix("committer ") {
                committer = Some(value.to_owned());
            }
        }
        Ok(CommitMeta {
            author: author.context("commit without author")?,
            committer,
            message: raw[split + 2..].to_vec(),
        })
    }

    /// Writes a commit object with exactly the given message bytes. The author line is copied
    /// verbatim from `meta`; the committer is the current identity unless `meta.committer` keeps
    /// the original (a pure re-parent). With `commit.gpgsign=true` Git's own `commit-tree -S` is
    /// used instead, which cannot preserve odd identity whitespace.
    pub fn commit_tree(
        &self,
        tree: &str,
        parents: &[String],
        message: &[u8],
        meta: Option<&CommitMeta>,
    ) -> Result<String> {
        let signing = crate::runner::config(&self.root)?.is_true("commit.gpgsign");
        if signing {
            return self.signed_commit_tree(tree, parents, message, meta);
        }
        use crate::runner::{Who, ident};
        let author = match meta {
            Some(meta) => meta.author.clone(),
            None => ident(&self.root, Who::Author)?,
        };
        let committer = match meta.and_then(|meta| meta.committer.clone()) {
            Some(committer) => committer,
            None => ident(&self.root, Who::Committer)?,
        };
        let mut object = format!("tree {tree}\n").into_bytes();
        for parent in parents {
            object.extend_from_slice(format!("parent {parent}\n").as_bytes());
        }
        object.extend_from_slice(format!("author {author}\ncommitter {committer}\n\n").as_bytes());
        object.extend_from_slice(message);
        let oid = self.write_object(store::Kind::Commit, &object)?;
        note_created(&self.dir, &oid);
        Ok(oid)
    }

    fn signed_commit_tree(
        &self,
        tree: &str,
        parents: &[String],
        message: &[u8],
        meta: Option<&CommitMeta>,
    ) -> Result<String> {
        let mut args = vec!["commit-tree", tree];
        for parent in parents {
            args.push("-p");
            args.push(parent);
        }
        args.extend(["-S", "-F", "-"]);
        let split = |line: &str| -> Option<(String, String, String)> {
            let open = line.rfind('<')?;
            let close = line.rfind('>')?;
            Some((
                line[..open].trim_end().to_owned(),
                line[open + 1..close].to_owned(),
                format!("@{}", line[close + 1..].trim()),
            ))
        };
        let parts = meta.and_then(|meta| split(&meta.author));
        let mut env: Vec<(&str, &OsStr)> = Vec::new();
        if let Some((name, email, date)) = &parts {
            env.push(("GIT_AUTHOR_NAME", OsStr::new(name)));
            env.push(("GIT_AUTHOR_EMAIL", OsStr::new(email)));
            env.push(("GIT_AUTHOR_DATE", OsStr::new(date)));
        }
        let output = self.run_with(&args, &env, Some(message.to_vec()))?;
        Ok(text(&output)?.trim().to_owned())
    }

    pub fn merge_trees(&self, base: &str, ours: &str, theirs: &str) -> Result<MergeResult> {
        if let Some(tree) = self.trivial_merge(base, ours, theirs) {
            return Ok(MergeResult {
                tree,
                conflicts: Vec::new(),
                messages: String::new(),
            });
        }
        require_git(2, 43)?;
        let base_argument = format!("--merge-base={base}");
        let args = [
            "merge-tree",
            "--write-tree",
            "-z",
            &base_argument,
            ours,
            theirs,
        ];
        let output = self.raw(&args, &[], None)?;
        if !matches!(output.code, Some(0 | 1)) {
            return Err(Self::failure(&args, &output));
        }
        let mut fields = output.stdout.split(|byte| *byte == 0);
        let tree = text(fields.next().context("merge-tree without tree")?)?.to_owned();
        let mut conflicts: Vec<MergeConflict> = Vec::new();
        for record in fields.by_ref() {
            if record.is_empty() {
                break;
            }
            let (meta, path) = split_tab(record)?;
            let mut parts = meta.split(' ');
            let mode = parts.next().context("merge-tree mode")?.to_owned();
            let oid = parts.next().context("merge-tree oid")?.to_owned();
            let stage: usize = parts.next().context("merge-tree stage")?.parse()?;
            if !(1..=3).contains(&stage) {
                bail!("unexpected merge stage {stage}");
            }
            match conflicts.last_mut() {
                Some(last) if last.path == path => last.stages[stage - 1] = Some((mode, oid)),
                _ => {
                    let mut stages = [None, None, None];
                    stages[stage - 1] = Some((mode, oid));
                    conflicts.push(MergeConflict { path, stages });
                }
            }
        }
        let messages = fields
            .map(|field| String::from_utf8_lossy(field).into_owned())
            .collect::<Vec<_>>()
            .join(" ");
        Ok(MergeResult {
            tree,
            conflicts,
            messages: escape_block(&messages),
        })
    }

    /// Three-way merges blobs; returns merged bytes and the conflict count.
    pub fn merge_file(
        &self,
        base: &str,
        ours: &str,
        theirs: &str,
        labels: [&str; 3],
        favor: Favor,
        diff3: bool,
    ) -> Result<(Vec<u8>, usize)> {
        require_git(2, 43)?;
        let mut args = vec!["merge-file", "--object-id", "-p"];
        match favor {
            Favor::None => {}
            Favor::Ours => args.push("--ours"),
            Favor::Theirs => args.push("--theirs"),
            Favor::Union => args.push("--union"),
        }
        if diff3 {
            args.push("--diff3");
        }
        for label in labels {
            args.push("-L");
            args.push(label);
        }
        args.extend([ours, base, theirs]);
        let output = self.raw(&args, &[], None)?;
        match output.code {
            Some(code) if (0..=127).contains(&code) => {
                Ok((output.stdout, usize::try_from(code).unwrap_or(0)))
            }
            _ => Err(Self::failure(&args, &output)),
        }
    }
}

/// A path `update-index` accepts and the in-process tree editor handles the same way: no empty,
/// `.`/`..` or `.git`-like components (Git also refuses HFS/NTFS spellings of `.git`).
fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && path.split('/').all(|component| {
            let folded: String = component
                .chars()
                .filter(|c| !matches!(*c, '\u{200c}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{206a}'..='\u{206f}' | '\u{feff}'))
                .collect::<String>()
                .to_ascii_lowercase();
            !(component.is_empty()
                || component == "."
                || component == ".."
                || folded.starts_with(".git")
                || folded.starts_with("git~"))
        })
}

/// An in-memory tree being edited: subtrees load lazily and unchanged ones keep their ids.
enum Node {
    Leaf(u32, String),
    Tree(String),
    Edited(BTreeMap<Vec<u8>, Node>),
}

fn load_node(store: &Store, tree: &str) -> Result<BTreeMap<Vec<u8>, Node>> {
    Ok(store
        .tree(tree)?
        .into_iter()
        .map(|entry| {
            let node = if entry.is_tree() {
                Node::Tree(entry.oid)
            } else {
                Node::Leaf(entry.mode, entry.oid)
            };
            (entry.name, node)
        })
        .collect())
}

fn edit_tree(store: &Store, base: &str, edits: &[TreeEdit]) -> Result<String> {
    let mut root = load_node(store, base)?;
    for edit in edits {
        let components: Vec<&[u8]> = edit.path.split('/').map(str::as_bytes).collect();
        let entry = match &edit.entry {
            Some((mode, oid)) => {
                let mode = u32::from_str_radix(mode, 8).context("tree entry mode")?;
                if mode & 0o170000 != 0o160000 && !store.exists(oid) {
                    bail!("invalid object {oid} for '{}'", edit.path);
                }
                Some((mode, oid.clone()))
            }
            None => None,
        };
        apply_edit(store, &mut root, &components, &entry)?;
    }
    write_node(store, root)?.map_or_else(|| store.write(store::Kind::Tree, b""), Ok)
}

fn apply_edit(
    store: &Store,
    children: &mut BTreeMap<Vec<u8>, Node>,
    components: &[&[u8]],
    entry: &Option<(u32, String)>,
) -> Result<()> {
    let (first, rest) = components.split_first().context("empty path")?;
    if rest.is_empty() {
        match entry {
            Some((mode, oid)) => {
                children.insert(first.to_vec(), Node::Leaf(*mode, oid.clone()));
            }
            None => {
                if matches!(children.get(*first), Some(Node::Leaf(..))) {
                    children.remove(*first);
                }
            }
        }
        return Ok(());
    }
    match children.get(*first) {
        // Nothing to remove below a missing directory or a file.
        None | Some(Node::Leaf(..)) if entry.is_none() => return Ok(()),
        // Adding below a file replaces the file, as update-index does.
        None | Some(Node::Leaf(..)) => {
            children.insert(first.to_vec(), Node::Edited(BTreeMap::new()));
        }
        Some(Node::Tree(oid)) => {
            let loaded = load_node(store, oid)?;
            children.insert(first.to_vec(), Node::Edited(loaded));
        }
        Some(Node::Edited(_)) => {}
    }
    let Some(Node::Edited(sub)) = children.get_mut(*first) else {
        bail!("tree editor invariant");
    };
    apply_edit(store, sub, rest, entry)
}

/// Writes an edited tree bottom-up; `None` for a tree left empty (Git never writes those).
fn write_node(store: &Store, children: BTreeMap<Vec<u8>, Node>) -> Result<Option<String>> {
    let mut entries = Vec::with_capacity(children.len());
    for (name, node) in children {
        let (mode, oid) = match node {
            Node::Leaf(mode, oid) => (mode, oid),
            Node::Tree(oid) => (0o040000, oid),
            Node::Edited(grandchildren) => match write_node(store, grandchildren)? {
                Some(oid) => (0o040000, oid),
                None => continue,
            },
        };
        entries.push(store::TreeEntry { mode, name, oid });
    }
    if entries.is_empty() {
        return Ok(None);
    }
    let bytes = store::encode_tree(&mut entries)?;
    store.write(store::Kind::Tree, &bytes).map(Some)
}

fn split_tab(record: &[u8]) -> Result<(&str, String)> {
    let tab = record
        .iter()
        .position(|byte| *byte == b'\t')
        .context("malformed Git record")?;
    Ok((
        text(&record[..tab])?,
        String::from_utf8(record[tab + 1..].to_vec())
            .map_err(|_| fail(Code::Unsupported, "non-UTF-8 paths are unsupported"))?,
    ))
}

/// Requires Git `major.minor` or newer (cached after the first call).
pub(crate) fn require_git(major: u32, minor: u32) -> Result<()> {
    use std::sync::OnceLock;
    static VERSION: OnceLock<(u32, u32)> = OnceLock::new();
    let version = VERSION.get_or_init(|| {
        let output = std::process::Command::new(crate::runner::program())
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .output();
        let text = output
            .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
            .unwrap_or_default();
        let mut numbers = text
            .split_whitespace()
            .nth(2)
            .unwrap_or("0.0")
            .split('.')
            .map(|part| part.parse::<u32>().unwrap_or(0));
        (numbers.next().unwrap_or(0), numbers.next().unwrap_or(0))
    });
    if *version >= (major, minor) {
        Ok(())
    } else {
        Err(fail(
            Code::Unsupported,
            format!(
                "Git {major}.{minor} or newer is required (found {}.{})",
                version.0, version.1
            ),
        ))
    }
}

/// A reference transaction applied atomically with compare-and-swap semantics.
#[derive(Default)]
pub(crate) struct RefTx {
    commands: Vec<String>,
}

impl RefTx {
    pub fn update(&mut self, name: &str, new: &str, old: &str) {
        self.commands.push(format!("update {name} {new} {old}"));
    }
    pub fn update_no_deref(&mut self, name: &str, new: &str, old: &str) {
        self.commands
            .push(format!("option no-deref\nupdate {name} {new} {old}"));
    }
    /// Fails if the reference already exists.
    pub fn create(&mut self, name: &str, new: &str) {
        self.commands.push(format!("create {name} {new}"));
    }
    pub fn delete(&mut self, name: &str, old: &str) {
        self.commands.push(format!("delete {name} {old}"));
    }

    pub fn commit(self, git: &Git, message: &str) -> Result<()> {
        let mut input = String::from("start\n");
        for command in &self.commands {
            input.push_str(command);
            input.push('\n');
        }
        input.push_str("prepare\ncommit\n");
        let args = ["update-ref", "--stdin", "-m", message];
        let output = git.raw(&args, &[], Some(input.into_bytes()))?;
        if output.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("cannot lock ref")
            || stderr.contains("but expected")
            || stderr.contains("already exists")
            || stderr.contains("reference already exists")
        {
            return Err(fail_hint(
                Code::Stale,
                "a reference changed while aisc was working; nothing was changed",
                "run aisc log and retry",
            ));
        }
        Err(Git::failure(&args, &output))
    }
}

pub(crate) fn path_exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn sh(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn repo() -> (tempfile::TempDir, Git) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().to_path_buf();
        sh(&root, &["init", "-b", "main"]);
        sh(&root, &["config", "user.name", "T"]);
        sh(&root, &["config", "user.email", "t@example.invalid"]);
        std::fs::write(root.join("a.txt"), "1\n2\n3\n").unwrap();
        sh(&root, &["add", "."]);
        sh(&root, &["commit", "-m", "base"]);
        let dir = root.join(".git");
        (directory, Git { root, dir })
    }

    #[test]
    fn blobs_trees_commits_and_metadata_round_trip() {
        let (_guard, git) = repo();
        let head = git.head_state().unwrap();
        let tree = git.commit_tree_of(&head.oid).unwrap();
        let blob = git.write_blob(b"hello\n", true).unwrap();
        let new_tree = git
            .tree_with(
                &tree,
                &[
                    TreeEdit {
                        path: "dir/b.txt".into(),
                        entry: Some(("100644".into(), blob.clone())),
                    },
                    TreeEdit {
                        path: "a.txt".into(),
                        entry: None,
                    },
                ],
            )
            .unwrap();
        let entries = git
            .ls_tree(&new_tree, &["dir/b.txt".into(), "a.txt".into()])
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries["dir/b.txt"].1, blob);
        assert_eq!(git.read_blobs(&[blob]).unwrap(), vec![b"hello\n".to_vec()]);
        let meta = git.read_commit_meta(&head.oid).unwrap();
        let again = git
            .commit_tree(&tree, &[], &meta.message, Some(&meta))
            .unwrap();
        assert_eq!(git.read_commit_meta(&again).unwrap().author, meta.author);
        // Hand-built commit with awkward but legal bytes survives a re-commit exactly.
        let object = format!(
            "tree {tree}\nauthor  Spaced  Name  <a@example.invalid> 1700000000 +0130\ncommitter C <c@example.invalid> 1700000001 -0200\n\nsubject  \n\ntrailing spaces   \nno final newline"
        );
        let hand = git
            .run_with(
                &["hash-object", "-t", "commit", "-w", "--stdin"],
                &[],
                Some(object.into_bytes()),
            )
            .unwrap();
        let hand = text(&hand).unwrap().trim().to_owned();
        let original = git.read_commit_meta(&hand).unwrap();
        let copy = git
            .commit_tree(
                &tree,
                &[],
                &original.message,
                Some(&original.clone().with_current_committer()),
            )
            .unwrap();
        let copied = git.read_commit_meta(&copy).unwrap();
        assert_eq!(
            copied.author,
            " Spaced  Name  <a@example.invalid> 1700000000 +0130"
        );
        assert_eq!(copied.message, original.message);
        assert!(copied.message.ends_with(b"no final newline"));
        assert_ne!(
            copied.committer, original.committer,
            "committer is the current identity"
        );
        let reparent = git
            .commit_tree(&tree, &[], &original.message, Some(&original))
            .unwrap();
        assert_eq!(
            git.read_commit_meta(&reparent).unwrap().committer,
            original.committer,
            "a pure re-parent keeps the committer"
        );
        assert_eq!(
            git.run(&["cat-file", "commit", &reparent]).unwrap(),
            git.run(&["cat-file", "commit", &hand]).unwrap(),
            "identical bytes"
        );
        assert!(
            git.dir
                .join("aisc/tmp")
                .read_dir()
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[test]
    fn merge_trees_and_files_report_conflicts() {
        let (_guard, git) = repo();
        let head = git.head_state().unwrap().oid;
        let base = git.commit_tree_of(&head).unwrap();
        let blob = |text: &str| git.write_blob(text.as_bytes(), true).unwrap();
        let tree = |text: &str| {
            git.tree_with(
                &base,
                &[TreeEdit {
                    path: "a.txt".into(),
                    entry: Some(("100644".into(), blob(text))),
                }],
            )
            .unwrap()
        };
        let clean = git
            .merge_trees(&base, &tree("1\n2\n3\n4\n"), &base)
            .unwrap();
        assert!(clean.conflicts.is_empty());
        let conflicted = git
            .merge_trees(&base, &tree("x\n2\n3\n"), &tree("y\n2\n3\n"))
            .unwrap();
        assert_eq!(conflicted.conflicts.len(), 1);
        assert_eq!(conflicted.conflicts[0].path, "a.txt");
        assert!(conflicted.conflicts[0].stages.iter().all(Option::is_some));
        let (merged, count) = git
            .merge_file(
                &blob("1\n2\n"),
                &blob("x\n2\n"),
                &blob("y\n2\n"),
                ["ONTO", "base", "br"],
                Favor::None,
                true,
            )
            .unwrap();
        assert_eq!(count, 1);
        assert!(String::from_utf8(merged).unwrap().contains("<<<<<<< ONTO"));
        let (theirs, count) = git
            .merge_file(
                &blob("1\n2\n"),
                &blob("x\n2\n"),
                &blob("y\n2\n"),
                ["a", "b", "c"],
                Favor::Theirs,
                false,
            )
            .unwrap();
        assert_eq!((theirs, count), (b"y\n2\n".to_vec(), 0));
    }

    #[test]
    fn ref_transactions_map_cas_failures_to_stale() {
        let (_guard, git) = repo();
        let head = git.head_state().unwrap().oid;
        let tree = git.commit_tree_of(&head).unwrap();
        let other = git
            .commit_tree(&tree, std::slice::from_ref(&head), b"other", None)
            .unwrap();
        let mut transaction = RefTx::default();
        transaction.update("refs/heads/main", &other, &head);
        transaction.create("refs/heads/side", &other);
        transaction.commit(&git, "aisc test").unwrap();
        let mut stale = RefTx::default();
        stale.update("refs/heads/main", &head, &head);
        let error = stale.commit(&git, "aisc test").unwrap_err();
        let error = error.downcast_ref::<super::super::HistoryError>().unwrap();
        assert_eq!(error.code, Code::Stale);
        let mut duplicate = RefTx::default();
        duplicate.create("refs/heads/side", &head);
        let error = duplicate.commit(&git, "aisc test").unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<super::super::HistoryError>()
                .unwrap()
                .code,
            Code::Stale
        );
        assert_eq!(git.ref_oid("refs/heads/main").unwrap().unwrap(), other);
    }

    /// A repository with packed and loose objects and refs, a tag, a remote-tracking symref, a
    /// symlink, an executable and a linked worktree.
    fn rich_repo() -> (tempfile::TempDir, Git, Vec<String>) {
        let (directory, git) = repo();
        let root = git.root.clone();
        let write = |path: &str, text: &str| {
            let full = root.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, text).unwrap();
        };
        write("dir/sub/x.txt", "x\n");
        write("dir/y.txt", "y\n");
        write("a.b", "dot\n");
        write("run.sh", "echo\n");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.join("run.sh"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        std::os::unix::fs::symlink("a.txt", root.join("link")).unwrap();
        sh(&root, &["add", "."]);
        sh(&root, &["commit", "-m", "second\n\nbody line"]);
        sh(&root, &["tag", "-a", "v1", "-m", "tag"]);
        sh(&root, &["gc", "-q"]);
        sh(&root, &["switch", "-q", "-c", "topic"]);
        write("dir/y.txt", "y2\n");
        std::fs::remove_file(root.join("a.b")).unwrap();
        sh(&root, &["commit", "-qam", "  third subject  "]);
        sh(&root, &["update-ref", "refs/remotes/origin/main", "main"]);
        sh(
            &root,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        let other = directory.path().join("other");
        sh(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "elsewhere",
                other.to_str().unwrap(),
            ],
        );
        let commits = sh(&root, &["rev-list", "--all"])
            .lines()
            .map(str::to_owned)
            .collect();
        (directory, git, commits)
    }

    #[test]
    fn in_process_reads_match_git() {
        let (_guard, git, commits) = rich_repo();
        assert!(git.fast(|_| Ok(())).is_some(), "fast path available");
        let head = git.head_state().unwrap();
        assert_eq!(head.oid, sh(&git.root, &["rev-parse", "HEAD"]));
        assert_eq!(head.symbolic.as_deref(), Some("refs/heads/topic"));
        for name in [
            "refs/heads/main",
            "refs/heads/topic",
            "refs/tags/v1",
            "refs/remotes/origin/HEAD",
            "refs/heads/absent",
            "HEAD",
        ] {
            assert_eq!(
                git.ref_oid(name).unwrap(),
                git.probe(&["rev-parse", "--verify", "--quiet", name])
                    .unwrap(),
                "{name}"
            );
        }
        for spec in [
            "main",
            "topic",
            "v1",
            "origin",
            "origin/main",
            "HEAD",
            "nope",
            // Git's first rule: names under $GIT_DIR, including per-worktree HEADs.
            "main-worktree/HEAD",
            "worktrees/other/HEAD",
            "ORIG_HEAD",
            &commits[0],
        ] {
            let argument = format!("{spec}^{{commit}}");
            assert_eq!(
                git.resolve_commit(spec).unwrap(),
                git.probe(&["rev-parse", "--verify", "--quiet", &argument])
                    .unwrap(),
                "{spec}"
            );
        }
        for commit in &commits {
            let argument = format!("{commit}^{{tree}}");
            let tree = git.commit_tree_of(commit).unwrap();
            assert_eq!(
                tree,
                git.line(&["rev-parse", "--verify", &argument]).unwrap()
            );
            let paths = vec!["dir".to_owned(), "link".to_owned(), "run.sh".to_owned()];
            let mut expected = BTreeMap::new();
            let output = git
                .run(&[
                    "ls-tree",
                    "-r",
                    "-z",
                    "--full-tree",
                    &tree,
                    "--",
                    "dir",
                    "link",
                    "run.sh",
                ])
                .unwrap();
            for record in output.split(|byte| *byte == 0).filter(|r| !r.is_empty()) {
                let (meta, path) = split_tab(record).unwrap();
                let fields: Vec<&str> = meta.split(' ').collect();
                expected.insert(path, (fields[0].to_owned(), fields[2].to_owned()));
            }
            assert_eq!(git.ls_tree(&tree, &paths).unwrap(), expected);
            let raw = git.run(&["cat-file", "commit", commit]).unwrap();
            assert_eq!(
                git.read_commit_meta(commit).unwrap().message,
                raw[raw.windows(2).position(|pair| pair == b"\n\n").unwrap() + 2..].to_vec()
            );
            let blobs: Vec<String> = expected.values().map(|(_, oid)| oid.clone()).collect();
            for (blob, bytes) in blobs.iter().zip(git.read_blobs(&blobs).unwrap()) {
                assert_eq!(bytes, git.run(&["cat-file", "blob", blob]).unwrap());
            }
        }
        // A ref to a missing object: listed exactly as for-each-ref lists it.
        std::fs::write(
            git.dir.join("refs/heads/broken"),
            "1234567890123456789012345678901234567890\n",
        )
        .unwrap();
        let listed = git
            .fast(|store| store.list_refs(&["refs/heads/", "refs/remotes/"]))
            .unwrap();
        let expected = git
            .line(&[
                "for-each-ref",
                "--format=%(refname) %(objectname) %(symref)",
                "refs/heads",
                "refs/remotes",
            ])
            .unwrap();
        let listed: Vec<String> = listed
            .iter()
            .map(|(name, oid, symref)| format!("{name} {oid} {symref}"))
            .collect();
        assert_eq!(listed.join("\n"), expected);
        let mut checked_out = git.checked_out().unwrap();
        checked_out.sort();
        assert_eq!(checked_out, ["refs/heads/elsewhere", "refs/heads/topic"]);
        let ids = git.short_ids(&commits).unwrap();
        let mut arguments = vec!["log", "--no-walk=unsorted", "--abbrev=4", "--format=%h"];
        arguments.extend(commits.iter().map(String::as_str));
        assert_eq!(ids.join("\n"), git.line(&arguments).unwrap());
        // The branch range as `git log` prints it, subjects and file lists included.
        let main = git.ref_oid("refs/heads/main").unwrap().unwrap();
        let (walked, counts) = super::super::view::walk(&git, &head.oid, Some(&main), 20).unwrap();
        let log = git
            .run(&[
                "-c",
                "core.quotePath=false",
                "log",
                "--first-parent",
                "--no-renames",
                "--name-only",
                "--abbrev=4",
                "--format=%x01%H%x1f%h%x1f%T%x1f%P%x1f%s",
                &head.oid,
                "^refs/heads/main",
                "--",
            ])
            .unwrap();
        let parsed = super::super::view::parse_log(&log).unwrap();
        assert_eq!(format!("{walked:?}"), format!("{parsed:?}"));
        assert_eq!(counts, Some((1, 0)));
        let (all, _) = super::super::view::walk(&git, &head.oid, None, 20).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[2].files, ["a.txt"], "a root commit lists its files");
        // Tree changes as `diff-tree -r` lists them.
        let trees: Vec<String> = all.iter().map(|commit| commit.tree.clone()).collect();
        let changes = git.tree_changes(&trees[2], &trees[0]).unwrap();
        let raw = git
            .run(&[
                "diff-tree",
                "-r",
                "-z",
                "--no-renames",
                "--no-abbrev",
                &trees[2],
                &trees[0],
            ])
            .unwrap();
        let fields: Vec<&[u8]> = raw
            .split(|byte| *byte == 0)
            .filter(|f| !f.is_empty())
            .collect();
        let paths: Vec<String> = fields
            .chunks(2)
            .map(|pair| String::from_utf8(pair[1].to_vec()).unwrap())
            .collect();
        assert_eq!(
            changes
                .iter()
                .map(|(path, _, _)| path.clone())
                .collect::<Vec<_>>(),
            paths
        );
    }

    #[test]
    fn in_process_trees_match_update_index_and_write_tree() {
        let (_guard, git, _) = rich_repo();
        let base = git.commit_tree_of("HEAD").unwrap();
        let blob = git.write_blob(b"new\n", true).unwrap();
        let cases: Vec<Vec<TreeEdit>> = vec![
            vec![TreeEdit {
                path: "deep/new/file.txt".into(),
                entry: Some(("100644".into(), blob.clone())),
            }],
            // A file replaces a directory, and a directory a file.
            vec![
                TreeEdit {
                    path: "dir".into(),
                    entry: Some(("100755".into(), blob.clone())),
                },
                TreeEdit {
                    path: "a.txt/inner".into(),
                    entry: Some(("100644".into(), blob.clone())),
                },
            ],
            // Removing the last file of a directory removes the directory; removing a
            // directory name or a missing path does nothing.
            vec![
                TreeEdit {
                    path: "dir/sub/x.txt".into(),
                    entry: None,
                },
                TreeEdit {
                    path: "dir".into(),
                    entry: None,
                },
                TreeEdit {
                    path: "missing/file".into(),
                    entry: None,
                },
            ],
            vec![TreeEdit {
                path: "link".into(),
                entry: Some(("120000".into(), blob.clone())),
            }],
            vec![],
        ];
        for edits in cases {
            let ours = git.tree_with(&base, &edits).unwrap();
            let theirs = git.tree_with_git(&base, &edits).unwrap();
            assert_eq!(ours, theirs);
        }
        let entries = vec![
            ("wt/1".to_owned(), "100644".to_owned(), blob.clone()),
            ("op.json".to_owned(), "100644".to_owned(), blob.clone()),
            ("wt/0".to_owned(), "100644".to_owned(), blob.clone()),
        ];
        let empty = git.empty_tree().unwrap();
        let edits: Vec<TreeEdit> = entries
            .iter()
            .map(|(path, mode, oid)| TreeEdit {
                path: path.clone(),
                entry: Some((mode.clone(), oid.clone())),
            })
            .collect();
        assert_eq!(
            git.tree_of_entries(&entries).unwrap(),
            git.tree_with_git(&empty, &edits).unwrap()
        );
    }

    #[test]
    fn trivial_merges_match_merge_tree_and_the_rest_goes_to_git() {
        let (_guard, git, _) = rich_repo();
        let base = git.commit_tree_of("HEAD").unwrap();
        let blob = |text: &str| git.write_blob(text.as_bytes(), true).unwrap();
        let tree = |edits: Vec<(&str, Option<&str>)>| {
            let edits: Vec<TreeEdit> = edits
                .into_iter()
                .map(|(path, text)| TreeEdit {
                    path: path.to_owned(),
                    entry: text.map(|text| ("100644".to_owned(), blob(text))),
                })
                .collect();
            git.tree_with(&base, &edits).unwrap()
        };
        let merge_tree = |ours: &str, theirs: &str| {
            let argument = format!("--merge-base={base}");
            let output = git
                .raw(
                    &["merge-tree", "--write-tree", "-z", &argument, ours, theirs],
                    &[],
                    None,
                )
                .unwrap();
            assert_eq!(output.code, Some(0));
            String::from_utf8(output.stdout)
                .unwrap()
                .trim_end_matches('\0')
                .to_owned()
        };
        let trivial = [
            (
                tree(vec![("n1.txt", Some("1\n"))]),
                tree(vec![("dir/n2.txt", Some("2\n"))]),
            ),
            (
                tree(vec![("a.txt", None)]),
                tree(vec![("dir/y.txt", Some("other\n"))]),
            ),
            (tree(vec![("dir/sub/x.txt", Some("x2\n"))]), base.clone()),
            (base.clone(), tree(vec![("q.txt", Some("q\n"))])),
        ];
        for (ours, theirs) in &trivial {
            let found = git.trivial_merge(&base, ours, theirs).expect("trivial");
            assert_eq!(found, merge_tree(ours, theirs));
        }
        let not_trivial = [
            // Both sides touch the same path.
            (
                tree(vec![("a.txt", Some("x\n"))]),
                tree(vec![("a.txt", Some("y\n"))]),
            ),
            // A possible rename (delete and add on one side).
            (
                tree(vec![("a.txt", None), ("moved.txt", Some("1\n2\n3\n"))]),
                tree(vec![("b.txt", Some("b\n"))]),
            ),
            // A path of one side above a path of the other.
            (
                tree(vec![("p", Some("file\n"))]),
                tree(vec![("p/q", Some("nested\n"))]),
            ),
        ];
        for (ours, theirs) in &not_trivial {
            assert_eq!(git.trivial_merge(&base, ours, theirs), None);
        }
    }
}
