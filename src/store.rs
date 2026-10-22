//! In-process access to a repository's objects, references and index through gitoxide's
//! `gix-*` crates, so that reads (and loose-object writes) need no Git subprocess.
//!
//! Every entry point returns `None` when this repository uses something the fast path cannot
//! reproduce exactly: reftable or SHA-256 storage, unknown extensions, replace refs, grafts,
//! shallow history, `GIT_NAMESPACE`. Callers then run Git as before, so behaviour never depends
//! on which path answered. Objects are content-addressed, so a loose object written here is
//! byte-for-byte the one `git hash-object -w` would write.
use anyhow::{Context, Result, bail};
use gix_object::{Find, Write as _};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub(crate) use gix_object::Kind;

/// One tree entry: `(mode, oid)`, mode as `ls-tree` prints it (`100644`, `040000`, ...).
pub(crate) type Entry = (String, String);

pub(crate) struct Store {
    pub common_dir: PathBuf,
    odb: gix_odb::Handle,
    refs: gix_ref::file::Store,
}

/// Per thread (the gix handles are not `Sync`): git dir → (generation, store if usable).
type Stores = HashMap<PathBuf, (u64, Option<Rc<Store>>)>;

thread_local! {
    static STORES: RefCell<Stores> = RefCell::new(HashMap::new());
}

/// The store of `git_dir`, or `None` when the fast path is unavailable for this repository.
pub(crate) fn open(git_dir: &Path) -> Option<Rc<Store>> {
    let generation = crate::runner::generation();
    if let Some(found) = STORES.with(|stores| {
        stores
            .borrow()
            .get(git_dir)
            .filter(|(seen, _)| *seen == generation)
            .map(|(_, store)| store.clone())
    }) {
        return found;
    }
    let store = Store::at(git_dir).ok().flatten().map(Rc::new);
    STORES.with(|stores| {
        stores
            .borrow_mut()
            .insert(git_dir.to_owned(), (generation, store.clone()))
    });
    store
}

/// Runs `body` on the store; `None` (fast path unavailable, or it failed) means "ask Git".
pub(crate) fn with<T>(git_dir: &Path, body: impl FnOnce(&Store) -> Result<T>) -> Option<T> {
    let store = open(git_dir)?;
    body(&store).ok()
}

fn looks_like_git_dir(path: &Path) -> bool {
    path.join("HEAD").is_file()
        && (path.join("commondir").is_file()
            || (path.join("objects").is_dir() && path.join("refs").is_dir()))
}

/// `git rev-parse --show-toplevel` and `--absolute-git-dir` from `cwd`, without Git, for the
/// ordinary layouts: a `.git` directory, or a `.git` file (linked worktree, submodule) in `cwd`
/// or above it. `None` for everything else Git decides itself: ceiling directories, crossing a
/// file system, a directory inside a Git dir, bare repositories, `core.worktree`, configuration
/// includes, and repositories owned by another user (`safe.directory`).
pub(crate) fn discover(cwd: &Path) -> Option<(PathBuf, PathBuf)> {
    use std::os::unix::fs::MetadataExt;
    if std::env::var_os("GIT_CEILING_DIRECTORIES").is_some()
        || std::env::var_os("GIT_DISCOVERY_ACROSS_FILESYSTEM").is_some()
    {
        return None;
    }
    let device = std::fs::metadata(cwd).ok()?.dev();
    let mut directory = cwd.to_owned();
    let git_dir = loop {
        let candidate = directory.join(".git");
        match std::fs::symlink_metadata(&candidate) {
            Ok(metadata) if metadata.is_dir() && looks_like_git_dir(&candidate) => {
                break candidate;
            }
            Ok(metadata) if metadata.is_file() => {
                let text = std::fs::read_to_string(&candidate).ok()?;
                let target = text
                    .strip_prefix("gitdir: ")?
                    .trim_end_matches(['\n', '\r']);
                let target = directory.join(target);
                if !looks_like_git_dir(&target) {
                    return None;
                }
                break target;
            }
            Ok(_) => return None,
            Err(_) => {}
        }
        if looks_like_git_dir(&directory) {
            return None; // inside a Git directory, or a bare repository
        }
        let parent = directory.parent()?.to_owned();
        if std::fs::metadata(&parent).ok()?.dev() != device {
            return None;
        }
        directory = parent;
    };
    let root = std::fs::canonicalize(&directory).ok()?;
    let git_dir = std::fs::canonicalize(&git_dir).ok()?;
    let common = common_dir_of(&git_dir).ok()?;
    // SAFETY: geteuid has no preconditions.
    let user = unsafe { libc::geteuid() };
    for owned in [&root, &git_dir, &common] {
        if std::fs::metadata(owned).ok()?.uid() != user {
            return None;
        }
    }
    if git_dir.join("config.worktree").exists() {
        return None;
    }
    let config = std::fs::read_to_string(common.join("config")).ok()?;
    for line in config.lines() {
        let line = line.trim().to_ascii_lowercase();
        let key = line.split(['=', ' ', '\t']).next().unwrap_or_default();
        if line.starts_with("[include")
            || key == "worktree"
            || key == "bare" && line.contains("true")
        {
            return None;
        }
    }
    Some((root, git_dir))
}

fn common_dir_of(git_dir: &Path) -> Result<PathBuf> {
    match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(text) => {
            let path = Path::new(text.trim_end_matches(['\n', '\r']));
            Ok(std::fs::canonicalize(git_dir.join(path))?)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(git_dir.to_owned()),
        Err(error) => Err(error.into()),
    }
}

/// Whether the repository configuration (format version and extensions) is one the fast path
/// reads exactly. Extensions are only ever read from the repository's own config file.
fn supported_format(config: &str) -> bool {
    let mut section = String::new();
    for raw in config.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            let name = line
                .trim_start_matches('[')
                .split([']', ' ', '"', '.'])
                .next()
                .unwrap_or_default();
            section = name.to_ascii_lowercase();
            // A section header may carry a variable on the same line: be conservative.
            if line
                .find(']')
                .is_some_and(|end| !line[end + 1..].trim().is_empty())
                && !line[line.find(']').unwrap_or(0) + 1..]
                    .trim()
                    .starts_with(['#', ';'])
            {
                return false;
            }
            continue;
        }
        if line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        let (key, value) = line.split_once('=').unwrap_or((line, "true"));
        let key = key.trim().to_ascii_lowercase();
        let value = value
            .split(['#', ';'])
            .next()
            .unwrap_or_default()
            .trim()
            .trim_matches('"')
            .to_ascii_lowercase();
        match (section.as_str(), key.as_str()) {
            ("core", "repositoryformatversion") if !matches!(value.as_str(), "0" | "1") => {
                return false;
            }
            ("extensions", "objectformat") if value != "sha1" => return false,
            ("extensions", "refstorage") if value != "files" => return false,
            ("extensions", "objectformat" | "refstorage" | "worktreeconfig" | "noop") => {}
            ("extensions", _) => return false,
            _ => {}
        }
    }
    true
}

fn has_entries(directory: &Path) -> bool {
    std::fs::read_dir(directory).is_ok_and(|mut entries| entries.next().is_some())
}

impl Store {
    fn at(git_dir: &Path) -> Result<Option<Self>> {
        for name in [
            "GIT_NAMESPACE",
            "GIT_REPLACE_REF_BASE",
            "GIT_SHALLOW_FILE",
            "GIT_GRAFT_FILE",
        ] {
            if std::env::var_os(name).is_some() {
                return Ok(None);
            }
        }
        let common_dir = common_dir_of(git_dir)?;
        let config = std::fs::read_to_string(common_dir.join("config")).unwrap_or_default();
        if !supported_format(&config)
            || has_entries(&common_dir.join("refs/replace"))
            || common_dir.join("shallow").exists()
            || common_dir.join("info/grafts").exists()
            || std::fs::read(common_dir.join("packed-refs"))
                .is_ok_and(|packed| packed.windows(13).any(|window| window == b"refs/replace/"))
        {
            return Ok(None);
        }
        let odb = gix_odb::at(common_dir.join("objects"))?;
        let options = gix_ref::store::init::Options {
            write_reflog: gix_ref::store::WriteReflog::Disable,
            object_hash: gix_hash::Kind::Sha1,
            precompose_unicode: false,
            prohibit_windows_device_names: false,
        };
        let refs = if common_dir == git_dir {
            gix_ref::file::Store::at(git_dir.to_owned(), options)
        } else {
            gix_ref::file::Store::for_linked_worktree(
                git_dir.to_owned(),
                common_dir.clone(),
                options,
            )
        };
        Ok(Some(Self {
            common_dir,
            odb,
            refs,
        }))
    }

    // ------------------------------------------------------------------------- objects

    pub fn object(&self, oid: &str) -> Result<Option<(Kind, Vec<u8>)>> {
        let id = parse_oid(oid)?;
        let mut buffer = Vec::new();
        Ok(self
            .odb
            .try_find(&id, &mut buffer)
            .map_err(|error| anyhow::anyhow!("{error}"))?
            .map(|data| (data.kind, data.data.to_vec())))
    }

    pub fn exists(&self, oid: &str) -> bool {
        matches!(self.object(oid), Ok(Some(_)))
    }

    pub fn object_of(&self, oid: &str, kind: Kind) -> Result<Vec<u8>> {
        match self.object(oid)? {
            Some((found, data)) if found == kind => Ok(data),
            _ => bail!("object {oid} is not a {kind}"),
        }
    }

    /// Peels tags until a commit; `None` when `oid` is missing or not a commit-ish.
    pub fn peel_to_commit(&self, oid: &str) -> Result<Option<String>> {
        let mut current = oid.to_owned();
        for _ in 0..32 {
            match self.object(&current)? {
                Some((Kind::Commit, _)) => return Ok(Some(current)),
                Some((Kind::Tag, data)) => {
                    let target = header_value(&data, b"object ").context("tag without object")?;
                    current = target;
                }
                _ => return Ok(None),
            }
        }
        Ok(None)
    }

    pub fn write(&self, kind: Kind, bytes: &[u8]) -> Result<String> {
        Ok(self
            .odb
            .write_buf(kind, bytes)
            .map_err(|error| anyhow::anyhow!("{error}"))?
            .to_string())
    }

    pub fn commit(&self, oid: &str) -> Result<RawCommit> {
        RawCommit::parse(&self.object_of(oid, Kind::Commit)?)
    }

    pub fn tree(&self, oid: &str) -> Result<Vec<TreeEntry>> {
        parse_tree(&self.object_of(oid, Kind::Tree)?)
    }

    /// The entry at `path` (slash separated) in `tree`.
    pub fn entry_at(&self, tree: &str, path: &str) -> Result<Option<Entry>> {
        let mut current = tree.to_owned();
        let components: Vec<&str> = path.split('/').collect();
        for (position, component) in components.iter().enumerate() {
            let entries = self.tree(&current)?;
            let Some(found) = entries
                .iter()
                .find(|entry| entry.name == component.as_bytes())
            else {
                return Ok(None);
            };
            if position + 1 == components.len() {
                return Ok(Some((found.mode_string(), found.oid.clone())));
            }
            if !found.is_tree() {
                return Ok(None);
            }
            current.clone_from(&found.oid);
        }
        Ok(None)
    }

    /// Every non-tree entry below `tree`, keyed by full path (`ls-tree -r`).
    pub fn flatten(
        &self,
        tree: &str,
        prefix: &str,
        out: &mut BTreeMap<String, Entry>,
    ) -> Result<()> {
        for entry in self.tree(tree)? {
            let name = String::from_utf8(entry.name.clone()).context("non-UTF-8 path")?;
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            if entry.is_tree() {
                self.flatten(&entry.oid, &path, out)?;
            } else {
                out.insert(path, (entry.mode_string(), entry.oid.clone()));
            }
        }
        Ok(())
    }

    /// Paths whose entries differ between two trees (`diff-tree -r --name-only --no-renames`
    /// order), with the entry on each side.
    pub fn diff_trees(
        &self,
        old: Option<&str>,
        new: Option<&str>,
        prefix: &str,
        out: &mut Vec<(String, Option<Entry>, Option<Entry>)>,
    ) -> Result<()> {
        if old == new {
            return Ok(());
        }
        let load = |tree: Option<&str>| -> Result<Vec<TreeEntry>> {
            tree.map_or(Ok(Vec::new()), |tree| self.tree(tree))
        };
        let (left, right) = (load(old)?, load(new)?);
        let (mut i, mut j) = (0, 0);
        while i < left.len() || j < right.len() {
            let order = match (left.get(i), right.get(j)) {
                (Some(a), Some(b)) => tree_order(a, b),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, _) => std::cmp::Ordering::Greater,
            };
            let (a, b) = match order {
                std::cmp::Ordering::Less => {
                    i += 1;
                    (Some(&left[i - 1]), None)
                }
                std::cmp::Ordering::Greater => {
                    j += 1;
                    (None, Some(&right[j - 1]))
                }
                std::cmp::Ordering::Equal => {
                    i += 1;
                    j += 1;
                    (Some(&left[i - 1]), Some(&right[j - 1]))
                }
            };
            let entry = a.or(b).expect("one side");
            let name = String::from_utf8(entry.name.clone()).context("non-UTF-8 path")?;
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            if entry.is_tree() {
                let sub = |side: Option<&TreeEntry>| side.map(|entry| entry.oid.clone());
                let (a, b) = (sub(a), sub(b));
                if a != b {
                    self.diff_trees(a.as_deref(), b.as_deref(), &path, out)?;
                }
            } else {
                let side =
                    |side: Option<&TreeEntry>| side.map(|e| (e.mode_string(), e.oid.clone()));
                let (a, b) = (side(a), side(b));
                if a != b {
                    out.push((path, a, b));
                }
            }
        }
        Ok(())
    }

    /// The shortest unique prefix of `oid`, at least `minimum` hex digits (`--abbrev=N`).
    pub fn abbrev(&self, oid: &str, minimum: usize) -> Result<String> {
        let id = parse_oid(oid)?;
        let candidate = gix_odb::store::prefix::disambiguate::Candidate::new(id, minimum)
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let prefix = self
            .odb
            .disambiguate_prefix(candidate)
            .map_err(|error| anyhow::anyhow!("{error}"))?
            .context("object to abbreviate is missing")?;
        Ok(prefix.to_string())
    }

    // ------------------------------------------------------------------------- references

    /// The direct target of a reference: `Ok(Ok(oid))` or `Ok(Err(symbolic target))`.
    fn find(&self, name: &str) -> Result<Option<gix_ref::Reference>> {
        self.refs
            .try_find(name)
            .map_err(|error| anyhow::anyhow!("{error}"))
    }

    /// The object a reference finally points to (`rev-parse --verify NAME`), following symbolic
    /// refs; `None` when it does not exist or dangles.
    pub fn resolve_ref(&self, name: &str) -> Result<Option<String>> {
        let mut current = name.to_owned();
        for _ in 0..8 {
            let Some(reference) = self.find(&current)? else {
                return Ok(None);
            };
            match reference.target {
                gix_ref::Target::Object(oid) => return Ok(Some(oid.to_string())),
                gix_ref::Target::Symbolic(target) => current = target.as_bstr().to_string(),
            }
        }
        bail!("symbolic reference chain too deep")
    }

    /// `rev-parse --verify --quiet NAME` for a reference name, following Git's lookup rules
    /// (`NAME`, `refs/NAME`, `refs/tags/NAME`, `refs/heads/NAME`, `refs/remotes/NAME`,
    /// `refs/remotes/NAME/HEAD`; the first that resolves wins). `Ok(None)` when the spec is
    /// revision syntax, a pseudo-ref or possibly an abbreviated id: Git decides those.
    pub fn dwim(&self, spec: &str) -> Result<Option<Option<String>>> {
        if !check_ref_format(spec, true) {
            return Ok(None);
        }
        let mut candidates = Vec::new();
        if spec == "HEAD" || spec.starts_with("refs/") {
            candidates.push(spec.to_owned());
        } else if self.common_dir.join(spec).exists() || self.refs.git_dir().join(spec).exists() {
            return Ok(None);
        }
        for pattern in [
            "refs/{}",
            "refs/tags/{}",
            "refs/heads/{}",
            "refs/remotes/{}",
            "refs/remotes/{}/HEAD",
        ] {
            candidates.push(pattern.replace("{}", spec));
        }
        for candidate in candidates {
            if let Some(oid) = self.resolve_ref(&candidate)? {
                return Ok(Some(Some(oid)));
            }
        }
        if spec.len() >= 4 && spec.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Ok(None);
        }
        Ok(Some(None))
    }

    /// `rev-parse --verify REV^{tree}` for an object id or reference name.
    pub fn peel_to_tree(&self, rev: &str) -> Result<Option<String>> {
        let mut current = if rev.len() == 40 && rev.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            rev.to_ascii_lowercase()
        } else {
            match self.dwim(rev)? {
                Some(Some(oid)) => oid,
                _ => return Ok(None),
            }
        };
        for _ in 0..32 {
            match self.object(&current)? {
                Some((Kind::Tree, _)) => return Ok(Some(current)),
                Some((Kind::Commit, data)) => {
                    current = header_value(&data, b"tree ").context("commit without tree")?;
                }
                Some((Kind::Tag, data)) => {
                    current = header_value(&data, b"object ").context("tag without object")?;
                }
                _ => return Ok(None),
            }
        }
        Ok(None)
    }

    /// `symbolic-ref -q HEAD` and `rev-parse --verify HEAD^{commit}`.
    pub fn head(&self) -> Result<(Option<String>, Option<String>)> {
        let reference = self.find("HEAD")?.context("HEAD is missing")?;
        let symbolic = match &reference.target {
            gix_ref::Target::Symbolic(target) => Some(target.as_bstr().to_string()),
            gix_ref::Target::Object(_) => None,
        };
        let oid = match self.resolve_ref("HEAD")? {
            Some(oid) => self.peel_to_commit(&oid)?,
            None => None,
        };
        Ok((symbolic, oid))
    }

    /// `for-each-ref --format=%(refname)%00%(objectname)%00%(symref) PREFIX…`, sorted by name.
    pub fn list_refs(&self, prefixes: &[&str]) -> Result<Vec<(String, String, String)>> {
        let platform = self
            .refs
            .iter()
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let mut found = Vec::new();
        for prefix in prefixes {
            let prefix: &gix_path::RelativePath = (*prefix)
                .try_into()
                .map_err(|error| anyhow::anyhow!("{error:?}"))?;
            for reference in platform.prefixed(prefix)? {
                let reference = reference.map_err(|error| anyhow::anyhow!("{error}"))?;
                let name = reference.name.as_bstr().to_string();
                match reference.target {
                    gix_ref::Target::Object(oid) => {
                        found.push((name, oid.to_string(), String::new()))
                    }
                    gix_ref::Target::Symbolic(target) => {
                        let target = target.as_bstr().to_string();
                        // for-each-ref skips symbolic refs that dangle.
                        if let Some(oid) = self.resolve_ref(&target)? {
                            found.push((name, oid, target));
                        }
                    }
                }
            }
        }
        found.sort();
        Ok(found)
    }

    /// Branches checked out in any worktree (`worktree list --porcelain` `branch` lines).
    pub fn checked_out(&self) -> Result<Vec<String>> {
        let mut heads = vec![self.common_dir.join("HEAD")];
        if let Ok(entries) = std::fs::read_dir(self.common_dir.join("worktrees")) {
            let mut linked: Vec<PathBuf> = entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path().join("HEAD"))
                .filter(|path| path.is_file())
                .collect();
            linked.sort();
            heads.extend(linked);
        }
        let bare = std::fs::read_to_string(self.common_dir.join("config"))
            .unwrap_or_default()
            .lines()
            .any(|line| {
                let line = line.trim().to_ascii_lowercase().replace(' ', "");
                line == "bare=true"
            });
        let mut branches = Vec::new();
        for (index, head) in heads.iter().enumerate() {
            if index == 0 && bare {
                continue;
            }
            let text = std::fs::read_to_string(head)?;
            if let Some(target) = text.trim_end().strip_prefix("ref: ")
                && target.starts_with("refs/heads/")
            {
                branches.push(target.to_owned());
            }
        }
        Ok(branches)
    }
}

// ----------------------------------------------------------------------------- the index

/// One index entry: path, mode, oid, stage and the flags the verbs care about.
#[derive(Clone, Debug)]
pub(crate) struct IndexEntry {
    pub path: String,
    pub mode: String,
    pub oid: String,
    pub stage: u8,
    pub assume_valid: bool,
    pub skip_worktree: bool,
    pub intent_to_add: bool,
}

/// Reads `GIT_DIR/index`; an absent index is empty.
pub(crate) fn read_index(git_dir: &Path) -> Result<Vec<IndexEntry>> {
    let path = git_dir.join("index");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = gix_index::File::at(
        path,
        gix_hash::Kind::Sha1,
        false,
        gix_index::decode::Options::default(),
    )
    .map_err(|error| anyhow::anyhow!("{error}"))?;
    entries_of(&file)
}

fn entries_of(file: &gix_index::File) -> Result<Vec<IndexEntry>> {
    use gix_index::entry::Flags;
    file.entries()
        .iter()
        .map(|entry| {
            Ok(IndexEntry {
                path: String::from_utf8(entry.path(file).to_vec()).context("non-UTF-8 path")?,
                mode: format!("{:06o}", entry.mode.bits()),
                oid: entry.id.to_string(),
                stage: entry.stage_raw() as u8,
                assume_valid: entry.flags.contains(Flags::ASSUME_VALID),
                skip_worktree: entry.flags.contains(Flags::SKIP_WORKTREE),
                intent_to_add: entry.flags.contains(Flags::INTENT_TO_ADD),
            })
        })
        .collect()
}

/// A version 2 index of stage-0 entries with zeroed stat data, for `git write-tree`.
pub(crate) fn write_index(entries: &[(String, String, String)]) -> Result<Vec<u8>> {
    let mut sorted: Vec<&(String, String, String)> = entries.iter().collect();
    sorted.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut out = Vec::new();
    out.extend_from_slice(b"DIRC");
    out.extend_from_slice(&2u32.to_be_bytes());
    out.extend_from_slice(&u32::try_from(sorted.len())?.to_be_bytes());
    for (path, mode, oid) in sorted {
        let start = out.len();
        out.extend_from_slice(&[0u8; 24]); // ctime, mtime, dev, ino
        let mode = u32::from_str_radix(mode, 8).context("index mode")?;
        out.extend_from_slice(&mode.to_be_bytes());
        out.extend_from_slice(&[0u8; 12]); // uid, gid, size
        out.extend_from_slice(parse_oid(oid)?.as_bytes());
        let length = u16::try_from(path.len().min(0x0fff))?;
        out.extend_from_slice(&length.to_be_bytes());
        out.extend_from_slice(path.as_bytes());
        let used = out.len() - start;
        let padded = (used + 8) & !7;
        out.resize(start + padded, 0);
    }
    let mut hasher = gix_hash::hasher(gix_hash::Kind::Sha1);
    hasher.update(&out);
    let digest = hasher
        .try_finalize()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    out.extend_from_slice(digest.as_bytes());
    Ok(out)
}

/// The id Git assigns to `bytes` stored as an object of `kind`.
pub(crate) fn hash_object(kind: Kind, bytes: &[u8]) -> Result<String> {
    let mut hasher = gix_hash::hasher(gix_hash::Kind::Sha1);
    hasher.update(format!("{kind} {}\0", bytes.len()).as_bytes());
    hasher.update(bytes);
    Ok(hasher
        .try_finalize()
        .map_err(|error| anyhow::anyhow!("{error}"))?
        .to_string())
}

// ----------------------------------------------------------------------------- parsing

fn parse_oid(oid: &str) -> Result<gix_hash::ObjectId> {
    if oid.len() != 40 {
        bail!("not a full object id: {oid}");
    }
    gix_hash::ObjectId::from_hex(oid.as_bytes()).map_err(|error| anyhow::anyhow!("{error}"))
}

fn header_value(data: &[u8], key: &[u8]) -> Option<String> {
    for line in data.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix(key) {
            return String::from_utf8(value.to_vec()).ok();
        }
    }
    None
}

/// A commit's tree, parents and raw bytes.
#[derive(Clone, Debug)]
pub(crate) struct RawCommit {
    pub tree: String,
    pub parents: Vec<String>,
    pub raw: Vec<u8>,
}

impl RawCommit {
    fn parse(data: &[u8]) -> Result<Self> {
        let mut tree = None;
        let mut parents = Vec::new();
        for line in data.split(|byte| *byte == b'\n') {
            if line.is_empty() {
                break;
            }
            if let Some(value) = line.strip_prefix(b"tree ") {
                tree = Some(String::from_utf8(value.to_vec())?);
            } else if let Some(value) = line.strip_prefix(b"parent ") {
                parents.push(String::from_utf8(value.to_vec())?);
            }
        }
        Ok(Self {
            tree: tree.context("commit without tree")?,
            parents,
            raw: data.to_vec(),
        })
    }

    /// `%s`: the first paragraph of the message, its lines joined by spaces (`None`: not UTF-8).
    pub fn subject(&self) -> Option<String> {
        let split = self
            .raw
            .windows(2)
            .position(|pair| pair == b"\n\n")
            .map_or(self.raw.len(), |position| position + 2);
        let message = std::str::from_utf8(&self.raw[split..]).ok()?;
        let mut lines = Vec::new();
        for line in message.split('\n') {
            // Git's isspace() is space, tab, CR and LF.
            let line = line.trim_end_matches([' ', '\t', '\r']);
            if line.is_empty() {
                if lines.is_empty() {
                    continue;
                }
                break;
            }
            lines.push(line.to_owned());
        }
        Some(lines.join(" "))
    }

    pub fn has_encoding(&self) -> bool {
        header_value(&self.raw, b"encoding ").is_some()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TreeEntry {
    pub mode: u32,
    pub name: Vec<u8>,
    pub oid: String,
}

impl TreeEntry {
    pub fn is_tree(&self) -> bool {
        self.mode & 0o170000 == 0o040000
    }
    /// Git's canonical mode, as `ls-tree` prints it.
    pub fn mode_string(&self) -> String {
        canonical_mode(self.mode).to_owned()
    }
}

pub(crate) fn canonical_mode(mode: u32) -> &'static str {
    match mode & 0o170000 {
        0o040000 => "040000",
        0o120000 => "120000",
        0o160000 => "160000",
        _ if mode & 0o100 != 0 => "100755",
        _ => "100644",
    }
}

fn parse_tree(data: &[u8]) -> Result<Vec<TreeEntry>> {
    let mut entries = Vec::new();
    let mut position = 0;
    while position < data.len() {
        let space = data[position..]
            .iter()
            .position(|byte| *byte == b' ')
            .context("malformed tree")?
            + position;
        let mode = u32::from_str_radix(std::str::from_utf8(&data[position..space])?, 8)?;
        let nul = data[space + 1..]
            .iter()
            .position(|byte| *byte == 0)
            .context("malformed tree")?
            + space
            + 1;
        let name = data[space + 1..nul].to_vec();
        let raw = data.get(nul + 1..nul + 21).context("truncated tree")?;
        entries.push(TreeEntry {
            mode,
            name,
            oid: gix_hash::ObjectId::from_bytes_or_panic(raw).to_string(),
        });
        position = nul + 21;
    }
    Ok(entries)
}

/// Git's tree order: names compare as if directories had a trailing `/`.
pub(crate) fn tree_order(a: &TreeEntry, b: &TreeEntry) -> std::cmp::Ordering {
    let key = |entry: &TreeEntry| {
        let mut name = entry.name.clone();
        if entry.is_tree() {
            name.push(b'/');
        }
        name
    };
    key(a).cmp(&key(b))
}

/// Serializes tree entries in canonical order and mode spelling.
pub(crate) fn encode_tree(entries: &mut [TreeEntry]) -> Result<Vec<u8>> {
    entries.sort_by(tree_order);
    let mut out = Vec::new();
    for entry in entries.iter() {
        let mode = match canonical_mode(entry.mode) {
            "040000" => "40000",
            other => other,
        };
        out.extend_from_slice(mode.as_bytes());
        out.push(b' ');
        out.extend_from_slice(&entry.name);
        out.push(0);
        out.extend_from_slice(parse_oid(&entry.oid)?.as_bytes());
    }
    Ok(out)
}

/// Git's `check_refname_format` (flags: one-level names allowed only when `onelevel`).
pub(crate) fn check_ref_format(name: &str, onelevel: bool) -> bool {
    let bytes = name.as_bytes();
    if bytes == b"@" {
        return false;
    }
    let mut components = 0;
    let mut rest = bytes;
    loop {
        let mut last = 0u8;
        let mut length = 0;
        for &byte in rest {
            match byte {
                b'/' => break,
                0..=0x1f | 0x7f | b' ' | b':' | b'?' | b'[' | b'\\' | b'^' | b'~' | b'*' => {
                    return false;
                }
                b'.' if last == b'.' => return false,
                b'{' if last == b'@' => return false,
                _ => {}
            }
            last = byte;
            length += 1;
        }
        let component = &rest[..length];
        if component.is_empty() || component[0] == b'.' || component.ends_with(b".lock") {
            return false;
        }
        components += 1;
        if length == rest.len() {
            if component.ends_with(b".") {
                return false;
            }
            break;
        }
        rest = &rest[length + 1..];
    }
    onelevel || components >= 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ref_name_rules_match_git_check_ref_format() {
        let names = [
            "refs/heads/main",
            "refs/heads/feature/x",
            "refs/heads/a..b",
            "refs/heads/.hidden",
            "refs/heads/x.lock",
            "refs/heads/x.",
            "refs/heads/a@{b",
            "refs/heads/a@b",
            "refs/heads/sp ace",
            "refs/heads/tab\t",
            "refs/heads/star*",
            "refs/heads/q?",
            "refs/heads/br[",
            "refs/heads/back\\slash",
            "refs/heads/caret^",
            "refs/heads/tilde~",
            "refs/heads/colon:",
            "refs/heads//double",
            "refs/heads/trailing/",
            "refs/heads/-dash",
            "refs/heads/ünï",
            "refs/heads/@",
            "refs/heads/a.lock/b",
            "refs/heads/x.y",
            "refs/heads/{brace}",
            "@",
            "main",
        ];
        for name in names {
            let output = std::process::Command::new("git")
                .args(["check-ref-format", name])
                .output()
                .unwrap();
            assert_eq!(
                check_ref_format(name, false),
                output.status.success(),
                "{name:?}"
            );
        }
    }

    #[test]
    fn index_and_tree_encodings_round_trip() {
        let blob = hash_object(Kind::Blob, b"hello\n").unwrap();
        assert_eq!(blob, "ce013625030ba8dba906f756967f9e9ca394464a");
        let mut entries = vec![
            TreeEntry {
                mode: 0o100644,
                name: b"b".to_vec(),
                oid: blob.clone(),
            },
            TreeEntry {
                mode: 0o040000,
                name: b"a".to_vec(),
                oid: "4b825dc642cb6eb9a060e54bf8d69288fbee4904".into(),
            },
            TreeEntry {
                mode: 0o100664,
                name: b"a.txt".to_vec(),
                oid: blob.clone(),
            },
        ];
        let bytes = encode_tree(&mut entries).unwrap();
        let names: Vec<&[u8]> = entries.iter().map(|entry| entry.name.as_slice()).collect();
        assert_eq!(names, [&b"a.txt"[..], b"a", b"b"], "a.txt sorts before a/");
        let parsed = parse_tree(&bytes).unwrap();
        assert_eq!(
            parsed[0].mode_string(),
            "100644",
            "non-canonical modes normalized"
        );
        assert_eq!(parsed[1].mode_string(), "040000");
        let directory = tempfile::tempdir().unwrap();
        let index = write_index(&[("x/y".into(), "100755".into(), blob.clone())]).unwrap();
        std::fs::write(directory.path().join("index"), index).unwrap();
        let read = read_index(directory.path()).unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].path, "x/y");
        assert_eq!(read[0].mode, "100755");
        assert_eq!(read[0].oid, blob);
    }
}
