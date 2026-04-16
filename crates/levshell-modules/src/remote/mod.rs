//! Remote / SSH module family (spec §2.5, §6.2 item 4).
//!
//! Three modules share the infrastructure in this submodule:
//!
//! - [`SshMonitorModule`] — SSH Connection Dashboard (§2.5.1). Probes
//!   each configured host on a short interval and publishes
//!   [`SshFleetState`].
//! - [`GpuDashboardModule`] — GPU Utilization Dashboard (§2.5.4).
//!   Polls `nvidia-smi` and publishes [`GpuFleetState`].
//! - [`RemoteJobsModule`] — Remote Job Monitor (§2.5.3). Polls SLURM
//!   `squeue` and publishes [`RemoteJobsState`].
//!
//! All three consume the same [`HostRegistry`] (read from
//! `~/.config/levshell/hosts/*.toml`) and the same [`RemoteRunner`]
//! trait (production [`SshRunner`]; [`MockRunner`] for tests). A host
//! opts into each module via its `roles` array in the TOML file.

pub mod gpu;
pub mod host;
pub mod jobs;
pub mod runner;
pub mod ssh_monitor;

pub use gpu::{
    GpuDashboardModule, GpuFleetState, GpuHostState, GpuSample, GPU_WIDGET_ID, GPU_WIDGET_TYPE,
};
pub use host::{HostConfig, HostFile, HostRegistry, HostRegistryError, HostRole};
pub use jobs::{
    JobsHostState, RemoteJobsModule, RemoteJobsState, SlurmJob, JOBS_WIDGET_ID, JOBS_WIDGET_TYPE,
};
pub use runner::{CommandOutput, MockRunner, RemoteError, RemoteRunner, SshRunner};
pub use ssh_monitor::{
    SshFleetState, SshHostState, SshMonitorModule, SSH_WIDGET_ID, SSH_WIDGET_TYPE,
};
