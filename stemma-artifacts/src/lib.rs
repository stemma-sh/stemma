//! Host-side artifact access for Stemma transports.
//!
//! The DOCX engine deliberately accepts and returns bytes. This crate owns the
//! separate host boundary: resolving caller paths, confining agent-controlled
//! access to one root, identifying exact input bytes, protecting consumed
//! sources from output aliasing, and committing new artifacts without clobbering
//! an existing path.
//! Artifact identities are portable serialized receipts, so supplied and
//! resolved paths that are not valid UTF-8 are refused before source bytes are
//! opened or output staging begins.
//!
//! `commit_new` provides staged, create-new visibility. It does not promise
//! cleanup after process termination or power-loss durability. Platform commit
//! primitives may also leave the same complete staged file under its temporary
//! name if destination creation succeeds but staging-link cleanup fails. The
//! boundary does not defend against a malicious local process racing path
//! components during a call. Transports must describe those limits rather than
//! implying a stronger operating-system sandbox.

#![forbid(unsafe_code)]

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
#[cfg(any(test, feature = "test-support"))]
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use same_file::is_same_file;
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use tempfile::{Builder as TempBuilder, NamedTempFile};
use thiserror::Error;

mod task_manifest;

pub use task_manifest::*;

/// The digest algorithm used for exact artifact identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DigestAlgorithm {
    Sha256,
}

/// An algorithm-qualified digest of exact artifact bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct ArtifactDigest {
    /// Algorithm used to produce `hex`.
    pub algorithm: DigestAlgorithm,
    /// Lowercase hexadecimal digest bytes.
    pub hex: String,
}

/// Serializable identity for one input or output artifact.
///
/// The boundary only returns identities whose supplied and resolved paths are
/// valid UTF-8, so derived serialization cannot fail on either path field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct ArtifactIdentity {
    /// Caller-defined role such as `input_docx` or `output_redline`.
    pub role: String,
    /// The path exactly as supplied by the caller.
    pub supplied_path: PathBuf,
    /// The path resolved by this authority for filesystem access.
    pub resolved_path: PathBuf,
    /// SHA-256 over the exact bytes read or committed.
    pub digest: ArtifactDigest,
    /// Exact byte length of the artifact.
    pub bytes: u64,
}

/// An input artifact and the exact bytes whose identity it carries.
#[derive(Debug)]
pub struct ReadArtifact {
    identity: ArtifactIdentity,
    bytes: Vec<u8>,
}

impl ReadArtifact {
    /// Exact-byte identity of this source artifact.
    pub fn identity(&self) -> &ArtifactIdentity {
        &self.identity
    }

    /// Source bytes covered by [`ReadArtifact::identity`].
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Consume the artifact and return its source bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// Consume the artifact without losing either identity or bytes.
    pub fn into_parts(self) -> (ArtifactIdentity, Vec<u8>) {
        (self.identity, self.bytes)
    }
}

/// The only collision policy supported by this first boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CollisionPolicy {
    CreateNew,
}

/// What a successful commit did to its destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ArtifactDisposition {
    Created,
}

/// Identity and persistence policy for a successfully committed artifact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct OutputArtifact {
    /// Exact identity of the committed bytes.
    pub identity: ArtifactIdentity,
    /// Collision policy enforced by the commit.
    pub collision_policy: CollisionPolicy,
    /// Actual destination disposition.
    pub disposition: ArtifactDisposition,
}

#[derive(Clone, Debug)]
enum AuthorityMode {
    /// All caller paths must resolve beneath this canonical root.
    Rooted { root: PathBuf },
    /// A human explicitly supplied the paths; relative paths use this fixed base.
    Explicit { base: PathBuf },
}

/// Filesystem authority used by a transport edge.
///
/// `rooted` is for agent-controlled transports. `explicit` is for a direct CLI
/// invocation where each path is itself the user's grant of authority; it still
/// fixes relative paths to the construction-time current directory.
#[derive(Clone, Debug)]
pub struct PathAuthority {
    mode: AuthorityMode,
    #[cfg(any(test, feature = "test-support"))]
    failpoint: Option<CommitFailpoint>,
    #[cfg(any(test, feature = "test-support"))]
    failpoint_fired: Arc<AtomicBool>,
}

impl PathAuthority {
    /// Constrain all access to one existing directory.
    pub fn rooted(root: impl AsRef<Path>) -> Result<Self, ArtifactError> {
        let supplied = root.as_ref().to_path_buf();
        let resolved = canonicalize_from_current(&supplied).map_err(|source| {
            ArtifactError::AuthorityResolution {
                supplied: supplied.clone(),
                source,
            }
        })?;
        require_directory(&supplied, &resolved)?;
        require_utf8_identity_path(&resolved, "authority", "root")?;
        Ok(Self {
            mode: AuthorityMode::Rooted { root: resolved },
            #[cfg(any(test, feature = "test-support"))]
            failpoint: None,
            #[cfg(any(test, feature = "test-support"))]
            failpoint_fired: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Grant access to paths explicitly supplied by a human caller.
    pub fn explicit() -> Result<Self, ArtifactError> {
        let base = std::env::current_dir().map_err(ArtifactError::CurrentDirectory)?;
        Self::explicit_at(base)
    }

    /// As [`PathAuthority::explicit`], with a fixed base for relative paths.
    pub fn explicit_at(base: impl AsRef<Path>) -> Result<Self, ArtifactError> {
        let supplied = base.as_ref().to_path_buf();
        let resolved = canonicalize_from_current(&supplied).map_err(|source| {
            ArtifactError::AuthorityResolution {
                supplied: supplied.clone(),
                source,
            }
        })?;
        require_directory(&supplied, &resolved)?;
        Ok(Self {
            mode: AuthorityMode::Explicit { base: resolved },
            #[cfg(any(test, feature = "test-support"))]
            failpoint: None,
            #[cfg(any(test, feature = "test-support"))]
            failpoint_fired: Arc::new(AtomicBool::new(false)),
        })
    }

    /// The canonical allowed root, or `None` for explicit human authority.
    pub fn root(&self) -> Option<&Path> {
        match &self.mode {
            AuthorityMode::Rooted { root } => Some(root),
            AuthorityMode::Explicit { .. } => None,
        }
    }

    /// Resolve and validate a create-new destination without writing it.
    ///
    /// This is a declaration-time check only; callers must still use
    /// commit_new, which repeats the collision check atomically at commit time.
    /// It exists so a task can bind its manifest destination and reject an
    /// existing path before any document mutation.
    pub fn resolve_new_path(&self, path: impl AsRef<Path>) -> Result<PathBuf, ArtifactError> {
        let supplied_path = path.as_ref().to_path_buf();
        require_utf8_identity_path(&supplied_path, "destination", "supplied")?;
        require_no_windows_stream_syntax(&supplied_path, "destination")?;
        let (resolved_path, _) = self.resolve_destination(&supplied_path)?;
        require_utf8_identity_path(&resolved_path, "destination", "resolved")?;
        require_no_windows_stream_syntax(&resolved_path, "destination")?;
        match fs::symlink_metadata(&resolved_path) {
            Ok(_) => {
                if let Ok(existing_resolved) = fs::canonicalize(&resolved_path) {
                    self.require_authorized(&supplied_path, &existing_resolved)?;
                }
                Err(ArtifactError::OutputExists {
                    path: resolved_path,
                })
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(resolved_path),
            Err(source) => Err(ArtifactError::DestinationMetadata {
                path: resolved_path,
                source,
            }),
        }
    }

    /// Read one source artifact under this authority and identify its exact bytes.
    ///
    /// `max_bytes` is checked both before and during the read. `None` deliberately
    /// means unbounded by this crate; transports should pass their own resource
    /// limit for agent-controlled input.
    pub fn read_source(
        &self,
        path: impl AsRef<Path>,
        role: impl Into<String>,
        max_bytes: Option<u64>,
    ) -> Result<ReadArtifact, ArtifactError> {
        let role = validate_role(role.into())?;
        let supplied_path = path.as_ref().to_path_buf();
        require_utf8_identity_path(&supplied_path, "source", "supplied")?;
        require_no_windows_stream_syntax(&supplied_path, "source")?;
        let resolved_path = self.resolve_existing(&supplied_path, "source")?;
        require_utf8_identity_path(&resolved_path, "source", "resolved")?;
        require_no_windows_stream_syntax(&resolved_path, "source")?;

        // Reject obvious FIFOs/devices before File::open, which can block for a
        // FIFO with no writer. The handle check below remains necessary for a
        // path replacement race between this metadata call and open.
        let path_metadata =
            fs::metadata(&resolved_path).map_err(|source| ArtifactError::ReadMetadata {
                path: resolved_path.clone(),
                source,
            })?;
        if !path_metadata.is_file() {
            return Err(ArtifactError::SourceNotFile {
                path: resolved_path,
            });
        }

        let mut file = File::open(&resolved_path).map_err(|source| ArtifactError::ReadOpen {
            path: resolved_path.clone(),
            source,
        })?;
        let metadata = file
            .metadata()
            .map_err(|source| ArtifactError::ReadMetadata {
                path: resolved_path.clone(),
                source,
            })?;
        if !metadata.is_file() {
            return Err(ArtifactError::SourceNotFile {
                path: resolved_path,
            });
        }
        if let Some(limit) = max_bytes
            && metadata.len() > limit
        {
            return Err(ArtifactError::SourceTooLarge {
                path: resolved_path,
                size: metadata.len(),
                limit,
            });
        }

        let mut bytes = Vec::new();
        if let Some(limit) = max_bytes {
            let read_limit = limit.saturating_add(1);
            Read::by_ref(&mut file)
                .take(read_limit)
                .read_to_end(&mut bytes)
                .map_err(|source| ArtifactError::Read {
                    path: resolved_path.clone(),
                    source,
                })?;
            if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
                return Err(ArtifactError::SourceTooLarge {
                    path: resolved_path,
                    size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                    limit,
                });
            }
        } else {
            file.read_to_end(&mut bytes)
                .map_err(|source| ArtifactError::Read {
                    path: resolved_path.clone(),
                    source,
                })?;
        }

        let identity = ArtifactIdentity {
            role,
            supplied_path,
            resolved_path,
            digest: digest_bytes(&bytes),
            bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        };
        Ok(ReadArtifact { identity, bytes })
    }

    /// Stage and commit a new artifact without replacing any existing path.
    ///
    /// Every previously consumed source identity for the operation or session
    /// must be supplied. A literal source path is protected even if another
    /// process deletes it after it was read; existing symlink and hard-link
    /// aliases are detected where the platform exposes same-file identity.
    pub fn commit_new(
        &self,
        path: impl AsRef<Path>,
        role: impl Into<String>,
        bytes: &[u8],
        protected_sources: &[ArtifactIdentity],
    ) -> Result<OutputArtifact, ArtifactError> {
        let role = validate_role(role.into())?;
        let supplied_path = path.as_ref().to_path_buf();
        require_utf8_identity_path(&supplied_path, "destination", "supplied")?;
        require_no_windows_stream_syntax(&supplied_path, "destination")?;
        let (resolved_path, parent) = self.resolve_destination(&supplied_path)?;
        require_utf8_identity_path(&resolved_path, "destination", "resolved")?;
        require_no_windows_stream_syntax(&resolved_path, "destination")?;

        for source in protected_sources {
            if resolved_path == source.resolved_path {
                return Err(protected_source_error(&resolved_path, source));
            }
        }

        match fs::symlink_metadata(&resolved_path) {
            Ok(_) => {
                // Resolve an existing symlink before returning a collision so a
                // rooted caller gets the more important authority violation.
                if let Ok(existing_resolved) = fs::canonicalize(&resolved_path) {
                    self.require_authorized(&supplied_path, &existing_resolved)?;
                }
                // `same-file` opens the destination. Restrict that probe to a
                // regular file so an existing FIFO/device cannot block the
                // create-new refusal path.
                let aliases_can_be_checked =
                    fs::metadata(&resolved_path).is_ok_and(|metadata| metadata.is_file());
                if aliases_can_be_checked {
                    for source in protected_sources {
                        let source_metadata =
                            fs::metadata(&source.resolved_path).map_err(|source_error| {
                                ArtifactError::AliasCheck {
                                    destination: resolved_path.clone(),
                                    source_path: source.resolved_path.clone(),
                                    source: source_error,
                                }
                            })?;
                        if !source_metadata.is_file() {
                            return Err(ArtifactError::AliasCheck {
                                destination: resolved_path,
                                source_path: source.resolved_path.clone(),
                                source: io::Error::new(
                                    io::ErrorKind::InvalidInput,
                                    "protected source is no longer a regular file",
                                ),
                            });
                        }
                        match is_same_file(&resolved_path, &source.resolved_path) {
                            Ok(true) => {
                                return Err(protected_source_error(&resolved_path, source));
                            }
                            Ok(false) => {}
                            Err(error) => {
                                return Err(ArtifactError::AliasCheck {
                                    destination: resolved_path,
                                    source_path: source.resolved_path.clone(),
                                    source: error,
                                });
                            }
                        }
                    }
                }
                return Err(ArtifactError::OutputExists {
                    path: resolved_path,
                });
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(ArtifactError::DestinationMetadata {
                    path: resolved_path,
                    source,
                });
            }
        }

        let expected_digest = digest_bytes(bytes);
        let expected_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let mut stage_builder = TempBuilder::new();
        stage_builder.prefix(".stemma-stage-");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            stage_builder.permissions(fs::Permissions::from_mode(0o666));
        }
        let mut staged =
            stage_builder
                .tempfile_in(&parent)
                .map_err(|source| ArtifactError::StageCreate {
                    directory: parent.clone(),
                    source,
                })?;
        staged
            .write_all(bytes)
            .map_err(|source| ArtifactError::StageWrite {
                destination: resolved_path.clone(),
                source,
            })?;
        staged.flush().map_err(|source| ArtifactError::StageFlush {
            destination: resolved_path.clone(),
            source,
        })?;
        staged
            .as_file()
            .sync_all()
            .map_err(|source| ArtifactError::StageSync {
                destination: resolved_path.clone(),
                source,
            })?;

        #[cfg(any(test, feature = "test-support"))]
        self.apply_failpoint(&mut staged, &resolved_path)?;

        let (staged_digest, staged_len) = digest_staged(&staged, &resolved_path)?;
        if staged_digest != expected_digest || staged_len != expected_len {
            return Err(ArtifactError::StageVerificationMismatch {
                destination: resolved_path,
                expected_digest,
                actual_digest: staged_digest,
                expected_bytes: expected_len,
                actual_bytes: staged_len,
            });
        }

        let committed = match staged.persist_noclobber(&resolved_path) {
            Ok(file) => file,
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                return Err(ArtifactError::OutputExists {
                    path: resolved_path,
                });
            }
            Err(error) => {
                return Err(ArtifactError::Commit {
                    destination: resolved_path,
                    source: error.error,
                });
            }
        };

        #[cfg(any(test, feature = "test-support"))]
        if self.failpoint == Some(CommitFailpoint::AfterCommit) {
            drop(committed);
            return Err(self.postcommit_failpoint_error(&resolved_path));
        }

        let mut committed = committed;

        let (actual_digest, actual_len) = match digest_file(&mut committed) {
            Ok(identity) => identity,
            Err(source) => {
                drop(committed);
                let cleanup_error = rollback_new_artifact(&resolved_path);
                return Err(ArtifactError::CommitVerificationRead {
                    destination: resolved_path,
                    source,
                    cleanup_error,
                });
            }
        };
        if actual_digest != expected_digest || actual_len != expected_len {
            // This is an invariant failure after the create-new commit. Removing
            // the new path is the only safe rollback; any cleanup failure is
            // disclosed in-band rather than absorbed.
            drop(committed);
            let cleanup_error = rollback_new_artifact(&resolved_path);
            return Err(ArtifactError::CommitVerificationMismatch {
                destination: resolved_path,
                expected_digest,
                actual_digest,
                expected_bytes: expected_len,
                actual_bytes: actual_len,
                cleanup_error,
            });
        }

        Ok(OutputArtifact {
            identity: ArtifactIdentity {
                role,
                supplied_path,
                resolved_path,
                digest: expected_digest,
                bytes: expected_len,
            },
            collision_policy: CollisionPolicy::CreateNew,
            disposition: ArtifactDisposition::Created,
        })
    }

    fn resolve_existing(
        &self,
        supplied: &Path,
        kind: &'static str,
    ) -> Result<PathBuf, ArtifactError> {
        let candidate = self.candidate(supplied);
        self.require_lexically_authorized(supplied, &candidate)?;
        let resolved =
            fs::canonicalize(&candidate).map_err(|source| ArtifactError::PathResolution {
                kind,
                supplied: supplied.to_path_buf(),
                candidate,
                source,
            })?;
        self.require_authorized(supplied, &resolved)?;
        Ok(resolved)
    }

    fn resolve_destination(&self, supplied: &Path) -> Result<(PathBuf, PathBuf), ArtifactError> {
        if supplied.as_os_str().is_empty() {
            return Err(ArtifactError::InvalidDestination {
                supplied: supplied.to_path_buf(),
                reason: "destination must name a file",
            });
        }
        let candidate = self.candidate(supplied);
        self.require_lexically_authorized(supplied, &candidate)?;
        let file_name = candidate
            .file_name()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| ArtifactError::InvalidDestination {
                supplied: supplied.to_path_buf(),
                reason: "destination must name a file",
            })?;
        let parent = candidate
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let resolved_parent =
            fs::canonicalize(parent).map_err(|source| ArtifactError::PathResolution {
                kind: "destination parent",
                supplied: supplied.to_path_buf(),
                candidate: parent.to_path_buf(),
                source,
            })?;
        require_directory(supplied, &resolved_parent)?;
        self.require_authorized(supplied, &resolved_parent)?;
        Ok((resolved_parent.join(file_name), resolved_parent))
    }

    fn candidate(&self, supplied: &Path) -> PathBuf {
        if supplied.is_absolute() {
            return supplied.to_path_buf();
        }
        match &self.mode {
            AuthorityMode::Rooted { root } => root.join(supplied),
            AuthorityMode::Explicit { base } => base.join(supplied),
        }
    }

    fn require_authorized(&self, supplied: &Path, resolved: &Path) -> Result<(), ArtifactError> {
        let AuthorityMode::Rooted { root } = &self.mode else {
            return Ok(());
        };
        if resolved.starts_with(root) {
            return Ok(());
        }
        Err(ArtifactError::PathOutsideAuthority {
            supplied: supplied.to_path_buf(),
            resolved: resolved.to_path_buf(),
            root: root.clone(),
        })
    }

    /// Reject obvious rooted escapes before any metadata lookup. Canonical
    /// authorization below remains necessary for symlinked path components.
    ///
    /// Containment is decided by canonical identity, not spelling: an absolute
    /// path may name a location inside the root under a different surface form
    /// (Windows 8.3 short names, a symlinked temp directory such as macOS
    /// `/var` -> `/private/var`, verbatim `\\?\` prefixes). When the candidate
    /// does not lexically start with the canonical root, containment is probed
    /// by resolving the candidate (or, for a not-yet-created destination leaf,
    /// its parent). Every probe failure maps to the same authority refusal so
    /// this gate cannot be used as an existence probe for paths outside the
    /// root. Parent (`..`) components are refused outright regardless of
    /// where they would resolve.
    fn require_lexically_authorized(
        &self,
        supplied: &Path,
        candidate: &Path,
    ) -> Result<(), ArtifactError> {
        let AuthorityMode::Rooted { root } = &self.mode else {
            return Ok(());
        };
        let has_parent_component = supplied
            .components()
            .any(|component| component == Component::ParentDir);
        if !has_parent_component
            && (candidate.starts_with(root) || canonical_containment_probe(root, candidate))
        {
            return Ok(());
        }
        Err(ArtifactError::PathOutsideAuthority {
            supplied: supplied.to_path_buf(),
            resolved: candidate.to_path_buf(),
            root: root.clone(),
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn with_commit_failpoint(mut self, failpoint: CommitFailpoint) -> Self {
        self.failpoint = Some(failpoint);
        self.failpoint_fired = Arc::new(AtomicBool::new(false));
        self
    }

    #[cfg(any(test, feature = "test-support"))]
    fn apply_failpoint(
        &self,
        staged: &mut NamedTempFile,
        destination: &Path,
    ) -> Result<(), ArtifactError> {
        match self.failpoint {
            None | Some(CommitFailpoint::AfterCommit) => Ok(()),
            Some(CommitFailpoint::AfterStageSync) => Err(ArtifactError::InjectedFailure {
                point: "after_stage_sync",
                destination: destination.to_path_buf(),
            }),
            Some(CommitFailpoint::AfterStageSyncOnce) => {
                if self.failpoint_fired.swap(true, Ordering::SeqCst) {
                    Ok(())
                } else {
                    Err(ArtifactError::InjectedFailure {
                        point: "after_stage_sync_once",
                        destination: destination.to_path_buf(),
                    })
                }
            }
            Some(CommitFailpoint::CorruptStagedBytes) => {
                staged
                    .as_file_mut()
                    .write_all(b"injected-corruption")
                    .map_err(|source| ArtifactError::StageWrite {
                        destination: destination.to_path_buf(),
                        source,
                    })?;
                staged.flush().map_err(|source| ArtifactError::StageFlush {
                    destination: destination.to_path_buf(),
                    source,
                })?;
                Ok(())
            }
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    fn postcommit_failpoint_error(&self, destination: &Path) -> ArtifactError {
        debug_assert_eq!(self.failpoint, Some(CommitFailpoint::AfterCommit));
        let cleanup_error = rollback_new_artifact(destination);
        if let Some(cleanup_error) = cleanup_error {
            return ArtifactError::InjectedFailureCleanup {
                point: "after_commit",
                destination: destination.to_path_buf(),
                cleanup_error,
            };
        }
        ArtifactError::InjectedFailure {
            point: "after_commit",
            destination: destination.to_path_buf(),
        }
    }
}

/// Deterministic failure injection for artifact-boundary tests.
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[doc(hidden)]
pub enum CommitFailpoint {
    AfterStageSync,
    AfterStageSyncOnce,
    CorruptStagedBytes,
    AfterCommit,
}

/// Typed, contextual failures from the artifact boundary.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ArtifactError {
    #[error("artifact role must not be empty")]
    InvalidRole,
    #[error("cannot determine the current directory: {0}")]
    CurrentDirectory(#[source] io::Error),
    #[error("cannot resolve filesystem authority {supplied}: {source}")]
    AuthorityResolution {
        supplied: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("filesystem authority {supplied} resolved to {resolved}, which is not a directory")]
    AuthorityNotDirectory {
        supplied: PathBuf,
        resolved: PathBuf,
    },
    #[error(
        "cannot identify {operation} artifact: its {representation} path {path:?} is not valid UTF-8 and cannot be represented in serialized receipts"
    )]
    IdentityPathNotUtf8 {
        operation: &'static str,
        representation: &'static str,
        path: PathBuf,
    },
    #[error(
        "refusing {operation} path {path}: Windows alternate data stream syntax is outside the artifact contract"
    )]
    WindowsAlternateDataStream {
        operation: &'static str,
        path: PathBuf,
    },
    #[error("cannot resolve {kind} path {supplied} (candidate {candidate}): {source}")]
    PathResolution {
        kind: &'static str,
        supplied: PathBuf,
        candidate: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "path {supplied} resolves or would access {resolved}, outside the allowed root {root}; choose a path without parent traversal inside that root"
    )]
    PathOutsideAuthority {
        supplied: PathBuf,
        resolved: PathBuf,
        root: PathBuf,
    },
    #[error("invalid destination {supplied}: {reason}")]
    InvalidDestination {
        supplied: PathBuf,
        reason: &'static str,
    },
    #[error("source path {path} is not a regular file")]
    SourceNotFile { path: PathBuf },
    #[error("cannot open source artifact {path}: {source}")]
    ReadOpen {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot inspect source artifact {path}: {source}")]
    ReadMetadata {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot read source artifact {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("source artifact {path} is {size} bytes, over the {limit}-byte limit")]
    SourceTooLarge {
        path: PathBuf,
        size: u64,
        limit: u64,
    },
    #[error("cannot inspect destination {path}: {source}")]
    DestinationMetadata {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "refusing to write {destination}: it aliases protected source {source_path} ({source_role})"
    )]
    ProtectedSource {
        destination: PathBuf,
        source_path: PathBuf,
        source_role: String,
    },
    #[error(
        "cannot prove whether destination {destination} aliases source {source_path}: {source}"
    )]
    AliasCheck {
        destination: PathBuf,
        source_path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("refusing to replace existing output {path}: this boundary supports create-new only")]
    OutputExists { path: PathBuf },
    #[error("cannot create a staging file in {directory}: {source}")]
    StageCreate {
        directory: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot write staged artifact for {destination}: {source}")]
    StageWrite {
        destination: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot flush staged artifact for {destination}: {source}")]
    StageFlush {
        destination: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot sync staged artifact for {destination}: {source}")]
    StageSync {
        destination: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot reopen staged artifact for {destination}: {source}")]
    StageVerificationRead {
        destination: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "staged artifact for {destination} does not match supplied bytes (expected {expected_bytes} bytes/{expected_digest:?}, got {actual_bytes} bytes/{actual_digest:?})"
    )]
    StageVerificationMismatch {
        destination: PathBuf,
        expected_digest: ArtifactDigest,
        actual_digest: ArtifactDigest,
        expected_bytes: u64,
        actual_bytes: u64,
    },
    #[cfg(any(test, feature = "test-support"))]
    #[error("injected artifact commit failure at {point} for {destination}")]
    InjectedFailure {
        point: &'static str,
        destination: PathBuf,
    },
    #[cfg(any(test, feature = "test-support"))]
    #[error(
        "injected artifact commit failure at {point} for {destination}; rollback failed: {cleanup_error}"
    )]
    InjectedFailureCleanup {
        point: &'static str,
        destination: PathBuf,
        cleanup_error: String,
    },
    #[error("cannot commit new artifact {destination}: {source}")]
    Commit {
        destination: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "cannot verify committed artifact {destination}: {source}; cleanup error: {cleanup_error:?}"
    )]
    CommitVerificationRead {
        destination: PathBuf,
        #[source]
        source: io::Error,
        cleanup_error: Option<String>,
    },
    #[error(
        "committed artifact {destination} does not match supplied bytes (expected {expected_bytes} bytes/{expected_digest:?}, got {actual_bytes} bytes/{actual_digest:?}); cleanup error: {cleanup_error:?}"
    )]
    CommitVerificationMismatch {
        destination: PathBuf,
        expected_digest: ArtifactDigest,
        actual_digest: ArtifactDigest,
        expected_bytes: u64,
        actual_bytes: u64,
        cleanup_error: Option<String>,
    },
}

fn canonicalize_from_current(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        fs::canonicalize(path)
    } else {
        fs::canonicalize(std::env::current_dir()?.join(path))
    }
}

fn require_directory(supplied: &Path, resolved: &Path) -> Result<(), ArtifactError> {
    let metadata = fs::metadata(resolved).map_err(|source| ArtifactError::AuthorityResolution {
        supplied: supplied.to_path_buf(),
        source,
    })?;
    if metadata.is_dir() {
        return Ok(());
    }
    Err(ArtifactError::AuthorityNotDirectory {
        supplied: supplied.to_path_buf(),
        resolved: resolved.to_path_buf(),
    })
}

fn validate_role(role: String) -> Result<String, ArtifactError> {
    if role.trim().is_empty() {
        return Err(ArtifactError::InvalidRole);
    }
    Ok(role)
}

fn require_utf8_identity_path(
    path: &Path,
    operation: &'static str,
    representation: &'static str,
) -> Result<(), ArtifactError> {
    if path.to_str().is_none() {
        return Err(ArtifactError::IdentityPathNotUtf8 {
            operation,
            representation,
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

/// Whether `candidate` names a location inside the canonical `root` once its
/// surface spelling is resolved. Only consulted when the lexical fast-path
/// failed. This is a gate, not the authority decision: everything it lets
/// through is re-checked against the root after full canonicalization in
/// `resolve_existing`/`resolve_destination`. A destination leaf that does not
/// exist yet is decided by its parent directory; `false` (refusal) covers
/// every resolution failure by design.
fn canonical_containment_probe(root: &Path, candidate: &Path) -> bool {
    if let Ok(resolved) = fs::canonicalize(candidate) {
        return resolved.starts_with(root);
    }
    let Some(parent) = candidate
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    else {
        return false;
    };
    let Ok(resolved_parent) = fs::canonicalize(parent) else {
        return false;
    };
    resolved_parent.starts_with(root)
}

fn has_windows_stream_component(path: &Path) -> bool {
    path.components().any(|component| {
        let Component::Normal(name) = component else {
            return false;
        };
        name.to_str().is_some_and(|name| name.contains(':'))
    })
}

fn require_no_windows_stream_syntax(
    path: &Path,
    operation: &'static str,
) -> Result<(), ArtifactError> {
    if has_windows_stream_component(path) {
        return Err(ArtifactError::WindowsAlternateDataStream {
            operation,
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

fn protected_source_error(destination: &Path, source: &ArtifactIdentity) -> ArtifactError {
    ArtifactError::ProtectedSource {
        destination: destination.to_path_buf(),
        source_path: source.resolved_path.clone(),
        source_role: source.role.clone(),
    }
}

fn rollback_new_artifact(path: &Path) -> Option<String> {
    fs::remove_file(path).err().map(|error| error.to_string())
}

fn digest_bytes(bytes: &[u8]) -> ArtifactDigest {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    ArtifactDigest {
        algorithm: DigestAlgorithm::Sha256,
        hex: format!("{:x}", hasher.finalize()),
    }
}

fn digest_staged(
    staged: &NamedTempFile,
    destination: &Path,
) -> Result<(ArtifactDigest, u64), ArtifactError> {
    let mut reader = staged
        .reopen()
        .map_err(|source| ArtifactError::StageVerificationRead {
            destination: destination.to_path_buf(),
            source,
        })?;
    digest_file(&mut reader).map_err(|source| ArtifactError::StageVerificationRead {
        destination: destination.to_path_buf(),
        source,
    })
}

fn digest_file(file: &mut File) -> io::Result<(ArtifactDigest, u64)> {
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        total = total.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
    }
    Ok((
        ArtifactDigest {
            algorithm: DigestAlgorithm::Sha256,
            hex: format!("{:x}", hasher.finalize()),
        },
        total,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;

    use tempfile::TempDir;

    fn write_file(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
    }

    fn stage_files(directory: &Path) -> Vec<PathBuf> {
        fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(".stemma-stage-"))
            })
            .collect()
    }

    fn explicit(directory: &Path) -> PathAuthority {
        PathAuthority::explicit_at(directory).expect("explicit authority")
    }

    fn rooted(directory: &Path) -> PathAuthority {
        PathAuthority::rooted(directory).expect("rooted authority")
    }

    #[cfg(unix)]
    fn symlink_file(source: &Path, destination: &Path) -> bool {
        std::os::unix::fs::symlink(source, destination).expect("create file symlink");
        true
    }

    /// APFS refuses to create names that are not valid UTF-8 (EILSEQ, os
    /// error 92 on macOS), so the on-disk halves of the non-UTF-8 identity
    /// tests cannot be provisioned there. Same bounded-skip pattern as the
    /// Windows symlink-privilege helper below: the limitation is reported,
    /// never absorbed, and any other error still panics.
    #[cfg(unix)]
    fn provision_non_utf8(result: io::Result<()>, what: &Path) -> bool {
        match result {
            Ok(()) => true,
            Err(error) if cfg!(target_os = "macos") && error.raw_os_error() == Some(92) => {
                eprintln!(
                    "skipping on-disk non-UTF-8 assertions: this filesystem cannot represent {}: {error}",
                    what.display()
                );
                false
            }
            Err(error) => panic!("provision non-UTF-8 name {}: {error}", what.display()),
        }
    }

    #[cfg(unix)]
    fn symlink_directory(source: &Path, destination: &Path) -> bool {
        std::os::unix::fs::symlink(source, destination).expect("create directory symlink");
        true
    }

    #[cfg(windows)]
    fn symlink_file(source: &Path, destination: &Path) -> bool {
        match std::os::windows::fs::symlink_file(source, destination) {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                eprintln!("skipping symlink assertion without Windows symlink privilege: {error}");
                false
            }
            Err(error) => panic!("create file symlink: {error}"),
        }
    }

    #[cfg(windows)]
    fn symlink_directory(source: &Path, destination: &Path) -> bool {
        match std::os::windows::fs::symlink_dir(source, destination) {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                eprintln!("skipping symlink assertion without Windows symlink privilege: {error}");
                false
            }
            Err(error) => panic!("create directory symlink: {error}"),
        }
    }

    #[test]
    fn rooted_read_returns_exact_serializable_identity() {
        let directory = TempDir::new().unwrap();
        let source = directory.path().join("input.bin");
        write_file(&source, b"abc");
        let authority = rooted(directory.path());

        let artifact = authority
            .read_source("input.bin", "input_docx", Some(3))
            .unwrap();

        assert_eq!(artifact.bytes(), b"abc");
        assert_eq!(artifact.identity().role, "input_docx");
        assert_eq!(artifact.identity().supplied_path, Path::new("input.bin"));
        assert_eq!(
            artifact.identity().resolved_path,
            source.canonicalize().unwrap()
        );
        assert_eq!(artifact.identity().bytes, 3);
        assert_eq!(
            artifact.identity().digest.hex,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let json = serde_json::to_value(artifact.identity()).unwrap();
        assert_eq!(json["digest"]["algorithm"], "sha256");
        assert_eq!(json["bytes"], 3);
        assert_eq!(
            authority.root(),
            Some(directory.path().canonicalize().unwrap().as_path())
        );
    }

    #[cfg(unix)]
    #[test]
    fn read_refuses_non_utf8_supplied_and_resolved_identity_paths() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        let directory = TempDir::new().unwrap();
        let invalid_name = OsString::from_vec(b"source-\xff.bin".to_vec());
        let invalid_relative = PathBuf::from(&invalid_name);
        let invalid_target = directory.path().join(&invalid_name);
        assert!(
            serde_json::to_value(&invalid_target).is_err(),
            "this regression requires a path serde cannot represent"
        );
        let on_disk = provision_non_utf8(fs::write(&invalid_target, b"source"), &invalid_target);

        // The supplied-representation refusal fires before any filesystem
        // access, so it holds whether or not the file could be provisioned.
        let authority = explicit(directory.path());
        let supplied_error = authority
            .read_source(&invalid_relative, "input", None)
            .expect_err("a non-UTF8 supplied path cannot produce a serializable identity");
        assert!(matches!(
            supplied_error,
            ArtifactError::IdentityPathNotUtf8 {
                operation: "source",
                representation: "supplied",
                ..
            }
        ));

        if !on_disk {
            return;
        }
        let alias = directory.path().join("utf8-alias.bin");
        assert!(symlink_file(&invalid_target, &alias));
        let resolved_error = authority
            .read_source("utf8-alias.bin", "input", None)
            .expect_err("a UTF-8 alias must not hide a non-UTF8 canonical identity path");
        assert!(matches!(
            resolved_error,
            ArtifactError::IdentityPathNotUtf8 {
                operation: "source",
                representation: "resolved",
                ..
            }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rooted_authority_rejects_non_utf8_root_while_explicit_defers_to_read() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        let world = TempDir::new().unwrap();
        let root = world
            .path()
            .join(OsString::from_vec(b"workspace-\xff".to_vec()));
        if !provision_non_utf8(fs::create_dir(&root), &root) {
            return;
        }
        write_file(&root.join("input.bin"), b"source");

        let rooted_error = PathAuthority::rooted(&root)
            .expect_err("a rooted transport cannot ever identify paths below this root");
        assert!(matches!(
            rooted_error,
            ArtifactError::IdentityPathNotUtf8 {
                operation: "authority",
                representation: "root",
                ..
            }
        ));

        let explicit = PathAuthority::explicit_at(&root)
            .expect("explicit CLI authority preserves its existing construction semantics");
        let read_error = explicit
            .read_source("input.bin", "input", None)
            .expect_err("explicit authority still refuses before emitting an invalid identity");
        assert!(matches!(
            read_error,
            ArtifactError::IdentityPathNotUtf8 {
                operation: "source",
                representation: "resolved",
                ..
            }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn commit_refuses_non_utf8_supplied_and_resolved_paths_before_staging() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        let directory = TempDir::new().unwrap();
        let authority = explicit(directory.path());
        let invalid_output = PathBuf::from(OsString::from_vec(b"output-\xff.bin".to_vec()));
        let supplied_error = authority
            .commit_new(&invalid_output, "output", b"bytes", &[])
            .expect_err("a non-UTF8 supplied destination cannot produce an identity");
        assert!(matches!(
            supplied_error,
            ArtifactError::IdentityPathNotUtf8 {
                operation: "destination",
                representation: "supplied",
                ..
            }
        ));
        assert!(!directory.path().join(&invalid_output).exists());
        assert!(stage_files(directory.path()).is_empty());

        let invalid_parent = directory
            .path()
            .join(OsString::from_vec(b"directory-\xff".to_vec()));
        if !provision_non_utf8(fs::create_dir(&invalid_parent), &invalid_parent) {
            return;
        }
        let alias_parent = directory.path().join("utf8-directory-alias");
        assert!(symlink_directory(&invalid_parent, &alias_parent));
        let resolved_error = authority
            .commit_new("utf8-directory-alias/output.bin", "output", b"bytes", &[])
            .expect_err("a UTF-8 parent alias must not hide a non-UTF8 resolved destination");
        assert!(matches!(
            resolved_error,
            ArtifactError::IdentityPathNotUtf8 {
                operation: "destination",
                representation: "resolved",
                ..
            }
        ));
        assert!(!invalid_parent.join("output.bin").exists());
        assert!(stage_files(&invalid_parent).is_empty());
    }

    #[test]
    fn read_source_enforces_the_pre_and_in_band_size_limit() {
        let directory = TempDir::new().unwrap();
        write_file(&directory.path().join("source.bin"), b"12345");

        let error = rooted(directory.path())
            .read_source("source.bin", "input", Some(4))
            .unwrap_err();

        assert!(matches!(
            error,
            ArtifactError::SourceTooLarge {
                size: 5,
                limit: 4,
                ..
            }
        ));
    }

    #[test]
    fn windows_stream_syntax_detection_is_component_scoped() {
        assert!(has_windows_stream_component(Path::new(
            "nested/input.docx:stemma-output"
        )));
        assert!(!has_windows_stream_component(Path::new(
            "nested/input.docx"
        )));
    }

    #[test]
    fn windows_alternate_stream_syntax_is_refused_before_read_or_staging() {
        let directory = TempDir::new().unwrap();
        let source_path = directory.path().join("input.docx");
        write_file(&source_path, b"source");
        let authority = rooted(directory.path());
        let source = authority.read_source("input.docx", "input", None).unwrap();

        let read_error = authority
            .read_source("input.docx:hidden", "input", None)
            .expect_err("ADS reads are outside the artifact contract");
        assert!(matches!(
            read_error,
            ArtifactError::WindowsAlternateDataStream {
                operation: "source",
                ..
            }
        ));

        let write_error = authority
            .commit_new(
                "input.docx:stemma-output",
                "output",
                b"new",
                std::slice::from_ref(source.identity()),
            )
            .expect_err("ADS writes must not attach a stream to a protected source");
        assert!(matches!(
            write_error,
            ArtifactError::WindowsAlternateDataStream {
                operation: "destination",
                ..
            }
        ));
        assert_eq!(fs::read(source_path).unwrap(), b"source");
        assert!(stage_files(directory.path()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn resolved_alias_cannot_hide_portable_stream_syntax() {
        let directory = TempDir::new().unwrap();
        let source = directory.path().join("source:stream");
        write_file(&source, b"source");
        let source_alias = directory.path().join("source-alias");
        assert!(symlink_file(&source, &source_alias));

        let authority = rooted(directory.path());
        let read_error = authority
            .read_source("source-alias", "input", None)
            .expect_err("canonical source syntax must be checked after alias resolution");
        assert!(matches!(
            read_error,
            ArtifactError::WindowsAlternateDataStream {
                operation: "source",
                ..
            }
        ));

        let output_parent = directory.path().join("output:streams");
        fs::create_dir(&output_parent).unwrap();
        let output_alias = directory.path().join("output-alias");
        assert!(symlink_directory(&output_parent, &output_alias));
        let write_error = authority
            .commit_new("output-alias/result.docx", "output", b"new", &[])
            .expect_err("canonical destination syntax must be checked before staging");
        assert!(matches!(
            write_error,
            ArtifactError::WindowsAlternateDataStream {
                operation: "destination",
                ..
            }
        ));
        assert!(!output_parent.join("result.docx").exists());
        assert!(stage_files(&output_parent).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn read_source_rejects_a_fifo_before_open_can_block() {
        let directory = TempDir::new().unwrap();
        let fifo = directory.path().join("source.fifo");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("run mkfifo");
        assert!(status.success(), "mkfifo failed with {status}");

        let error = rooted(directory.path())
            .read_source("source.fifo", "input", Some(16))
            .expect_err("a FIFO must be rejected before File::open");

        assert!(matches!(error, ArtifactError::SourceNotFile { .. }));
    }

    #[test]
    fn empty_roles_are_refused_at_the_boundary() {
        let directory = TempDir::new().unwrap();
        write_file(&directory.path().join("source.bin"), b"x");
        let authority = rooted(directory.path());

        assert!(matches!(
            authority.read_source("source.bin", "  ", None),
            Err(ArtifactError::InvalidRole)
        ));
        assert!(matches!(
            authority.commit_new("out.bin", "", b"x", &[]),
            Err(ArtifactError::InvalidRole)
        ));
    }

    #[test]
    fn rooted_authority_refuses_parent_and_absolute_escape() {
        let world = TempDir::new().unwrap();
        let root = world.path().join("root");
        fs::create_dir(&root).unwrap();
        let outside = world.path().join("outside.bin");
        write_file(&outside, b"outside");
        let authority = rooted(&root);

        let missing_outside = world.path().join("missing.bin");
        for path in [
            PathBuf::from("../outside.bin"),
            outside.clone(),
            PathBuf::from("../missing.bin"),
            missing_outside,
        ] {
            let error = authority.read_source(&path, "input", None).unwrap_err();
            assert!(
                matches!(error, ArtifactError::PathOutsideAuthority { .. }),
                "unexpected error for {}: {error}",
                path.display()
            );
        }

        for output in [
            PathBuf::from("../new-relative-output.bin"),
            world.path().join("new-absolute-output.bin"),
        ] {
            let error = authority
                .commit_new(&output, "output", b"bytes", &[])
                .unwrap_err();
            assert!(matches!(error, ArtifactError::PathOutsideAuthority { .. }));
            assert!(!world.path().join(output.file_name().unwrap()).exists());
        }
    }

    /// Containment is canonical identity, not spelling: the same in-root
    /// location supplied through an alternate surface form (here a directory
    /// symlink; on real hosts Windows 8.3 short names or the macOS
    /// `/var` -> `/private/var` temp alias) must be authorized for both reads
    /// and fresh destinations.
    #[test]
    fn rooted_authority_accepts_alternate_spellings_of_in_root_absolute_paths() {
        let world = TempDir::new().unwrap();
        let real_root = world.path().join("real-root");
        fs::create_dir(&real_root).unwrap();
        write_file(&real_root.join("input.bin"), b"source");
        let alias_root = world.path().join("alias-root");
        if !symlink_directory(&real_root, &alias_root) {
            return;
        }
        let canonical_root = real_root.canonicalize().unwrap();
        let authority = rooted(&real_root);

        let input = authority
            .read_source(alias_root.join("input.bin"), "input", None)
            .expect("an in-root absolute path is authorized regardless of spelling");
        assert_eq!(input.bytes(), b"source");
        assert_eq!(
            input.identity().resolved_path,
            canonical_root.join("input.bin")
        );

        let output = authority
            .commit_new(alias_root.join("output.bin"), "output", b"bytes", &[])
            .expect("a fresh in-root destination is authorized regardless of spelling");
        assert_eq!(
            output.identity.resolved_path,
            canonical_root.join("output.bin")
        );
        assert!(real_root.join("output.bin").exists());
    }

    #[test]
    fn explicit_authority_allows_each_human_supplied_path() {
        let world = TempDir::new().unwrap();
        let base = world.path().join("base");
        fs::create_dir(&base).unwrap();
        let outside = world.path().join("outside.bin");
        write_file(&outside, b"outside");
        let authority = explicit(&base);

        let input = authority
            .read_source("../outside.bin", "input", None)
            .unwrap();

        assert_eq!(input.bytes(), b"outside");
        assert_eq!(authority.root(), None);
    }

    #[test]
    fn rooted_read_refuses_an_existing_symlink_escape() {
        let world = TempDir::new().unwrap();
        let root = world.path().join("root");
        fs::create_dir(&root).unwrap();
        let outside = world.path().join("outside.bin");
        write_file(&outside, b"outside");
        let link = root.join("link.bin");
        if !symlink_file(&outside, &link) {
            return;
        }

        let error = rooted(&root)
            .read_source("link.bin", "input", None)
            .unwrap_err();

        assert!(matches!(error, ArtifactError::PathOutsideAuthority { .. }));
    }

    #[test]
    fn rooted_commit_refuses_a_parent_directory_symlink_escape() {
        let world = TempDir::new().unwrap();
        let root = world.path().join("root");
        let outside = world.path().join("outside");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        let linked_parent = root.join("linked-parent");
        if !symlink_directory(&outside, &linked_parent) {
            return;
        }

        let error = rooted(&root)
            .commit_new("linked-parent/output.bin", "output", b"new", &[])
            .unwrap_err();

        assert!(matches!(error, ArtifactError::PathOutsideAuthority { .. }));
        assert!(!outside.join("output.bin").exists());
    }

    #[test]
    fn rooted_commit_reports_an_existing_symlink_escape_as_authority_failure() {
        let world = TempDir::new().unwrap();
        let root = world.path().join("root");
        fs::create_dir(&root).unwrap();
        let outside = world.path().join("outside.bin");
        write_file(&outside, b"outside");
        let link = root.join("link.bin");
        if !symlink_file(&outside, &link) {
            return;
        }

        let error = rooted(&root)
            .commit_new("link.bin", "output", b"new", &[])
            .unwrap_err();

        assert!(matches!(error, ArtifactError::PathOutsideAuthority { .. }));
        assert_eq!(fs::read(outside).unwrap(), b"outside");
    }

    #[test]
    fn commit_new_stages_and_returns_the_exact_created_identity() {
        let directory = TempDir::new().unwrap();
        let source_path = directory.path().join("source.bin");
        write_file(&source_path, b"source");
        let authority = explicit(directory.path());
        let source = authority.read_source("source.bin", "input", None).unwrap();

        let output = authority
            .commit_new(
                "result.bin",
                "output_redline",
                b"result bytes",
                &[source.identity().clone()],
            )
            .unwrap();

        assert_eq!(
            fs::read(directory.path().join("result.bin")).unwrap(),
            b"result bytes"
        );
        assert_eq!(output.identity.role, "output_redline");
        assert_eq!(output.identity.supplied_path, Path::new("result.bin"));
        assert_eq!(output.identity.bytes, 12);
        assert_eq!(output.collision_policy, CollisionPolicy::CreateNew);
        assert_eq!(output.disposition, ArtifactDisposition::Created);
        assert_eq!(output.identity.digest, digest_bytes(b"result bytes"));
        assert!(stage_files(directory.path()).is_empty());

        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["collision_policy"], "create_new");
        assert_eq!(json["disposition"], "created");
    }

    #[test]
    fn zero_byte_artifacts_have_an_exact_identity() {
        let directory = TempDir::new().unwrap();
        let output = explicit(directory.path())
            .commit_new("empty.bin", "output", b"", &[])
            .unwrap();

        assert_eq!(output.identity.bytes, 0);
        assert_eq!(
            output.identity.digest.hex,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn existing_output_is_refused_without_changing_it() {
        let directory = TempDir::new().unwrap();
        let output = directory.path().join("output.bin");
        write_file(&output, b"keep me");

        let error = explicit(directory.path())
            .commit_new("output.bin", "output", b"replacement", &[])
            .unwrap_err();

        assert!(matches!(error, ArtifactError::OutputExists { .. }));
        assert_eq!(fs::read(output).unwrap(), b"keep me");
        assert!(stage_files(directory.path()).is_empty());
    }

    #[test]
    fn existing_output_fails_closed_when_a_protected_source_disappears() {
        let directory = TempDir::new().unwrap();
        let source_path = directory.path().join("source.bin");
        let output_path = directory.path().join("output.bin");
        write_file(&source_path, b"source");
        write_file(&output_path, b"keep me");
        let authority = explicit(directory.path());
        let source = authority.read_source("source.bin", "input", None).unwrap();
        fs::remove_file(source_path).unwrap();

        let error = authority
            .commit_new(
                "output.bin",
                "output",
                b"replacement",
                std::slice::from_ref(source.identity()),
            )
            .expect_err("missing protected source state must fail closed");

        assert!(matches!(error, ArtifactError::AliasCheck { .. }));
        assert_eq!(fs::read(output_path).unwrap(), b"keep me");
        assert!(stage_files(directory.path()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn existing_output_never_opens_a_protected_source_replaced_by_a_fifo() {
        let directory = TempDir::new().unwrap();
        let source_path = directory.path().join("source.bin");
        let output_path = directory.path().join("output.bin");
        write_file(&source_path, b"source");
        write_file(&output_path, b"keep me");
        let authority = explicit(directory.path());
        let source = authority.read_source("source.bin", "input", None).unwrap();
        fs::remove_file(&source_path).unwrap();
        let status = std::process::Command::new("mkfifo")
            .arg(&source_path)
            .status()
            .expect("run mkfifo");
        assert!(status.success(), "mkfifo failed with {status}");

        let error = authority
            .commit_new(
                "output.bin",
                "output",
                b"replacement",
                std::slice::from_ref(source.identity()),
            )
            .expect_err("non-regular protected source state must fail without opening it");

        assert!(matches!(error, ArtifactError::AliasCheck { .. }));
        assert_eq!(fs::read(output_path).unwrap(), b"keep me");
        assert!(stage_files(directory.path()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn committed_artifact_uses_normal_create_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = TempDir::new().unwrap();
        let reference_path = directory.path().join("reference.bin");
        File::create(&reference_path).unwrap();
        let expected_mode = fs::metadata(reference_path).unwrap().permissions().mode() & 0o777;

        explicit(directory.path())
            .commit_new("output.bin", "output", b"bytes", &[])
            .unwrap();
        let actual_mode = fs::metadata(directory.path().join("output.bin"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;

        assert_eq!(actual_mode, expected_mode);
    }

    #[test]
    fn literal_source_path_is_protected_even_after_the_file_is_deleted() {
        let directory = TempDir::new().unwrap();
        let source_path = directory.path().join("source.bin");
        write_file(&source_path, b"source");
        let authority = explicit(directory.path());
        let source = authority.read_source("source.bin", "input", None).unwrap();
        fs::remove_file(&source_path).unwrap();

        let error = authority
            .commit_new(
                "source.bin",
                "output",
                b"replacement",
                &[source.identity().clone()],
            )
            .unwrap_err();

        assert!(matches!(error, ArtifactError::ProtectedSource { .. }));
        assert!(!source_path.exists());
    }

    #[test]
    fn hard_link_alias_of_a_source_is_protected() {
        let directory = TempDir::new().unwrap();
        let source_path = directory.path().join("source.bin");
        let alias_path = directory.path().join("alias.bin");
        write_file(&source_path, b"source");
        let authority = explicit(directory.path());
        let source = authority.read_source("source.bin", "input", None).unwrap();
        fs::hard_link(&source_path, &alias_path).unwrap();

        let error = authority
            .commit_new(
                "alias.bin",
                "output",
                b"replacement",
                &[source.identity().clone()],
            )
            .unwrap_err();

        assert!(matches!(error, ArtifactError::ProtectedSource { .. }));
        assert_eq!(fs::read(source_path).unwrap(), b"source");
        assert_eq!(fs::read(alias_path).unwrap(), b"source");
    }

    #[test]
    fn symlink_alias_of_a_source_is_protected() {
        let directory = TempDir::new().unwrap();
        let source_path = directory.path().join("source.bin");
        let alias_path = directory.path().join("alias.bin");
        write_file(&source_path, b"source");
        let authority = explicit(directory.path());
        let source = authority.read_source("source.bin", "input", None).unwrap();
        if !symlink_file(&source_path, &alias_path) {
            return;
        }

        let error = authority
            .commit_new(
                "alias.bin",
                "output",
                b"replacement",
                &[source.identity().clone()],
            )
            .unwrap_err();

        assert!(matches!(error, ArtifactError::ProtectedSource { .. }));
        assert_eq!(fs::read(source_path).unwrap(), b"source");
    }

    #[test]
    fn normalized_literal_alias_of_a_source_is_protected() {
        let directory = TempDir::new().unwrap();
        let subdirectory = directory.path().join("sub");
        fs::create_dir(&subdirectory).unwrap();
        write_file(&directory.path().join("source.bin"), b"source");
        let authority = explicit(directory.path());
        let source = authority.read_source("source.bin", "input", None).unwrap();

        let error = authority
            .commit_new(
                "sub/../source.bin",
                "output",
                b"replacement",
                &[source.identity().clone()],
            )
            .unwrap_err();

        assert!(matches!(error, ArtifactError::ProtectedSource { .. }));
    }

    #[test]
    fn destination_requires_a_file_name_and_existing_parent() {
        let directory = TempDir::new().unwrap();
        let authority = explicit(directory.path());

        assert!(matches!(
            authority.commit_new("", "output", b"x", &[]),
            Err(ArtifactError::InvalidDestination { .. })
        ));
        assert!(matches!(
            authority.commit_new("missing/out.bin", "output", b"x", &[]),
            Err(ArtifactError::PathResolution { .. })
        ));
    }

    #[test]
    fn authority_must_resolve_to_an_existing_directory() {
        let directory = TempDir::new().unwrap();
        let file = directory.path().join("file.bin");
        write_file(&file, b"not a directory");

        assert!(matches!(
            PathAuthority::rooted(&file),
            Err(ArtifactError::AuthorityNotDirectory { .. })
        ));
        assert!(matches!(
            PathAuthority::rooted(directory.path().join("missing")),
            Err(ArtifactError::AuthorityResolution { .. })
        ));
    }

    #[test]
    fn injected_precommit_failure_leaves_no_output_or_stage_file() {
        let directory = TempDir::new().unwrap();
        let destination = directory.path().join("output.bin");
        let authority =
            explicit(directory.path()).with_commit_failpoint(CommitFailpoint::AfterStageSync);

        let error = authority
            .commit_new("output.bin", "output", b"bytes", &[])
            .unwrap_err();

        assert!(matches!(error, ArtifactError::InjectedFailure { .. }));
        assert!(!destination.exists());
        assert!(stage_files(directory.path()).is_empty());
    }

    #[test]
    fn staged_corruption_is_detected_before_commit_and_cleaned() {
        let directory = TempDir::new().unwrap();
        let destination = directory.path().join("output.bin");
        let authority =
            explicit(directory.path()).with_commit_failpoint(CommitFailpoint::CorruptStagedBytes);

        let error = authority
            .commit_new("output.bin", "output", b"bytes", &[])
            .unwrap_err();

        assert!(matches!(
            error,
            ArtifactError::StageVerificationMismatch { .. }
        ));
        assert!(!destination.exists());
        assert!(stage_files(directory.path()).is_empty());
    }

    #[test]
    fn injected_postcommit_failure_rolls_back_the_new_artifact() {
        let directory = TempDir::new().unwrap();
        let destination = directory.path().join("output.bin");
        let authority =
            explicit(directory.path()).with_commit_failpoint(CommitFailpoint::AfterCommit);

        let error = authority
            .commit_new("output.bin", "output", b"bytes", &[])
            .unwrap_err();

        assert!(matches!(error, ArtifactError::InjectedFailure { .. }));
        assert!(!destination.exists());
        assert!(stage_files(directory.path()).is_empty());
    }

    #[test]
    fn concurrent_create_new_commits_have_exactly_one_winner() {
        let directory = TempDir::new().unwrap();
        let authority = Arc::new(explicit(directory.path()));
        let barrier = Arc::new(Barrier::new(3));
        let mut threads = Vec::new();
        for bytes in [b"first".as_slice(), b"second".as_slice()] {
            let authority = Arc::clone(&authority);
            let barrier = Arc::clone(&barrier);
            let bytes = bytes.to_vec();
            threads.push(thread::spawn(move || {
                barrier.wait();
                authority.commit_new("winner.bin", "output", &bytes, &[])
            }));
        }
        barrier.wait();

        let results: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(ArtifactError::OutputExists { .. })))
                .count(),
            1
        );
        let winner = results.into_iter().find_map(Result::ok).unwrap();
        assert_eq!(
            digest_bytes(&fs::read(directory.path().join("winner.bin")).unwrap()),
            winner.identity.digest
        );
        assert!(stage_files(directory.path()).is_empty());
    }
}
