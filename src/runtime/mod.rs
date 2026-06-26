//! Container execution runtime (SPECS §31).
//!
//! Running an agent in a container is just a different command line handed to
//! the existing [`crate::contracts::PtyBackend`]: `run`/`attach`/`exec` are
//! plain `podman` argv. This module owns:
//!
//! - [`spec`] — the [`spec::ContainerSpec`] data the builders consume.
//! - [`container`] — **pure** argv builders (`build_run_args`/`attach`/`exec`).
//! - [`guards`] — **pure** hard security guardrails over a built argv.
//! - [`name`] — deterministic container naming + labels.
//! - [`image`] — image tag/hash/Containerfile logic + the `ensure_image` flow.
//!
//! The non-interactive control plane (build/inspect/remove/list) lives behind
//! the [`crate::contracts::ContainerRuntime`] trait, not here.

pub mod container;
pub mod guards;
pub mod image;
pub mod name;
pub mod podman;
pub mod spec;

pub use podman::PodmanCli;

pub use container::{build_attach_args, build_exec_args, build_run_args};
pub use guards::enforce_guardrails;
pub use name::{container_name, repo_hash, LABEL_REPO, LABEL_TAB};
pub use spec::{ContainerSpec, ResolvedAuthMount};
