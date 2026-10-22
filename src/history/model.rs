//! Data model shared by diff parsing, selectors and the verbs.
use std::ops::Range;

/// A text line including its trailing newline (absent only on a final line without one).
pub(crate) type Line = Vec<u8>;

/// A run of removed/added lines with up to 3 lines of context on each side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Block {
    /// Index in the old file of the first removed line (or the insertion point).
    pub old_pos: usize,
    /// Index in the new file of the first added line (or the point of deletion).
    pub new_pos: usize,
    pub removed: Vec<Line>,
    pub added: Vec<Line>,
    pub before: Vec<Line>,
    pub after: Vec<Line>,
    pub at_start: bool,
    pub at_eof: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Modified,
    Untracked,
    Added,
    Deleted,
    Binary,
    ModeOnly,
    TypeChange,
    Symlink,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Modified => "modified",
            Self::Untracked => "untracked",
            Self::Added => "new",
            Self::Deleted => "deleted",
            Self::Binary => "binary",
            Self::ModeOnly => "mode",
            Self::TypeChange => "typechange",
            Self::Symlink => "symlink",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct HunkInfo {
    pub blocks: Range<usize>,
    /// Full 64-letter id (letters g-v).
    pub id: String,
    /// Shortest unique prefix, at least 3 letters.
    pub short: String,
    pub line: usize,
    pub added: usize,
    pub removed: usize,
    pub hint: String,
    pub ws: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct FileChange {
    pub path: String,
    pub kind: Kind,
    pub old_mode: Option<String>,
    pub new_mode: Option<String>,
    pub old_oid: Option<String>,
    pub new_oid: Option<String>,
    pub blocks: Vec<Block>,
    pub hunks: Vec<HunkInfo>,
    /// Line count for untracked and whole-file text items.
    pub lines: usize,
    pub binary: bool,
    /// Content digest for untracked files (feeds the hunk id).
    pub digest: String,
    pub ws: bool,
}

impl FileChange {
    /// Whole-file items are selected or left whole; they have one pseudo-hunk.
    pub fn whole(&self) -> bool {
        self.kind != Kind::Modified
    }

    pub fn status(&self) -> &'static str {
        match self.kind {
            Kind::Modified | Kind::ModeOnly => "M",
            Kind::Untracked => "?",
            Kind::Added => "A",
            Kind::Deleted => "D",
            Kind::Binary => "B",
            Kind::TypeChange | Kind::Symlink => "T",
        }
    }

    pub fn mode_changed(&self) -> bool {
        self.old_mode.is_some() && self.new_mode.is_some() && self.old_mode != self.new_mode
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ChangeSet {
    pub files: Vec<FileChange>,
    /// Untracked directories that hold their own repository; they cannot be selected.
    pub nested: Vec<String>,
}

impl ChangeSet {
    pub fn index_of(&self, path: &str) -> Option<usize> {
        self.files
            .binary_search_by(|file| file.path.as_str().cmp(path))
            .ok()
    }
}
