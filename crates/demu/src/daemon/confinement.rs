//! Lexical path confinement for session output directories.
//!
//! The daemon treats every client-supplied file path as untrusted.
//! [`ConfinementDir`] anchors a session on the canonical form of its output
//! directory and confines every path under that root lexically:
//! [`ConfinementDir::join_confined`] inspects the components of a token and
//! performs no filesystem access, so it behaves the same for files that do
//! not exist yet. Parent-directory components are rejected outright rather
//! than resolved, and a final [`Path::starts_with`] guard keeps the returned
//! path inside the root.

use std::path::{Component, Path, PathBuf};

/// A directory that confines every file path under its root, lexically.
#[derive(Debug, Clone)]
pub struct ConfinementDir {
    /// The canonical root directory every confined path must start with.
    root: PathBuf,
}

/// A confinement failure.
#[derive(Debug, thiserror::Error)]
pub enum ConfinementError {
    /// The client supplied an empty path token.
    #[error("empty path token")]
    Empty,
    /// The client supplied a path that reaches outside the root.
    #[error("path {raw:?} escapes the session directory")]
    Escape {
        /// The raw token exactly as the client supplied it.
        raw: String,
    },
    /// Preparing the confinement directory failed.
    #[error("failed to prepare the confinement directory: {0}")]
    Io(#[from] std::io::Error),
}

impl ConfinementDir {
    /// Create the directory (with parents) and anchor on its canonical form.
    pub fn create(path: &Path) -> Result<Self, ConfinementError> {
        std::fs::create_dir_all(path)?;
        let root = std::fs::canonicalize(path)?;
        Ok(Self { root })
    }

    /// The canonical root path.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Join a user-supplied token under the root, lexically.
    ///
    /// The check never touches the filesystem, so it works for files that do
    /// not exist yet. An empty token is [`ConfinementError::Empty`]; any
    /// parent-directory component is an outright
    /// [`ConfinementError::Escape`]; an absolute token is rebuilt from its
    /// normal components and must land under the root.
    pub fn join_confined(&self, token: &str) -> Result<PathBuf, ConfinementError> {
        if token.is_empty() {
            return Err(ConfinementError::Empty);
        }
        let token_path = Path::new(token);
        if token_path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(ConfinementError::Escape {
                raw: token.to_string(),
            });
        }
        let resolved = if token_path.is_absolute() {
            let mut rebuilt = PathBuf::from("/");
            for component in token_path.components() {
                match component {
                    Component::Normal(part) => rebuilt.push(part),
                    Component::RootDir | Component::CurDir => {}
                    Component::Prefix(_) | Component::ParentDir => {
                        return Err(ConfinementError::Escape {
                            raw: token.to_string(),
                        });
                    }
                }
            }
            rebuilt
        } else {
            self.root.join(token_path)
        };
        if !resolved.starts_with(&self.root) {
            return Err(ConfinementError::Escape {
                raw: token.to_string(),
            });
        }
        Ok(resolved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp directory cleared of any prior test artifact.
    fn temp_dir(case: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("demu-daemon-confinement-{case}"));
        std::fs::remove_dir_all(&path).ok();
        path
    }

    #[test]
    fn a_relative_token_joins_under_the_root() {
        let dir = ConfinementDir::create(&temp_dir("relative")).expect("create");
        let confined = dir.join_confined("sub/file").expect("confined join");
        // The join never touches the filesystem, so the file need not exist.
        assert!(!confined.exists(), "the joined file exists already");
        assert_eq!(confined, dir.root().join("sub/file"));
    }

    #[test]
    fn traversal_tokens_reject() {
        let dir = ConfinementDir::create(&temp_dir("traversal")).expect("create");
        for token in ["../outside", "sub/../../outside"] {
            let error = dir.join_confined(token).expect_err("traversal rejects");
            assert!(
                matches!(error, ConfinementError::Escape { .. }),
                "traversal rejects: {error}"
            );
        }
    }

    #[test]
    fn an_absolute_token_outside_the_root_rejects() {
        let dir = ConfinementDir::create(&temp_dir("absolute-outside")).expect("create");
        let error = dir
            .join_confined("/etc/passwd")
            .expect_err("the absolute token rejects");
        assert!(
            matches!(error, ConfinementError::Escape { .. }),
            "the absolute token rejects: {error}"
        );
    }

    #[test]
    fn an_absolute_token_inside_the_root_is_accepted() {
        let dir = ConfinementDir::create(&temp_dir("absolute-inside")).expect("create");
        let token = dir.root().join("inside");
        let raw = token.to_str().expect("the temp path is utf-8");
        let confined = dir.join_confined(raw).expect("confined join");
        assert_eq!(confined, token);
    }

    #[test]
    fn the_filesystem_root_token_rejects() {
        let dir = ConfinementDir::create(&temp_dir("root-token")).expect("create");
        let error = dir.join_confined("/").expect_err("the root token rejects");
        assert!(
            matches!(error, ConfinementError::Escape { .. }),
            "the root token rejects: {error}"
        );
    }

    #[test]
    fn an_empty_token_rejects() {
        let dir = ConfinementDir::create(&temp_dir("empty")).expect("create");
        let error = dir.join_confined("").expect_err("the empty token rejects");
        assert!(
            matches!(error, ConfinementError::Empty),
            "the empty token rejects: {error}"
        );
    }

    #[test]
    fn appending_a_suffix_keeps_the_confined_path_under_the_root() {
        let dir = ConfinementDir::create(&temp_dir("suffix")).expect("create");
        let confined = dir.join_confined("shot").expect("confined join");
        let mut suffixed = confined.to_string_lossy().into_owned();
        suffixed.push_str(".vram");
        let suffixed = Path::new(&suffixed);
        assert!(
            suffixed.starts_with(dir.root()),
            "the suffixed path stays under the root: {suffixed:?}"
        );
    }

    #[test]
    fn create_builds_nested_directories_and_anchors_on_the_canonical_root() {
        let target = temp_dir("nested").join("one").join("two");
        let dir = ConfinementDir::create(&target).expect("create");
        let canonical = std::fs::canonicalize(&target).expect("canonicalize");
        assert_eq!(dir.root(), canonical.as_path());
    }

    #[test]
    fn create_reports_a_filesystem_error() {
        let base = temp_dir("blocked");
        std::fs::create_dir(&base).expect("base directory");
        std::fs::write(base.join("blocker"), "not a directory\n").expect("blocker file");
        let target = base.join("blocker").join("child");
        let error = ConfinementDir::create(&target).expect_err("create fails");
        assert!(
            matches!(error, ConfinementError::Io(_)),
            "create fails with an io error: {error}"
        );
    }
}
