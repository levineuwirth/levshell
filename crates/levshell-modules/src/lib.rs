//! Levshell built-in modules.
//!
//! Each top-bar feature (workspace indicator, system telemetry, notification
//! center, command palette, calendar hub, ...) is a [`Module`] implementation
//! that lives here. Modules read from `levshell-data`, subscribe to events on
//! the bus from `levshell-core`, and emit widget state patches that the
//! daemon forwards to the QML shell over `levshell-ipc`.
//!
//! [`Module`]: levshell_core::Module

#![forbid(unsafe_code)]

pub mod anki_due;
pub mod anki_review;
pub mod arxiv_watch;
pub mod clock;
pub mod context_engine;
pub mod proc_sniper;
pub mod context_snapshot;
pub mod escalation;
pub mod focus;
pub mod notifications;
pub mod rubber_duck;
pub mod ideation;
pub mod interruption;
pub mod palette;
pub mod warmup;
pub mod latex_status;
pub mod project_pulse;
pub mod reference_library;
pub mod remote;
pub mod session_timer;
pub mod sway;
pub mod telemetry;
pub mod theme;

pub use anki_due::{AnkiDueModule, ANKI_DUE_WIDGET_ID, ANKI_DUE_WIDGET_TYPE};
pub use anki_review::AnkiReviewModule;
pub use arxiv_watch::{ArxivConfig, ArxivWatchModule, ARXIV_WIDGET_ID, ARXIV_WIDGET_TYPE};
pub use clock::ClockModule;
pub use proc_sniper::ProcessSniperModule;
pub use context_engine::{
    default_context_engine, default_widgets, primary_output_width, ContextEngineModule,
};
pub use context_snapshot::{
    default_contexts_dir, delete_snapshot, list_snapshots, restore_snapshot, save_current,
    ContextSnapshot, ContextSnapshotError, OperationSummary, WindowSnapshot,
};
pub use escalation::{EscalationTracker, TickOutcome};
pub use notifications::{NotificationSender, NotificationsModule, NotifyRustSender};
pub use focus::{FocusModeModule, FocusModeTracker, TriggerAction, TriggerInput, TriggerPhase};
pub use rubber_duck::{
    default_rubber_duck_config_path, RubberDuckConfig, RubberDuckConfigError, RubberDuckModule,
};
pub use ideation::{
    IdeationConfig, IdeationConfigError, IdeationModule, Nudge, NudgeContext, NudgeKind,
    NudgeSelector, NudgeWeights, RecentEntity,
};
pub use interruption::{
    InterruptionCostModule, InterruptionState, InterruptionTracker, MIN_AWAY_SECS as INTERRUPTION_MIN_AWAY_SECS,
    WIDGET_ID as INTERRUPTION_WIDGET_ID, WIDGET_TYPE as INTERRUPTION_WIDGET_TYPE,
};
pub use remote::{
    CommandOutput, GpuDashboardModule, GpuFleetState, GpuHostState, GpuSample, HostConfig,
    HostFile, HostRegistry, HostRegistryError, HostRole, JobsHostState, MockRunner, RemoteError,
    RemoteJobsModule, RemoteJobsState, RemoteRunner, SlurmJob, SshFleetState, SshHostState,
    SshMonitorModule, SshRunner, GPU_WIDGET_ID, GPU_WIDGET_TYPE, JOBS_WIDGET_ID, JOBS_WIDGET_TYPE,
    SSH_WIDGET_ID, SSH_WIDGET_TYPE,
};
pub use latex_status::{LatexStatusModule, LATEX_WIDGET_ID, LATEX_WIDGET_TYPE};
pub use project_pulse::{
    ProjectPulseModule, PROJECT_PULSE_WIDGET_ID, PROJECT_PULSE_WIDGET_TYPE,
};
pub use reference_library::{
    ReferenceLibraryModule, REF_LIBRARY_WIDGET_ID, REF_LIBRARY_WIDGET_TYPE,
};
pub use session_timer::{
    SessionTimerConfig, SessionTimerModule, SESSION_TIMER_WIDGET_ID, SESSION_TIMER_WIDGET_TYPE,
};
pub use theme::{ThemeService, DEFAULT_THEME_NAME};
pub use warmup::{
    default_warmup_config_path, default_warmup_state_path, PersistedWarmupState, TriggerState,
    WarmupConfig, WarmupConfigError, WarmupModule,
};
pub use palette::{
    default_palette_providers, AppLauncherProvider, CalcProvider, NoteSearchProvider, PaletteItem,
    PaletteModule, PaletteProvider, PaletteState, RecentDocsProvider, RefSearchProvider,
    UnicodeProvider, WorkspaceSwitcherProvider, sway_switch_workspace, PALETTE_WIDGET_ID,
    PALETTE_WIDGET_TYPE,
};
pub use sway::{
    SwayWorkspaceModule, WorkspaceIndicatorState, WorkspaceInfo, WORKSPACE_WIDGET_ID,
    WORKSPACE_WIDGET_TYPE,
};
pub use telemetry::{
    BatteryModule, BatteryState, BatteryStatus, CpuModule, CpuSample, CpuState, DiskConfig,
    DiskModule, DiskState, IfaceRate, LinkQuality, MemoryModule, MemoryState, MountUsage,
    NetworkConfig, NetworkModule, NetworkState, PowerProfilesModule, PowerProfileState,
    UPowerWatcherModule, BATTERY_WIDGET_ID, BATTERY_WIDGET_TYPE, CPU_WIDGET_ID, CPU_WIDGET_TYPE,
    DISK_WIDGET_ID, DISK_WIDGET_TYPE, MEMORY_WIDGET_ID, MEMORY_WIDGET_TYPE, NETWORK_WIDGET_ID,
    NETWORK_WIDGET_TYPE, POWER_PROFILE_WIDGET_ID, POWER_PROFILE_WIDGET_TYPE,
};
