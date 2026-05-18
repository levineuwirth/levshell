//! Power-profile switcher (spec §2.3.2 — "switch between power
//! profiles").
//!
//! Talks to `power-profiles-daemon` over the system bus
//! (`net.hadess.PowerProfiles`), the same service GNOME/KDE drive. It
//! exposes the active profile and the platform's available set, and
//! flips it on a widget action — so the bar can offer power-saver /
//! balanced / performance without the user dropping to a terminal.
//!
//! Fail-soft, exactly like [`super::upower`]: no PPD on the bus (a host
//! that uses bare `tlp`/`tuned`, a container without a system bus) →
//! [`ModuleError::Unavailable`] from `start()` and the module parks. The
//! rest of the bar, battery widget included, is unaffected.
//!
//! State: `{ active: "balanced", available: ["power-saver", "balanced",
//! "performance"] }`, ordered coolest→hottest so the widget can cycle
//! predictably.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use levshell_core::{
    Event, EventKind, Module, ModuleError, ModuleResult, WidgetDescriptor,
};
use levshell_ipc::{
    DaemonMessage, EscalationLevel, WidgetPublisher, WidgetStatus, WidgetUpdate,
};
use serde::{Deserialize, Serialize};

pub const POWER_PROFILE_WIDGET_ID: &str = "power-profile";
pub const POWER_PROFILE_WIDGET_TYPE: &str = "power_profile";

const PPD_BUS: &str = "net.hadess.PowerProfiles";
const PPD_PATH: &str = "/net/hadess/PowerProfiles";
const PPD_IFACE: &str = "net.hadess.PowerProfiles";
const ACTIVE_PROP: &str = "ActiveProfile";
const PROFILES_PROP: &str = "Profiles";

/// Re-poll cadence. The active profile can change *without* a widget
/// action — power-profiles-daemon downgrades performance→balanced on
/// battery, and other clients (GNOME) can set it. 20 s keeps the bar
/// honest without busying the bus.
const TICK_INTERVAL: Duration = Duration::from_secs(20);
/// Upper bound for the one-shot D-Bus probe in `start()`. zbus has no
/// built-in call timeout and the runner does not protect `start()`, so
/// this is the only thing standing between a stalled PPD activation and
/// a daemon that never finishes module registration. Generous (a local
/// system-bus round-trip is sub-ms) but finite.
const START_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PowerProfileState {
    pub active: String,
    pub available: Vec<String>,
}

/// Sort the daemon's profile list into the canonical coolest→hottest
/// order so "cycle" is predictable regardless of the order PPD returned
/// them. Unknown profile names (future PPD additions) are kept, appended
/// in their original order after the three well-known ones.
pub fn order_profiles(names: impl IntoIterator<Item = String>) -> Vec<String> {
    const CANONICAL: [&str; 3] = ["power-saver", "balanced", "performance"];
    let names: Vec<String> = names.into_iter().collect();
    let mut ordered: Vec<String> = CANONICAL
        .iter()
        .filter(|c| names.iter().any(|n| n == *c))
        .map(|c| c.to_string())
        .collect();
    for n in &names {
        if !CANONICAL.contains(&n.as_str()) {
            ordered.push(n.clone());
        }
    }
    ordered
}

/// The profile after `current` in `available`, wrapping around. Returns
/// `None` only when `available` is empty. If `current` isn't in the list
/// (raced with an external change), start from the front.
pub fn next_profile(current: &str, available: &[String]) -> Option<String> {
    if available.is_empty() {
        return None;
    }
    let idx = available.iter().position(|p| p == current).unwrap_or(0);
    Some(available[(idx + 1) % available.len()].clone())
}

pub struct PowerProfilesModule {
    publisher: WidgetPublisher,
    conn: Option<zbus::Connection>,
    last: Option<PowerProfileState>,
}

impl PowerProfilesModule {
    pub fn new(publisher: WidgetPublisher) -> Self {
        Self {
            publisher,
            conn: None,
            last: None,
        }
    }

    async fn proxy(conn: &zbus::Connection) -> Result<zbus::Proxy<'_>, zbus::Error> {
        zbus::Proxy::new(conn, PPD_BUS, PPD_PATH, PPD_IFACE).await
    }

    async fn read_state(conn: &zbus::Connection) -> Result<PowerProfileState, zbus::Error> {
        let proxy = Self::proxy(conn).await?;
        let active: String = proxy.get_property(ACTIVE_PROP).await?;
        // `Profiles` is `aa{sv}`; each entry's `Profile` key is the name.
        let raw: Vec<HashMap<String, zbus::zvariant::OwnedValue>> =
            proxy.get_property(PROFILES_PROP).await?;
        let names = raw.iter().filter_map(|m| {
            m.get("Profile")
                .and_then(|v| String::try_from(v.clone()).ok())
        });
        Ok(PowerProfileState {
            active,
            available: order_profiles(names),
        })
    }

    async fn set_profile(conn: &zbus::Connection, profile: &str) -> Result<(), zbus::Error> {
        let proxy = Self::proxy(conn).await?;
        proxy.set_property(ACTIVE_PROP, profile).await?;
        Ok(())
    }

    fn publish(&mut self, state: &PowerProfileState) {
        let value = match serde_json::to_value(state) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "power-profiles: failed to serialize state");
                return;
            }
        };
        let update = WidgetUpdate {
            widget_id: POWER_PROFILE_WIDGET_ID.into(),
            widget_type: POWER_PROFILE_WIDGET_TYPE.into(),
            state: value,
            status: WidgetStatus::Normal,
            escalation: EscalationLevel::Ambient,
        };
        if let Err(e) = self.publisher.try_send(DaemonMessage::WidgetUpdate(update)) {
            tracing::warn!(error = %e, "power-profiles: failed to publish WidgetUpdate");
        }
        self.last = Some(state.clone());
    }

    async fn refresh(&mut self) {
        let Some(conn) = self.conn.clone() else {
            return;
        };
        match Self::read_state(&conn).await {
            Ok(state) => {
                if self.last.as_ref() != Some(&state) {
                    self.publish(&state);
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "power-profiles: re-read failed; keeping last state");
            }
        }
    }
}

#[async_trait]
impl Module for PowerProfilesModule {
    fn name(&self) -> &str {
        "power-profiles"
    }

    fn widgets(&self) -> Vec<WidgetDescriptor> {
        vec![WidgetDescriptor {
            id: POWER_PROFILE_WIDGET_ID.into(),
            widget_type: POWER_PROFILE_WIDGET_TYPE.into(),
        }]
    }

    fn tick_interval(&self) -> Option<Duration> {
        Some(TICK_INTERVAL)
    }

    fn subscribed_events(&self) -> Vec<EventKind> {
        vec![EventKind::WidgetActionReceived]
    }

    async fn start(&mut self) -> ModuleResult<()> {
        // The module runner timeout-wraps tick()/on_event() but NOT
        // start() (runner.rs). zbus has no built-in method-call timeout,
        // so an activatable-but-stalled power-profiles-daemon (or a
        // wedged system bus) would hang here forever — and because
        // modules register sequentially, that stalls the whole daemon's
        // registration and the bar comes up empty. Bound it ourselves so
        // the documented fail-soft ("the module parks") holds on the
        // hang path too, not just the fast-error path.
        let connect = async {
            let conn = zbus::Connection::system().await.map_err(|e| {
                ModuleError::Unavailable(format!(
                    "no system bus for power-profiles-daemon: {e}"
                ))
            })?;
            let state = Self::read_state(&conn).await.map_err(|e| {
                ModuleError::Unavailable(format!(
                    "power-profiles-daemon unreachable: {e}"
                ))
            })?;
            Ok::<_, ModuleError>((conn, state))
        };
        let (conn, state) = tokio::time::timeout(START_TIMEOUT, connect)
            .await
            .map_err(|_| {
                ModuleError::Unavailable(
                    "power-profiles-daemon: D-Bus probe timed out".into(),
                )
            })??;
        self.conn = Some(conn);
        self.publish(&state);
        Ok(())
    }

    /// The widget sends `power-profile set {profile}` to jump straight to
    /// a profile, or `power-profile cycle {}` to advance to the next one
    /// in canonical order.
    async fn on_event(&mut self, event: &Event) -> ModuleResult<()> {
        let Event::WidgetActionReceived {
            widget_id,
            action,
            data,
        } = event
        else {
            return Ok(());
        };
        if widget_id != POWER_PROFILE_WIDGET_ID {
            return Ok(());
        }
        let Some(conn) = self.conn.clone() else {
            return Ok(());
        };

        let target = match action.as_str() {
            "set" => serde_json::from_str::<serde_json::Value>(data)
                .ok()
                .and_then(|v| {
                    v.get("profile").and_then(|p| p.as_str()).map(str::to_owned)
                }),
            "cycle" => self.last.as_ref().and_then(|s| {
                next_profile(&s.active, &s.available)
            }),
            _ => None,
        };

        if let Some(profile) = target {
            // Only forward a profile PPD actually advertised. `cycle`
            // already derives from `available`; `set` carries verbatim
            // widget-action JSON, so without this an arbitrary string
            // would flow into the privileged `set_property` call. PPD
            // would reject it anyway, but unvalidated external input
            // shouldn't reach a privileged D-Bus write.
            let allowed = self
                .last
                .as_ref()
                .is_some_and(|s| s.available.iter().any(|p| p == &profile));
            if !allowed {
                tracing::warn!(
                    profile = %profile,
                    "power-profiles: ignoring set to a profile not in PPD's advertised set"
                );
                return Ok(());
            }
            match Self::set_profile(&conn, &profile).await {
                Ok(()) => {
                    tracing::info!(profile = %profile, "power-profiles: switched");
                    self.refresh().await;
                }
                Err(e) => {
                    tracing::warn!(profile = %profile, error = %e, "power-profiles: set failed");
                }
            }
        }
        Ok(())
    }

    async fn tick(&mut self) -> ModuleResult<()> {
        self.refresh().await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_profiles_uses_canonical_order() {
        let got = order_profiles(
            ["performance", "power-saver", "balanced"]
                .map(String::from),
        );
        assert_eq!(got, vec!["power-saver", "balanced", "performance"]);
    }

    #[test]
    fn order_profiles_keeps_unknown_profiles_after_known() {
        let got = order_profiles(
            ["balanced", "turbo", "power-saver"].map(String::from),
        );
        assert_eq!(got, vec!["power-saver", "balanced", "turbo"]);
    }

    #[test]
    fn order_profiles_handles_partial_sets() {
        // Some laptops expose only two profiles.
        let got = order_profiles(["balanced", "power-saver"].map(String::from));
        assert_eq!(got, vec!["power-saver", "balanced"]);
    }

    #[test]
    fn next_profile_wraps_around() {
        let av: Vec<String> = ["power-saver", "balanced", "performance"]
            .map(String::from)
            .into();
        assert_eq!(next_profile("power-saver", &av).as_deref(), Some("balanced"));
        assert_eq!(next_profile("balanced", &av).as_deref(), Some("performance"));
        assert_eq!(
            next_profile("performance", &av).as_deref(),
            Some("power-saver")
        );
    }

    #[test]
    fn next_profile_unknown_current_starts_from_front() {
        let av: Vec<String> = ["power-saver", "balanced"].map(String::from).into();
        assert_eq!(next_profile("bogus", &av).as_deref(), Some("balanced"));
    }

    #[test]
    fn next_profile_empty_is_none() {
        assert_eq!(next_profile("balanced", &[]), None);
    }
}
