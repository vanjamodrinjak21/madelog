//! A thin git2 wrapper: HEAD, commit metadata, per-file stats and short diff
//! excerpts — with the privacy rules from spec §7 applied at the source.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use git2::{Delta, Diff, DiffFindOptions, DiffOptions, Oid, Patch, Repository};
use regex::Regex;

use crate::event::{CommitFile, FileStatus, MAX_DIFF_EXCERPT_LINES};

pub struct Repo {
    inner: Repository,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitMeta {
    pub sha: String,
    pub message: String,
    /// `Name <email>`
    pub author: String,
    pub parents: Vec<String>,
    /// Commit time, seconds since the epoch.
    pub time: i64,
}

impl Repo {
    /// Open the repository containing `from`, searching upward.
    pub fn discover(from: &Path) -> Result<Repo> {
        let inner = Repository::discover(from).map_err(|e| {
            anyhow!(
                "not inside a git repository ({}): run `git init` first",
                e.message()
            )
        })?;
        Ok(Repo { inner })
    }

    pub fn inner(&self) -> &Repository {
        &self.inner
    }

    /// The `.git` directory.
    pub fn git_dir(&self) -> PathBuf {
        self.inner.path().to_path_buf()
    }

    /// The working tree root. Bare repositories have none.
    pub fn workdir(&self) -> Result<PathBuf> {
        self.inner
            .workdir()
            .map(Path::to_path_buf)
            .ok_or_else(|| anyhow!("this is a bare git repository; madelog needs a working tree"))
    }

    /// Whether the object store holds a commit with this sha at all. A commit
    /// git has never seen, or has pruned, cannot be placed in or out of the
    /// history, so callers treat it as neither.
    pub fn has_commit(&self, sha: &str) -> bool {
        git2::Oid::from_str(sha)
            .ok()
            .and_then(|oid| self.inner.find_commit(oid).ok())
            .is_some()
    }

    /// Whether `sha` is HEAD or one of its ancestors. A commit that a rebase,
    /// an amend or a history purge rewrote away may still sit in the object
    /// store, but it is no longer part of the history the repository tells,
    /// and the page must not count it.
    pub fn reachable_from_head(&self, sha: &str) -> Result<bool> {
        let Some(head) = self.head_sha()? else {
            return Ok(false);
        };
        if head == sha {
            return Ok(true);
        }
        let (Ok(head_oid), Ok(oid)) = (git2::Oid::from_str(&head), git2::Oid::from_str(sha)) else {
            return Ok(false);
        };
        if self.inner.find_commit(oid).is_err() {
            return Ok(false);
        }
        self.inner
            .graph_descendant_of(head_oid, oid)
            .with_context(|| format!("checking whether {sha} is in HEAD's history"))
    }

    /// The sha HEAD points at, or `None` before the first commit.
    pub fn head_sha(&self) -> Result<Option<String>> {
        match self.inner.head() {
            Ok(head) => Ok(Some(head.peel_to_commit()?.id().to_string())),
            // An unborn branch is not an error; the repo simply has no commits.
            Err(e)
                if e.code() == git2::ErrorCode::UnbornBranch
                    || e.code() == git2::ErrorCode::NotFound =>
            {
                Ok(None)
            }
            Err(e) => Err(e).context("reading HEAD"),
        }
    }

    pub fn commit_meta(&self, sha: &str) -> Result<CommitMeta> {
        let commit = self
            .inner
            .find_commit(Oid::from_str(sha).with_context(|| format!("bad sha {sha}"))?)
            .with_context(|| format!("no commit {sha}"))?;
        let author = commit.author();
        let name = author.name().unwrap_or("unknown");
        let email = author.email().unwrap_or("");
        Ok(CommitMeta {
            sha: commit.id().to_string(),
            message: commit.message().unwrap_or("").to_string(),
            author: if email.is_empty() {
                name.to_string()
            } else {
                format!("{name} <{email}>")
            },
            parents: commit.parent_ids().map(|id| id.to_string()).collect(),
            time: commit.time().seconds(),
        })
    }

    /// Per-file changes of `sha` against its first parent.
    ///
    /// Every changed file gets a path, a status and line counts. A diff
    /// excerpt is attached only when the file is text, tracked, not ignored
    /// and not matched by a `privacy.redact` pattern.
    pub fn file_changes(&self, sha: &str, filter: &ExcerptFilter) -> Result<Vec<CommitFile>> {
        let commit = self.inner.find_commit(Oid::from_str(sha)?)?;
        let tree = commit.tree()?;
        let parent_tree = if commit.parent_count() > 0 {
            Some(commit.parent(0)?.tree()?)
        } else {
            None
        };

        let mut opts = DiffOptions::new();
        opts.context_lines(3).id_abbrev(12);
        let mut diff = self
            .inner
            .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut opts))
            .context("diffing against the first parent")?;
        let mut find = DiffFindOptions::new();
        find.renames(true).copies(false);
        diff.find_similar(Some(&mut find)).ok();

        let count = diff.deltas().len();
        let mut files = Vec::with_capacity(count);
        for idx in 0..count {
            if let Some(file) = self.delta_to_file(&diff, idx, filter)? {
                files.push(file);
            }
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(files)
    }

    fn delta_to_file(
        &self,
        diff: &Diff<'_>,
        idx: usize,
        filter: &ExcerptFilter,
    ) -> Result<Option<CommitFile>> {
        let Some(delta) = diff.get_delta(idx) else {
            return Ok(None);
        };
        let path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .map(|p| p.to_string_lossy().replace('\\', "/"));
        let Some(path) = path else {
            return Ok(None);
        };

        let status = match delta.status() {
            Delta::Added | Delta::Copied | Delta::Untracked => FileStatus::Added,
            Delta::Deleted => FileStatus::Deleted,
            Delta::Renamed => FileStatus::Renamed,
            _ => FileStatus::Modified,
        };

        let mut patch = Patch::from_diff(diff, idx).ok().flatten();
        let (added, removed) = match &patch {
            Some(p) => {
                let (_context, adds, dels) = p.line_stats().unwrap_or((0, 0, 0));
                (adds as u64, dels as u64)
            }
            None => (0, 0),
        };

        let binary = patch.is_none() || self.delta_is_binary(&delta);
        let excerpt = if binary || !filter.allows(&path) || self.is_ignored(&path) {
            None
        } else {
            patch.as_mut().and_then(excerpt_from_patch)
        };

        Ok(Some(CommitFile {
            path,
            added,
            removed,
            status,
            diff_excerpt: excerpt,
        }))
    }

    fn delta_is_binary(&self, delta: &git2::DiffDelta<'_>) -> bool {
        if delta.flags().is_binary() {
            return true;
        }
        for file in [delta.new_file(), delta.old_file()] {
            if file.id().is_zero() {
                continue;
            }
            if let Ok(blob) = self.inner.find_blob(file.id()) {
                if blob.is_binary() {
                    return true;
                }
            }
        }
        false
    }

    /// Is this path excluded by `.gitignore` / `.git/info/exclude`?
    pub fn is_ignored(&self, path: &str) -> bool {
        self.inner.is_path_ignored(path).unwrap_or(false)
    }

    /// What `sha` added, and what its files ended up containing.
    ///
    /// Unlike [`Repo::file_changes`], nothing here is truncated or redacted:
    /// it never leaves this process. Attribution compares these lines with
    /// the lines an AI tool wrote and keeps only the counts.
    pub fn commit_diff(&self, sha: &str) -> Result<crate::capture::CommitDiff> {
        let commit = self.inner.find_commit(Oid::from_str(sha)?)?;
        let tree = commit.tree()?;
        let parent_tree = if commit.parent_count() > 0 {
            Some(commit.parent(0)?.tree()?)
        } else {
            None
        };

        let mut opts = DiffOptions::new();
        opts.context_lines(0);
        let diff = self
            .inner
            .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut opts))
            .context("diffing against the first parent")?;

        let mut files: Vec<crate::capture::CommitFileDiff> = Vec::new();
        for idx in 0..diff.deltas().len() {
            let Some(delta) = diff.get_delta(idx) else {
                continue;
            };
            let Some(path) = delta
                .new_file()
                .path()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
            else {
                continue;
            };
            if delta.status() == Delta::Deleted {
                continue;
            }

            let mut added: Vec<String> = Vec::new();
            if let Ok(Some(patch)) = Patch::from_diff(&diff, idx) {
                for h in 0..patch.num_hunks() {
                    let count = patch.num_lines_in_hunk(h).unwrap_or(0);
                    for l in 0..count {
                        if let Ok(line) = patch.line_in_hunk(h, l) {
                            if line.origin() == '+' {
                                if let Ok(text) = std::str::from_utf8(line.content()) {
                                    added.push(strip_newline(text).to_string());
                                }
                            }
                        }
                    }
                }
            }

            let content = self
                .inner
                .find_blob(delta.new_file().id())
                .ok()
                .filter(|blob| !blob.is_binary())
                .and_then(|blob| String::from_utf8(blob.content().to_vec()).ok())
                .map(|text| {
                    text.replace("\r\n", "\n")
                        .lines()
                        .map(str::to_string)
                        .collect::<Vec<String>>()
                })
                .unwrap_or_default();

            files.push(crate::capture::CommitFileDiff {
                path,
                added,
                content,
            });
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(crate::capture::CommitDiff { files })
    }
}

fn strip_newline(text: &str) -> &str {
    text.strip_suffix('\n')
        .map(|t| t.strip_suffix('\r').unwrap_or(t))
        .unwrap_or(text)
}

/// Decides which paths may contribute a diff excerpt.
#[derive(Debug, Default)]
pub struct ExcerptFilter {
    redact: Vec<Regex>,
}

impl ExcerptFilter {
    /// Compile `privacy.redact`. A pattern that does not compile is an error,
    /// so a typo never silently stops redacting.
    pub fn new(patterns: &[String]) -> Result<Self> {
        let mut redact = Vec::with_capacity(patterns.len());
        for p in patterns {
            let re = Regex::new(p)
                .with_context(|| format!("privacy.redact pattern `{p}` is not a valid regex"))?;
            redact.push(re);
        }
        Ok(ExcerptFilter { redact })
    }

    pub fn allows(&self, path: &str) -> bool {
        !self.redact.iter().any(|re| re.is_match(path))
    }
}

/// The first [`MAX_DIFF_EXCERPT_LINES`] lines of the patch, starting at the
/// first hunk header. Headers with blob ids and modes are not interesting.
fn excerpt_from_patch(patch: &mut Patch<'_>) -> Option<String> {
    let buf = patch.to_buf().ok()?;
    let text = buf.as_str()?;
    let mut lines = Vec::new();
    let mut started = false;
    for line in text.lines() {
        if !started {
            if line.starts_with("@@") {
                started = true;
            } else {
                continue;
            }
        }
        lines.push(line);
        if lines.len() == MAX_DIFF_EXCERPT_LINES {
            break;
        }
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filter_without_patterns_allows_everything() {
        let f = ExcerptFilter::new(&[]).unwrap();
        assert!(f.allows("src/main.rs"));
        assert!(f.allows(".env"));
    }

    #[test]
    fn redact_patterns_block_matching_paths() {
        let f = ExcerptFilter::new(&["^secrets/".into(), r"\.env$".into()]).unwrap();
        assert!(!f.allows("secrets/token.txt"));
        assert!(!f.allows("config/.env"));
        assert!(f.allows("src/secrets.rs"));
    }

    #[test]
    fn a_broken_redact_pattern_is_an_error() {
        let err = ExcerptFilter::new(&["(".into()]).unwrap_err();
        assert!(format!("{err:#}").contains("privacy.redact"), "{err:#}");
    }

    #[test]
    fn discover_fails_outside_a_repository() {
        let dir = tempfile::TempDir::new().unwrap();
        let err = Repo::discover(dir.path()).map(|_| ()).unwrap_err();
        assert!(format!("{err:#}").contains("git repository"), "{err:#}");
    }
}
