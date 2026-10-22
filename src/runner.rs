//! Process-wide state of the Git runner: which `git` executable runs, how many processes ran, and
//! the per-process caches of configuration and identity that would otherwise cost a `git config`
//! or `git var` process per question.
//!
//! Caches are invalidated by a generation counter that every Git subprocess which may change the
//! configuration (or the object/reference layout) bumps; see [`note`].
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

// ---------------------------------------------------------------------------------------------
// The Git executable
// ---------------------------------------------------------------------------------------------

/// The Git executable every subprocess runs, resolved once per process (see [`resolve`]).
pub(crate) fn program() -> &'static OsStr {
    static PROGRAM: OnceLock<OsString> = OnceLock::new();
    PROGRAM.get_or_init(|| resolve(&|name| std::env::var_os(name)))
}

fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

/// `AISC_GIT` (or `GIT`, when it names an executable by absolute path) wins. Otherwise the first
/// `git` on `PATH` is used, exactly the one `execvp` would pick, so a wrapper placed on `PATH`
/// still sees every call. The only exception is the macOS `/usr/bin/git` shim, which spends
/// about 5 ms per call in `xcrun` before running the developer tools' Git: that Git is run
/// directly. A `PATH` with relative entries keeps the plain name, whose lookup depends on the
/// child's working directory.
pub(crate) fn resolve(env: &dyn Fn(&str) -> Option<OsString>) -> OsString {
    if let Some(explicit) = env("AISC_GIT").filter(|value| !value.is_empty()) {
        return explicit;
    }
    if let Some(explicit) = env("GIT").filter(|value| {
        let path = Path::new(value);
        path.is_absolute() && executable(path)
    }) {
        return explicit;
    }
    let Some(path) = env("PATH") else {
        return OsString::from("git");
    };
    let mut found = None;
    for directory in std::env::split_paths(&path) {
        if directory.as_os_str().is_empty() || directory.is_relative() {
            return OsString::from("git");
        }
        let candidate = directory.join("git");
        if executable(&candidate) {
            found = Some(candidate);
            break;
        }
    }
    let Some(found) = found else {
        return OsString::from("git");
    };
    if cfg!(target_os = "macos")
        && is_xcrun_shim(&found)
        && let Some(real) = developer_git(env)
    {
        return real.into_os_string();
    }
    found.into_os_string()
}

fn is_xcrun_shim(path: &Path) -> bool {
    std::fs::canonicalize(path).is_ok_and(|real| real == Path::new("/usr/bin/git"))
}

/// The Git the `/usr/bin/git` shim forwards to: `$DEVELOPER_DIR` or the `xcode-select` choice,
/// else whatever the shim reports as its exec path.
fn developer_git(env: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let mut roots = Vec::new();
    if let Some(directory) = env("DEVELOPER_DIR").filter(|value| !value.is_empty()) {
        let directory = PathBuf::from(directory);
        roots.push(directory.join("Contents/Developer"));
        roots.push(directory);
    } else if let Ok(selected) = std::fs::read_link("/var/db/xcode_select_link") {
        roots.push(selected);
    }
    for root in roots {
        let candidate = root.join("usr/bin/git");
        if executable(&candidate) && !is_xcrun_shim(&candidate) {
            return Some(candidate);
        }
    }
    let output = std::process::Command::new("/usr/bin/git")
        .arg("--exec-path")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let exec_path = String::from_utf8(output.stdout).ok()?;
    let candidate = std::fs::canonicalize(Path::new(exec_path.trim()).join("git")).ok()?;
    (output.status.success() && executable(&candidate) && !is_xcrun_shim(&candidate))
        .then_some(candidate)
}

// ---------------------------------------------------------------------------------------------
// Process accounting and invalidation
// ---------------------------------------------------------------------------------------------

static SPAWNS: AtomicUsize = AtomicUsize::new(0);
static EPOCH: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static SPAWNED_HERE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Per repository root: (configuration changes, storage changes).
fn changes() -> &'static Mutex<HashMap<PathBuf, (u64, u64)>> {
    static CHANGES: OnceLock<Mutex<HashMap<PathBuf, (u64, u64)>>> = OnceLock::new();
    CHANGES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Git processes started by this process so far.
#[allow(dead_code)] // read by debugging aids
pub(crate) fn spawns() -> usize {
    SPAWNS.load(Ordering::SeqCst)
}

/// Git processes started by the calling thread so far.
#[allow(dead_code)] // read by tests
pub(crate) fn spawns_here() -> usize {
    SPAWNED_HERE.with(std::cell::Cell::get)
}

/// Changes whenever a Git command ran in `root` that may have changed configuration, the
/// reference layout (packing, replace refs) or object storage (new packs), or after
/// [`invalidate`]. Caches compare it before use.
pub(crate) fn generation(root: &Path) -> u64 {
    let (config, storage) = changes()
        .lock()
        .expect("generations")
        .get(root)
        .copied()
        .unwrap_or_default();
    EPOCH.load(Ordering::SeqCst) + config + storage
}

/// Like [`generation`], for configuration only (a fetch or gc keeps the configuration).
pub(crate) fn config_generation(root: &Path) -> u64 {
    let (config, _) = changes()
        .lock()
        .expect("generations")
        .get(root)
        .copied()
        .unwrap_or_default();
    EPOCH.load(Ordering::SeqCst) + config
}

/// Marks every cache of every repository stale, as a write through another channel would.
#[allow(dead_code)] // for callers that change a repository without the runner
pub(crate) fn invalidate() {
    EPOCH.fetch_add(1, Ordering::SeqCst);
}

/// The subcommand of a Git argument list, skipping global options.
pub(crate) fn subcommand<'a>(args: &[&'a str]) -> &'a str {
    let mut position = 0;
    while position < args.len() {
        match args[position] {
            "-c" | "-C" | "--git-dir" | "--work-tree" | "--namespace" | "--config-env" => {
                position += 2;
            }
            argument if argument.starts_with('-') => position += 1,
            argument => return argument,
        }
    }
    ""
}

/// Commands that can never change configuration, pack layout or replace refs. Everything else
/// (fetch, gc, config writes, remote, unknown commands) invalidates the caches.
fn harmless(args: &[&str]) -> bool {
    let name = subcommand(args);
    match name {
        "rev-parse" | "status" | "diff" | "diff-tree" | "diff-index" | "diff-files" | "log"
        | "rev-list" | "cat-file" | "ls-files" | "ls-tree" | "for-each-ref" | "var"
        | "merge-base" | "show" | "check-ref-format" | "check-ignore" | "worktree"
        | "symbolic-ref" | "update-ref" | "hash-object" | "write-tree" | "read-tree"
        | "update-index" | "merge-tree" | "merge-file" | "commit-tree" | "mktree" | "add"
        | "--version" | "version" => {
            // `worktree` subcommands change the worktree registry only, which is never cached.
            !args
                .iter()
                .any(|argument| argument.starts_with("refs/replace"))
        }
        "config" => args.iter().any(|argument| {
            matches!(
                *argument,
                "--get" | "--get-all" | "--get-regexp" | "-l" | "--list" | "--get-urlmatch"
            )
        }),
        _ => false,
    }
}

/// Called before every Git subprocess this program starts in `root`.
pub(crate) fn note(root: &Path, args: &[&str]) {
    SPAWNS.fetch_add(1, Ordering::SeqCst);
    SPAWNED_HERE.with(|count| count.set(count.get() + 1));
    if !harmless(args) {
        let mut changes = changes().lock().expect("generations");
        let entry = changes.entry(root.to_owned()).or_default();
        entry.1 += 1;
        // These rewrite objects and references, never the configuration.
        if !matches!(
            subcommand(args),
            "fetch" | "gc" | "repack" | "prune" | "pack-refs" | "replace"
        ) {
            entry.0 += 1;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Configuration (`git config -l -z`, once per process)
// ---------------------------------------------------------------------------------------------

/// Every configuration entry Git sees in this repository, in Git's order (later entries win).
#[derive(Debug, Default)]
pub(crate) struct Config {
    entries: Vec<(String, Option<String>)>,
}

impl Config {
    pub fn parse(bytes: &[u8]) -> Self {
        let entries = bytes
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
            .map(|record| {
                let record = String::from_utf8_lossy(record);
                match record.split_once('\n') {
                    Some((key, value)) => (key.to_owned(), Some(value.to_owned())),
                    None => (record.into_owned(), None),
                }
            })
            .collect();
        Self { entries }
    }

    /// The last value of `key` (section and name in lower case): `Some(None)` for a key without
    /// `=` (implicit true), `None` when unset.
    pub fn get(&self, key: &str) -> Option<Option<&str>> {
        self.entries
            .iter()
            .rev()
            .find(|(name, _)| same_key(name, key))
            .map(|(_, value)| value.as_deref())
    }

    /// Every key, in order (with repeats).
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(name, _)| name.as_str())
    }

    pub fn string(&self, key: &str) -> Option<&str> {
        self.get(key).flatten()
    }

    /// `git config --bool --get KEY` printing `true`; unset and invalid values count as false.
    pub fn is_true(&self, key: &str) -> bool {
        match self.get(key) {
            None => false,
            Some(None) => true,
            Some(Some(value)) => parse_bool(value).unwrap_or(false),
        }
    }
}

/// Section and variable names compare without case, a subsection (`branch.<name>.remote`) exactly.
fn same_key(a: &str, b: &str) -> bool {
    let split = |key: &str| -> (String, String, String) {
        let (section, rest) = key.split_once('.').unwrap_or((key, ""));
        let (middle, name) = rest.rsplit_once('.').unwrap_or(("", rest));
        (
            section.to_ascii_lowercase(),
            middle.to_owned(),
            name.to_ascii_lowercase(),
        )
    };
    split(a) == split(b)
}

/// Git's `git_parse_maybe_bool` plus its integer fallback.
pub(crate) fn parse_bool(value: &str) -> Option<bool> {
    let lower = value.to_ascii_lowercase();
    match lower.as_str() {
        "true" | "yes" | "on" => Some(true),
        "false" | "no" | "off" | "" => Some(false),
        _ => lower.parse::<i64>().ok().map(|number| number != 0),
    }
}

type ConfigCache = Mutex<HashMap<PathBuf, (u64, Arc<Config>)>>;

fn config_cache() -> &'static ConfigCache {
    static CACHE: OnceLock<ConfigCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The repository's configuration, read with one `git config -l -z` per process (and again only
/// after a command that may have changed it).
pub(crate) fn config(root: &Path) -> Result<Arc<Config>> {
    let generation = config_generation(root);
    if let Some((seen, config)) = config_cache().lock().expect("config cache").get(root)
        && *seen == generation
    {
        return Ok(config.clone());
    }
    let output = crate::git::run_git_raw(root, &["config", "-l", "-z"], &[], None, 16 << 20)?;
    // Exit 1 means "no configuration at all".
    if !(output.success() || output.code == Some(1)) || output.stdout_omitted != 0 {
        bail!(
            "git config -l failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let config = Arc::new(Config::parse(&output.stdout));
    config_cache()
        .lock()
        .expect("config cache")
        .insert(root.to_owned(), (generation, config.clone()));
    Ok(config)
}

// ---------------------------------------------------------------------------------------------
// Identity (`git var GIT_AUTHOR_IDENT` / `GIT_COMMITTER_IDENT`)
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Who {
    Author,
    Committer,
}

impl Who {
    fn env(self, field: &str) -> String {
        match self {
            Self::Author => format!("GIT_AUTHOR_{field}"),
            Self::Committer => format!("GIT_COMMITTER_{field}"),
        }
    }
    fn section(self) -> &'static str {
        match self {
            Self::Author => "author",
            Self::Committer => "committer",
        }
    }
}

type IdentCache = Mutex<HashMap<(PathBuf, Who), (u64, String)>>;

fn ident_cache() -> &'static IdentCache {
    static CACHE: OnceLock<IdentCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `git var GIT_<WHO>_IDENT`, byte for byte. Computed in process when Git's answer is fully
/// determined by the environment and configuration (an explicit name and e-mail, and a date that
/// is either absent or in one of the unambiguous formats below); otherwise Git is asked once and
/// the answer is kept for the rest of the process.
pub(crate) fn ident(root: &Path, who: Who) -> Result<String> {
    let generation = config_generation(root);
    let key = (root.to_owned(), who);
    if let Some((seen, value)) = ident_cache().lock().expect("ident cache").get(&key)
        && *seen == generation
    {
        return Ok(value.clone());
    }
    let config = config(root)?;
    let value = match local_ident(&config, who, &|name| std::env::var_os(name), now()) {
        Some(value) => value,
        None => {
            let variable = who.env("IDENT");
            let output = crate::git::run_git(root, &["var", &variable], None)?;
            String::from_utf8(output)
                .context("non-UTF-8 identity")?
                .strip_suffix('\n')
                .context("invalid git var response")?
                .to_owned()
        }
    };
    ident_cache()
        .lock()
        .expect("ident cache")
        .insert(key, (generation, value.clone()));
    Ok(value)
}

/// Seconds since the epoch and the local UTC offset in minutes.
fn now() -> (i64, i32) {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(0));
    (seconds, local_offset(seconds))
}

fn local_offset(seconds: i64) -> i32 {
    let time: libc::time_t = seconds as libc::time_t;
    // SAFETY: `localtime_r` fills the zeroed `tm` we own; both pointers are valid for the call.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let filled = unsafe { libc::localtime_r(&time, &mut tm) };
    if filled.is_null() {
        return 0;
    }
    i32::try_from(tm.tm_gmtoff / 60).unwrap_or(0)
}

/// A name or e-mail Git copies verbatim. Git strips "crud" from both ends and drops `<`, `>` and
/// newlines inside, and the crud set differs between Git versions (newer ones keep `.`), so a
/// value with any such character is left to Git.
fn verbatim(value: &str) -> Option<&str> {
    let edge = |byte: u8| {
        byte <= 32
            || matches!(
                byte,
                b'.' | b',' | b':' | b';' | b'<' | b'>' | b'"' | b'\\' | b'\''
            )
    };
    let bytes = value.as_bytes();
    let clean = bytes.first().is_none_or(|byte| !edge(*byte))
        && bytes.last().is_none_or(|byte| !edge(*byte))
        && !bytes.iter().any(|byte| matches!(byte, b'\n' | b'<' | b'>'));
    clean.then_some(value)
}

/// The ident Git would print, when it can be derived without Git; `None` means "ask Git".
pub(crate) fn local_ident(
    config: &Config,
    who: Who,
    env: &dyn Fn(&str) -> Option<OsString>,
    now: (i64, i32),
) -> Option<String> {
    let variable = |name: &str| -> Option<String> {
        env(name)
            .map(|value| value.into_string().ok())
            .unwrap_or(None)
    };
    if env(&who.env("NAME")).is_some_and(|value| value.to_str().is_none())
        || env(&who.env("EMAIL")).is_some_and(|value| value.to_str().is_none())
    {
        return None;
    }
    // Name: environment, then author.name/committer.name, then user.name.
    let name = variable(&who.env("NAME"))
        .or_else(|| {
            config
                .string(&format!("{}.name", who.section()))
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        })
        .or_else(|| config.string("user.name").map(str::to_owned))?;
    if config.get("user.name") == Some(None)
        || config.get(&format!("{}.name", who.section())) == Some(None)
        || config.get("user.email") == Some(None)
        || config.get(&format!("{}.email", who.section())) == Some(None)
    {
        return None; // a valueless key is a configuration error Git reports
    }
    // E-mail: environment (even empty), then author.email/committer.email, user.email, $EMAIL.
    let email = match variable(&who.env("EMAIL")) {
        Some(email) => email,
        None => config
            .string(&format!("{}.email", who.section()))
            .filter(|value| !value.is_empty())
            .or_else(|| config.string("user.email"))
            .map(str::to_owned)
            .or_else(|| {
                // $EMAIL does not count as configured for user.useConfigOnly.
                if config.is_true("user.useconfigonly") {
                    return None;
                }
                variable("EMAIL").filter(|value| !value.is_empty())
            })?,
    };
    if name.is_empty() {
        return None; // Git refuses an empty name
    }
    let name = verbatim(&name)?;
    let email = verbatim(&email)?;
    let date = match variable(&who.env("DATE")).filter(|value| !value.is_empty()) {
        Some(value) => parse_date(&value)?,
        None => (now.0, now.1),
    };
    let (sign, offset) = if date.1 < 0 {
        ('-', -date.1)
    } else {
        ('+', date.1)
    };
    Some(format!(
        "{name} <{email}> {} {sign}{:02}{:02}",
        date.0,
        offset / 60,
        offset % 60
    ))
}

/// The subset of Git's date syntax whose meaning is unambiguous: `@SECONDS ±HHMM`,
/// `SECONDS ±HHMM` (more than eight digits) and ISO 8601 `YYYY-MM-DD[T ]HH:MM:SS` followed by `Z`,
/// `±HH:MM` or `[ ]±HHMM`. Anything else is left to Git.
pub(crate) fn parse_date(value: &str) -> Option<(i64, i32)> {
    fn offset(text: &str) -> Option<i32> {
        let (sign, rest) = match text.as_bytes().first()? {
            b'+' => (1, &text[1..]),
            b'-' => (-1, &text[1..]),
            _ => return None,
        };
        let digits: String = rest.chars().filter(|c| *c != ':').collect();
        if digits.len() != 4
            || !digits.bytes().all(|byte| byte.is_ascii_digit())
            || (rest.contains(':') && rest.len() != 5)
        {
            return None;
        }
        let hours: i32 = digits[..2].parse().ok()?;
        let minutes: i32 = digits[2..].parse().ok()?;
        if hours > 14 || minutes > 59 {
            return None;
        }
        Some(sign * (hours * 60 + minutes))
    }
    let raw = value.strip_prefix('@').unwrap_or(value);
    if let Some((seconds, zone)) = raw.split_once(' ')
        && !seconds.is_empty()
        && seconds.bytes().all(|byte| byte.is_ascii_digit())
        && (value.starts_with('@') || seconds.len() > 8)
        && !zone.contains(':')
    {
        let seconds: i64 = seconds.parse().ok()?;
        return Some((seconds, offset(zone)?));
    }
    // ISO 8601.
    let bytes = value.as_bytes();
    if bytes.len() < 20 || !matches!(bytes[10], b'T' | b' ') {
        return None;
    }
    let number = |range: std::ops::Range<usize>| -> Option<i64> {
        let part = value.get(range)?;
        part.bytes()
            .all(|byte| byte.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    if bytes[4] != b'-' || bytes[7] != b'-' || bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1970..=2099).contains(&year)
        || !(1..=12).contains(&month)
        || day < 1
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let zone = &value[19..];
    let zone = if zone == "Z" {
        0
    } else {
        offset(zone.strip_prefix(' ').unwrap_or(zone))?
    };
    let days = days_from_civil(year, month, day);
    let local = days * 86_400 + hour * 3_600 + minute * 60 + second;
    Some((local - i64::from(zone) * 60, zone))
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn fake_env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let map: HashMap<String, OsString> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), OsString::from(value)))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn explicit_override_wins_and_path_order_is_kept() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first");
        let second = directory.path().join("second");
        let plain = directory.path().join("plain");
        for dir in [&first, &second, &plain] {
            std::fs::create_dir_all(dir).unwrap();
        }
        use std::os::unix::fs::PermissionsExt;
        for dir in [&first, &second] {
            std::fs::write(dir.join("git"), "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(dir.join("git"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        // Not executable: skipped, as execvp would.
        std::fs::write(plain.join("git"), "").unwrap();
        let path = std::env::join_paths([&plain, &first, &second]).unwrap();
        let path = path.to_str().unwrap();
        assert_eq!(
            resolve(&fake_env(&[("PATH", path)])),
            first.join("git").into_os_string(),
            "the first executable git on PATH, unchanged (wrappers keep working)"
        );
        assert_eq!(
            resolve(&fake_env(&[("PATH", path), ("AISC_GIT", "/opt/x/git")])),
            OsString::from("/opt/x/git")
        );
        let explicit = second.join("git");
        assert_eq!(
            resolve(&fake_env(&[
                ("PATH", path),
                ("GIT", explicit.to_str().unwrap())
            ])),
            explicit.into_os_string()
        );
        assert_eq!(
            resolve(&fake_env(&[("PATH", path), ("GIT", "git")])),
            first.join("git").into_os_string(),
            "GIT must be an absolute executable path"
        );
        assert_eq!(
            resolve(&fake_env(&[("PATH", "relative/bin:/usr/bin")])),
            OsString::from("git"),
            "relative PATH entries depend on the child's cwd: keep execvp's lookup"
        );
        assert_eq!(resolve(&fake_env(&[])), OsString::from("git"));
    }

    #[test]
    fn resolved_git_is_the_same_git_as_the_path_lookup() {
        let resolved = program();
        let version = |program: &OsStr| {
            let output = Command::new(program).arg("--version").output().unwrap();
            assert!(output.status.success());
            String::from_utf8(output.stdout).unwrap()
        };
        assert_eq!(version(resolved), version(OsStr::new("git")));
        if cfg!(target_os = "macos") && std::env::var_os("AISC_GIT").is_none() {
            let first = resolve(&|name| std::env::var_os(name));
            if is_xcrun_shim(Path::new(&first)) {
                panic!("the xcrun shim should have been bypassed");
            }
        }
    }

    #[test]
    fn config_cache_is_reused_until_a_write_invalidates_it() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let git = |args: &[&str]| {
            crate::git::run_git(root, args, None).unwrap();
        };
        git(&["init", "-q"]);
        git(&["config", "aisc.test", "one"]);
        assert_eq!(config(root).unwrap().string("aisc.test"), Some("one"));
        let before = spawns_here();
        assert_eq!(config(root).unwrap().string("aisc.test"), Some("one"));
        assert_eq!(spawns_here(), before, "a cached read starts no process");
        // A configuration write through the runner invalidates the cache.
        git(&["config", "aisc.test", "two"]);
        assert_eq!(config(root).unwrap().string("aisc.test"), Some("two"));
        // A harmless command keeps it.
        git(&["rev-parse", "--git-dir"]);
        let before = spawns_here();
        assert_eq!(config(root).unwrap().string("aisc.test"), Some("two"));
        assert_eq!(spawns_here(), before);
        let parsed = Config::parse(b"core.bare\nfalse\0user.flag\0user.name\nA\nB\0");
        assert_eq!(parsed.get("user.flag"), Some(None));
        assert!(parsed.is_true("user.flag"));
        assert!(!parsed.is_true("core.bare"));
        assert_eq!(parsed.string("USER.NAME"), Some("A\nB"));
    }

    #[test]
    fn local_identity_matches_git_var_byte_for_byte() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let status = Command::new("git")
            .args(["init", "-q"])
            .current_dir(root)
            .status()
            .unwrap();
        assert!(status.success());
        let names = [
            "Plain Name",
            "  Spaced  Name. ",
            "Dr. J. \"Quote\" O'Neil,",
            "<Angle> Brackets",
            "Ünïcödé Nämé",
            "trailing\\",
        ];
        let emails = ["a@example.invalid", " <weird@x>. ", ""];
        let dates = [
            "2026-01-01T00:00:00Z",
            "2026-02-28 13:14:15 +0530",
            "2024-02-29T23:59:59-07:00",
            "@1700000000 -0130",
            "1700000000 +0000",
        ];
        let mut cases = Vec::new();
        for (index, name) in names.iter().enumerate() {
            cases.push((index, *name, emails[index % emails.len()], dates[0]));
        }
        for date in dates {
            cases.push((0, "Plain Name", "a@example.invalid", date));
            cases.push((0, "Ünïcödé Nämé", "u@example.invalid", date));
        }
        for (index, name, email, date) in cases {
            for who in [Who::Author, Who::Committer] {
                let prefix = match who {
                    Who::Author => "GIT_AUTHOR",
                    Who::Committer => "GIT_COMMITTER",
                };
                let output = Command::new("git")
                    .args(["var", &format!("{prefix}_IDENT")])
                    .current_dir(root)
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .env("GIT_CONFIG_GLOBAL", "/dev/null")
                    .env(format!("{prefix}_NAME"), name)
                    .env(format!("{prefix}_EMAIL"), email)
                    .env(format!("{prefix}_DATE"), date)
                    .output()
                    .unwrap();
                let expected = String::from_utf8(output.stdout).unwrap();
                let env = fake_env(&[
                    (&format!("{prefix}_NAME"), name),
                    (&format!("{prefix}_EMAIL"), email),
                    (&format!("{prefix}_DATE"), date),
                ]);
                let ours = local_ident(&Config::default(), who, &env, (0, 0));
                // In process only when certain; otherwise Git answers (and may refuse).
                if let Some(ours) = &ours {
                    assert!(output.status.success(), "{name:?}: Git refuses, so must we");
                    assert_eq!(ours, expected.trim_end(), "{name:?} {date}");
                }
                if index == 0 {
                    assert!(
                        ours.is_some(),
                        "plain identities need no Git: {name:?} {date}"
                    );
                }
            }
        }
        // Configuration supplies the name and e-mail; author.* beats user.*.
        let config =
            Config::parse(b"user.name\nUser\0user.email\nu@example.invalid\0author.name\nAuthor\0");
        let env = fake_env(&[("GIT_AUTHOR_DATE", "@5 +0100")]);
        assert_eq!(
            local_ident(&config, Who::Author, &env, (0, 0)).as_deref(),
            Some("Author <u@example.invalid> 5 +0100")
        );
        assert_eq!(
            local_ident(&config, Who::Committer, &env, (77, -90)).as_deref(),
            Some("User <u@example.invalid> 77 -0130")
        );
        // No e-mail anywhere: Git auto-detects (or refuses), so we ask Git.
        let config = Config::parse(b"user.name\nUser\0");
        assert_eq!(
            local_ident(&config, Who::Author, &fake_env(&[]), (0, 0)),
            None
        );
        // Dates outside the unambiguous subset are left to Git.
        for date in [
            "yesterday",
            "2026-01-01",
            "1700000000",
            "2026-13-01T00:00:00Z",
        ] {
            assert_eq!(parse_date(date), None, "{date}");
        }
    }
}
