//! Git plumbing wrappers: blobs, trees, commits, merges and reference transactions.
use super::{Code, fail, fail_hint};
use crate::git::{Repo, run_git_raw, text};
use crate::output::escape_block;
use crate::process::Output;
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

const LIMIT: usize = 64 * 1_024 * 1_024;

pub(crate) type Entry = (String, String); // (mode, oid)

#[derive(Clone)]
pub(crate) struct Git {
    pub root: PathBuf,
    pub dir: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CommitMeta {
    pub author_name: String,
    pub author_email: String,
    /// `@<unix> <tz>` as accepted by GIT_AUTHOR_DATE.
    pub author_date: String,
    pub message: Vec<u8>,
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

    fn failure(args: &[&str], output: &Output) -> anyhow::Error {
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

    pub fn head_state(&self) -> Result<HeadState> {
        let oid = self
            .probe(&["rev-parse", "--verify", "--quiet", "HEAD^{commit}"])?
            .ok_or_else(|| {
                fail_hint(
                    Code::State,
                    "the repository has no commits yet",
                    "create an initial commit with git first",
                )
            })?;
        let symbolic = self.probe(&["symbolic-ref", "-q", "HEAD"])?;
        Ok(HeadState { symbolic, oid })
    }

    pub fn common_dir(&self) -> Result<PathBuf> {
        Ok(PathBuf::from(self.line(&[
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
        ])?))
    }

    pub fn empty_tree(&self) -> Result<String> {
        let output = self.run_with(
            &["hash-object", "-t", "tree", "-w", "--stdin"],
            &[],
            Some(Vec::new()),
        )?;
        Ok(text(&output)?.trim().to_owned())
    }

    pub fn zero_oid(&self, like: &str) -> String {
        "0".repeat(like.len())
    }

    pub fn resolve_commit(&self, spec: &str) -> Result<Option<String>> {
        if spec.is_empty() || spec.starts_with('-') {
            return Ok(None);
        }
        let argument = format!("{spec}^{{commit}}");
        self.probe(&["rev-parse", "--verify", "--quiet", &argument])
    }

    pub fn ref_oid(&self, name: &str) -> Result<Option<String>> {
        self.probe(&["rev-parse", "--verify", "--quiet", name])
    }

    pub fn commit_tree_of(&self, commit: &str) -> Result<String> {
        let argument = format!("{commit}^{{tree}}");
        self.line(&["rev-parse", "--verify", &argument])
    }

    /// Branches (`refs/heads/..`) checked out in any worktree of this repository.
    pub fn checked_out(&self) -> Result<Vec<String>> {
        let output = self.run(&["worktree", "list", "--porcelain"])?;
        Ok(text(&output)?
            .lines()
            .filter_map(|line| line.strip_prefix("branch "))
            .map(str::to_owned)
            .collect())
    }

    pub fn valid_branch_name(&self, name: &str) -> Result<()> {
        if name.is_empty() || name.starts_with('-') {
            return Err(fail(Code::Usage, format!("invalid branch name {name:?}")));
        }
        let reference = format!("refs/heads/{name}");
        match self.probe(&["check-ref-format", &reference])? {
            Some(_) => Ok(()),
            None => Err(fail(Code::Usage, format!("invalid branch name {name:?}"))),
        }
    }

    pub fn read_blobs(&self, oids: &[String]) -> Result<Vec<Vec<u8>>> {
        if oids.is_empty() {
            return Ok(Vec::new());
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
            } else if path.contains(['\n', '\r']) {
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

    pub fn ls_tree(&self, tree: &str, paths: &[String]) -> Result<BTreeMap<String, Entry>> {
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

    /// Builds `base_tree` plus `edits` in a throw-away index; the real index is never touched.
    pub fn tree_with(&self, base_tree: &str, edits: &[TreeEdit]) -> Result<String> {
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
        let raw = self.run(&["cat-file", "commit", commit])?;
        let split = raw
            .windows(2)
            .position(|pair| pair == b"\n\n")
            .context("commit without message separator")?;
        let header = text(&raw[..split])?;
        let mut author = None;
        for line in header.lines() {
            if line.starts_with("encoding ") {
                return Err(fail(
                    Code::Unsupported,
                    format!("commit {commit} declares a non-default encoding"),
                ));
            }
            if let Some(value) = line.strip_prefix("author ") {
                author = Some(value);
            }
        }
        let author = author.context("commit without author")?;
        let open = author.rfind('<').context("malformed author line")?;
        let close = author.rfind('>').context("malformed author line")?;
        let name = author[..open].trim_end().to_owned();
        let email = author[open + 1..close].to_owned();
        let date = author[close + 1..].trim();
        Ok(CommitMeta {
            author_name: name,
            author_email: email,
            author_date: format!("@{date}"),
            message: raw[split + 2..].to_vec(),
        })
    }

    pub fn commit_tree(
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
        let signing = self
            .probe(&["config", "--bool", "--get", "commit.gpgsign"])?
            .is_some_and(|value| value == "true");
        if signing {
            args.push("-S");
        }
        args.extend(["-F", "-"]);
        let mut body = message.to_vec();
        if !body.ends_with(b"\n") {
            body.push(b'\n');
        }
        let name_value;
        let email_value;
        let date_value;
        let mut env: Vec<(&str, &OsStr)> = Vec::new();
        if let Some(meta) = meta {
            name_value = meta.author_name.clone();
            email_value = meta.author_email.clone();
            date_value = meta.author_date.clone();
            env.push(("GIT_AUTHOR_NAME", OsStr::new(&name_value)));
            env.push(("GIT_AUTHOR_EMAIL", OsStr::new(&email_value)));
            env.push(("GIT_AUTHOR_DATE", OsStr::new(&date_value)));
        }
        let output = self.run_with(&args, &env, Some(body))?;
        Ok(text(&output)?.trim().to_owned())
    }

    pub fn merge_trees(&self, base: &str, ours: &str, theirs: &str) -> Result<MergeResult> {
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
        let output = std::process::Command::new("git").arg("--version").output();
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
        assert_eq!(
            git.read_commit_meta(&again).unwrap().author_date,
            meta.author_date
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
}
