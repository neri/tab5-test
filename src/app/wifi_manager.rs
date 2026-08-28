//! One owner for the ESP-Hosted session and the IPv4 stack.
//!
//! Stage 3 of `docs/WIFI_REFACTOR_PLAN.md` puts the two optional resources
//! beside the policy that explains why they exist. Stage 4 adds a bounded
//! transition history and retry policy. Stage 5 retains menu credentials only
//! in RAM while that connection is active, so a lost station or C6 link can be
//! rebuilt without user input.

use alloc::vec::Vec;

use crate::{net, sdio, tick, wifi};

use super::wifi_retry::{self, Decision};

const ASSOCIATION_TIMEOUT_MS: u64 = 20_000;
const DHCP_TIMEOUT_MS: u64 = 15_000;
const REPLACEMENT_DISCONNECT_TIMEOUT_MS: u32 = 3_000;
const STARTUP_RETRY_DELAY_MS: u32 = 500;
const STARTUP_MAX_ATTEMPTS: u32 = 3;
const HISTORY_LIMIT: usize = 16;
const NOTICE_LIMIT: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConnectionSource {
    ShellManual,
    MenuManaged,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum IpPolicy {
    Unconfigured,
    Dhcp,
    Static,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ProfileChoice {
    ConnectOnce,
    SaveAndAutoConnect,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ProfileSaveState {
    NotRequested,
    Pending,
    Saved,
    Failed,
}

#[derive(Clone, Copy)]
pub struct Association {
    pub ssid: [u8; wifi::station::SSID_MAX_BYTES],
    pub ssid_length: usize,
    pub channel: u32,
}

impl Association {
    pub fn ssid(&self) -> &[u8] {
        &self.ssid[..self.ssid_length]
    }
}

#[derive(Clone, Copy)]
pub enum Failure {
    Disabled,
    LinkBringUp,
    StartRpc,
    StartStatus(i32),
    ConnectRpc,
    ConnectStatus(i32),
    Disconnected(u32),
    AssociationTimedOut,
    TickUnavailable,
    MacRpc,
    MacStatus(i32),
    LinkLost,
    ConfigRpc,
    ConfigStatus(i32),
    StorageRpc,
    StorageStatus(i32),
    DisconnectRpc,
    DisconnectStatus(i32),
    DisconnectTimedOut,
    ModeRpc,
    ModeStatus(i32),
    StopRpc,
    StopStatus(i32),
}

#[derive(Clone, Copy)]
pub enum State {
    Off,
    LinkDown,
    Idle,
    Associating {
        deadline_ms: u64,
        attempt: u32,
        generation: u32,
    },
    RetryWaiting {
        deadline_ms: u64,
        next_attempt: u32,
        generation: u32,
        failure: Failure,
    },
    NeedsPassword(u32),
    Associated(Association),
    RequestingDhcp {
        association: Association,
        deadline_ms: u64,
    },
    AssociatedNoLease(Association),
    Online(Association),
    Failed(Failure),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Off,
    LinkDown,
    Idle,
    Associating,
    RetryWaiting,
    NeedsPassword,
    Associated,
    RequestingDhcp,
    AssociatedNoLease,
    Online,
    Failed,
}

impl State {
    pub fn phase(self) -> Phase {
        match self {
            Self::Off => Phase::Off,
            Self::LinkDown => Phase::LinkDown,
            Self::Idle => Phase::Idle,
            Self::Associating { .. } => Phase::Associating,
            Self::RetryWaiting { .. } => Phase::RetryWaiting,
            Self::NeedsPassword(_) => Phase::NeedsPassword,
            Self::Associated(_) => Phase::Associated,
            Self::RequestingDhcp { .. } => Phase::RequestingDhcp,
            Self::AssociatedNoLease(_) => Phase::AssociatedNoLease,
            Self::Online(_) => Phase::Online,
            Self::Failed(_) => Phase::Failed,
        }
    }
}

#[derive(Clone, Copy)]
pub enum Cause {
    Enabled,
    Disabled,
    Manual,
    LinkReady,
    ConnectRequested,
    Connected,
    Disconnected(u32),
    AssociationTimeout,
    RetryScheduled(u32),
    RetryTimer,
    StaleEvent(u32),
    RpcFailed,
    RpcStatus(i32),
    DhcpStarted,
    DhcpConfigured,
    DhcpTimeout,
    DhcpLost,
    StableConnection,
    LinkLost,
    StartupProfile,
    ProfileSaved,
    ProfileSaveFailed,
    ProfileForgotten,
    ReplacementDisconnected,
    DisconnectTimeout,
}

#[derive(Clone, Copy)]
pub struct Transition {
    pub at_ms: u64,
    pub from: Phase,
    pub to: Phase,
    pub cause: Cause,
    pub attempt: u32,
    pub generation: u32,
}

#[derive(Clone, Copy)]
pub enum Notice {
    Disconnected(u32),
    Reassociated,
    LinkLost,
}

struct Credentials {
    ssid: [u8; wifi::station::SSID_MAX_BYTES],
    ssid_length: usize,
    password: [u8; wifi::station::PASSWORD_MAX_BYTES],
    password_length: usize,
}

impl Credentials {
    fn new(ssid: &[u8], password: &[u8]) -> Self {
        let ssid_length = ssid.len().min(wifi::station::SSID_MAX_BYTES);
        let password_length = password.len().min(wifi::station::PASSWORD_MAX_BYTES);
        let mut credentials = Self {
            ssid: [0; wifi::station::SSID_MAX_BYTES],
            ssid_length,
            password: [0; wifi::station::PASSWORD_MAX_BYTES],
            password_length,
        };
        credentials.ssid[..ssid_length].copy_from_slice(&ssid[..ssid_length]);
        credentials.password[..password_length].copy_from_slice(&password[..password_length]);
        credentials
    }

    fn erase(&mut self) {
        zeroize(&mut self.ssid);
        zeroize(&mut self.password);
        self.ssid_length = 0;
        self.password_length = 0;
    }
}

impl Drop for Credentials {
    fn drop(&mut self) {
        self.erase();
    }
}

pub struct Manager {
    enabled: bool,
    saved_profile_exists: bool,
    session: Option<wifi::Rpc>,
    stack: Option<net::Stack>,
    source: Option<ConnectionSource>,
    ip_policy: IpPolicy,
    state: State,
    notices: Vec<Notice>,
    credentials: Option<Credentials>,
    attempt: u32,
    generation: u32,
    history: Vec<Transition>,
    connected_since_ms: Option<u64>,
    reconnecting: bool,
    save_pending: bool,
    profile_save_state: ProfileSaveState,
    profile_save_attempts: u32,
    profile_save_failures: u32,
    profile_forgets: u32,
    startup_retry_policy: bool,
}

impl Manager {
    pub const fn new() -> Self {
        Self {
            enabled: true,
            saved_profile_exists: false,
            session: None,
            stack: None,
            source: None,
            ip_policy: IpPolicy::Unconfigured,
            state: State::LinkDown,
            notices: Vec::new(),
            credentials: None,
            attempt: 0,
            generation: 0,
            history: Vec::new(),
            connected_since_ms: None,
            reconnecting: false,
            save_pending: false,
            profile_save_state: ProfileSaveState::NotRequested,
            profile_save_attempts: 0,
            profile_save_failures: 0,
            profile_forgets: 0,
            startup_retry_policy: false,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn has_saved_profile(&self) -> bool {
        self.saved_profile_exists
    }

    pub fn history(&self) -> &[Transition] {
        &self.history
    }

    pub fn session_mut(&mut self) -> Option<&mut wifi::Rpc> {
        self.session.as_mut()
    }

    pub fn stack(&self) -> Option<&net::Stack> {
        self.stack.as_ref()
    }

    pub fn profile_save_state(&self) -> ProfileSaveState {
        self.profile_save_state
    }

    pub fn profile_diagnostics(&self) -> (u32, u32, u32) {
        (
            self.profile_save_attempts,
            self.profile_save_failures,
            self.profile_forgets,
        )
    }

    /// Scrubs the volatile menu credential before an HP-core-only reboot.
    /// The C6 session remains available for the caller's best-effort deauth.
    pub fn erase_credentials(&mut self) {
        self.clear_credentials();
    }

    /// Temporary compatibility boundary for existing synchronous commands.
    /// Storage remains private to this owner; callers borrow both options
    /// together and cannot leave an old stack behind when `clear_link` is used.
    pub fn options_mut(&mut self) -> (&mut Option<wifi::Rpc>, &mut Option<net::Stack>) {
        (&mut self.session, &mut self.stack)
    }

    pub fn clear_link(&mut self) {
        self.clear_credentials();
        self.session = None;
        self.stack = None;
        self.source = None;
        self.ip_policy = IpPolicy::Unconfigured;
        self.attempt = 0;
        self.connected_since_ms = None;
        self.reconnecting = false;
        self.save_pending = false;
        self.profile_save_state = ProfileSaveState::NotRequested;
        self.transition(
            if self.enabled {
                State::LinkDown
            } else {
                State::Off
            },
            Cause::Manual,
        );
    }

    pub fn mark_disconnected(&mut self) {
        self.clear_credentials();
        self.stack = None;
        self.source = None;
        self.ip_policy = IpPolicy::Unconfigured;
        self.attempt = 0;
        self.connected_since_ms = None;
        self.reconnecting = false;
        self.save_pending = false;
        self.profile_save_state = ProfileSaveState::NotRequested;
        let state = if !self.enabled {
            State::Off
        } else if self.session.is_some() {
            State::Idle
        } else {
            State::LinkDown
        };
        self.transition(state, Cause::Manual);
    }

    pub fn begin_shell_attempt(&mut self) {
        self.clear_credentials();
        self.stack = None;
        self.source = Some(ConnectionSource::ShellManual);
        self.ip_policy = IpPolicy::Unconfigured;
        self.attempt = 1;
        self.connected_since_ms = None;
        self.reconnecting = false;
        self.save_pending = false;
        self.profile_save_state = ProfileSaveState::NotRequested;
        self.generation = self.generation.wrapping_add(1).max(1);
        self.transition(
            State::Associating {
                deadline_ms: tick::now_ms().saturating_add(ASSOCIATION_TIMEOUT_MS),
                attempt: self.attempt,
                generation: self.generation,
            },
            Cause::ConnectRequested,
        );
    }

    pub fn mark_shell_associated(&mut self, association: Association) {
        self.connected_since_ms = Some(tick::now_ms());
        self.transition(State::Associated(association), Cause::Connected);
    }

    pub fn mark_failed(&mut self, failure: Failure) {
        self.clear_credentials();
        self.stack = None;
        self.connected_since_ms = None;
        self.reconnecting = false;
        self.save_pending = false;
        self.transition(State::Failed(failure), cause_for_failure(failure));
    }

    pub fn set_ip_policy(&mut self, policy: IpPolicy) {
        self.ip_policy = policy;
        let association = match self.state {
            State::Associated(association)
            | State::Online(association)
            | State::RequestingDhcp { association, .. }
            | State::AssociatedNoLease(association) => Some(association),
            _ => None,
        };
        let Some(association) = association else {
            return;
        };
        let state = match policy {
            IpPolicy::Unconfigured => State::Associated(association),
            IpPolicy::Dhcp if self.stack.as_ref().is_some_and(net::Stack::has_address) => {
                State::Online(association)
            }
            IpPolicy::Dhcp => State::RequestingDhcp {
                association,
                deadline_ms: tick::now_ms().saturating_add(DHCP_TIMEOUT_MS),
            },
            IpPolicy::Static if self.stack.as_ref().is_some_and(net::Stack::has_address) => {
                State::Online(association)
            }
            IpPolicy::Static => State::Associated(association),
        };
        self.transition(state, Cause::Manual);
    }

    pub fn take_notices(&mut self) -> Vec<Notice> {
        core::mem::take(&mut self.notices)
    }

    /// Uses the splash screen's short, finite retry policy.
    pub fn begin_startup_retry_policy(&mut self) {
        self.startup_retry_policy = true;
    }

    /// Returns retry scheduling to the ordinary reason-specific policy.
    ///
    /// If Escape leaves while a short startup timer is pending, reschedule
    /// that same failure from now instead of carrying the 500 ms deadline
    /// into the console or Wi-Fi menu.
    pub fn finish_startup_retry_policy(&mut self) {
        self.startup_retry_policy = false;
        let State::RetryWaiting {
            next_attempt,
            generation,
            failure,
            ..
        } = self.state
        else {
            return;
        };
        let Decision::RetryAfter(delay_ms) = retry_decision(failure, self.attempt.max(1)) else {
            return;
        };
        self.transition(
            State::RetryWaiting {
                deadline_ms: tick::now_ms().saturating_add(delay_ms as u64),
                next_attempt,
                generation,
                failure,
            },
            Cause::RetryScheduled(delay_ms),
        );
    }

    /// Enables or disables the whole Wi-Fi subsystem. `Ok(true)` means an
    /// ON operation found a saved profile and started managed association.
    pub fn set_enabled(&mut self, enabled: bool) -> Result<bool, Failure> {
        if enabled == self.enabled {
            return Ok(false);
        }
        if enabled {
            let result = self.enable_from_persistent_state();
            if result.is_err() {
                // Enabling is transactional from the user's point of view:
                // if any RPC fails, return to the physical OFF state and
                // best-effort restore persistent NULL mode.
                let _ = self.disable_and_power_down();
            }
            result
        } else {
            self.disable_and_power_down()?;
            Ok(false)
        }
    }

    fn enable_from_persistent_state(&mut self) -> Result<bool, Failure> {
        let mut rpc = self.open_initialized_rpc()?;
        let config = match wifi::station::station_config(&mut rpc) {
            Some((0, config)) => config,
            Some((status, _)) => return Err(Failure::ConfigStatus(status)),
            None => return Err(Failure::ConfigRpc),
        };
        self.saved_profile_exists = !config.ssid().is_empty();

        match wifi::station::set_storage(&mut rpc, wifi::station::Storage::Flash) {
            Some(0) => {}
            Some(status) => return Err(Failure::StorageStatus(status)),
            None => return Err(Failure::StorageRpc),
        }
        match wifi::station::set_mode(&mut rpc, wifi::station::WIFI_MODE_STA) {
            Some(0) => {}
            Some(status) => return Err(Failure::ModeStatus(status)),
            None => return Err(Failure::ModeRpc),
        }
        if config.disabled_without_profile() {
            match wifi::station::set_station_config(&mut rpc, &[], &[]) {
                Some(0) => {}
                Some(status) => return Err(Failure::ConfigStatus(status)),
                None => return Err(Failure::ConfigRpc),
            }
        }
        match wifi::station::start_initialized(&mut rpc) {
            Some(0) => {}
            Some(status) => return Err(Failure::StartStatus(status)),
            None => return Err(Failure::StartRpc),
        }
        let _ = wifi::station::set_storage(&mut rpc, wifi::station::Storage::Ram);

        self.enabled = true;
        self.session = Some(rpc);
        self.stack = None;
        self.source = None;
        self.ip_policy = IpPolicy::Unconfigured;
        self.attempt = 0;
        self.connected_since_ms = None;
        self.reconnecting = false;
        self.profile_save_state = ProfileSaveState::NotRequested;
        self.transition(State::Idle, Cause::Enabled);

        if config.ssid().is_empty() {
            return Ok(false);
        }
        self.credentials = Some(Credentials::new(config.ssid(), config.password()));
        drop(config);
        self.source = Some(ConnectionSource::MenuManaged);
        self.ip_policy = IpPolicy::Dhcp;
        self.attempt = 1;
        self.profile_save_state = ProfileSaveState::Saved;
        self.record_only(Cause::StartupProfile);
        self.send_pending_connect()?;
        Ok(true)
    }

    fn disable_and_power_down(&mut self) -> Result<(), Failure> {
        let was_started = self.session.as_ref().is_some_and(wifi::Rpc::is_alive);
        let mut rpc = match self.session.take() {
            Some(rpc) if rpc.is_alive() => rpc,
            _ => match self.open_initialized_rpc() {
                Ok(rpc) => rpc,
                Err(failure) => {
                    self.clear_credentials();
                    self.stack = None;
                    self.source = None;
                    self.ip_policy = IpPolicy::Unconfigured;
                    self.enabled = false;
                    sdio::power_down_c6();
                    self.transition(State::Off, cause_for_failure(failure));
                    return Err(failure);
                }
            },
        };

        self.generation = self.generation.wrapping_add(1).max(1);
        self.clear_credentials();
        self.stack = None;
        self.source = None;
        self.ip_policy = IpPolicy::Unconfigured;
        self.attempt = 0;
        self.connected_since_ms = None;
        self.reconnecting = false;
        self.profile_save_state = ProfileSaveState::NotRequested;

        // Leaving the AP is best-effort: even if the event is lost, turning
        // the radio off and removing C6 power still fulfils the current-boot
        // OFF contract.
        let _ = rpc.take_events();
        if wifi::station::disconnect(&mut rpc) == Some(0) {
            let _ =
                wifi::station::wait_for_disconnection(&mut rpc, REPLACEMENT_DISCONNECT_TIMEOUT_MS);
        }

        let persisted = (|| {
            match wifi::station::set_storage(&mut rpc, wifi::station::Storage::Flash) {
                Some(0) => {}
                Some(status) => return Err(Failure::StorageStatus(status)),
                None => return Err(Failure::StorageRpc),
            }
            if !self.saved_profile_exists {
                match wifi::station::set_mode(&mut rpc, wifi::station::WIFI_MODE_STA) {
                    Some(0) => {}
                    Some(status) => return Err(Failure::ModeStatus(status)),
                    None => return Err(Failure::ModeRpc),
                }
                match wifi::station::set_disabled_without_profile(&mut rpc) {
                    Some(0) => {}
                    Some(status) => return Err(Failure::ConfigStatus(status)),
                    None => return Err(Failure::ConfigRpc),
                }
            }
            if was_started {
                match wifi::station::stop(&mut rpc) {
                    Some(0) => {}
                    Some(status) => return Err(Failure::StopStatus(status)),
                    None => return Err(Failure::StopRpc),
                }
            }
            match wifi::station::set_mode(&mut rpc, wifi::station::WIFI_MODE_NULL) {
                Some(0) => Ok(()),
                Some(status) => Err(Failure::ModeStatus(status)),
                None => Err(Failure::ModeRpc),
            }
        })();

        drop(rpc);
        sdio::power_down_c6();
        match persisted {
            Ok(()) => {
                self.enabled = false;
                self.transition(State::Off, Cause::Disabled);
                Ok(())
            }
            Err(failure) => {
                // Current-boot OFF still wins, but the caller must know that
                // the next power cycle may not retain it.
                self.enabled = false;
                self.transition(State::Off, cause_for_failure(failure));
                Err(failure)
            }
        }
    }

    /// Returns a low-level diagnostic which temporarily powered the C6 while
    /// Wi-Fi was OFF back to the physical OFF state.
    pub fn finish_disabled_diagnostic(&mut self) {
        if self.enabled {
            return;
        }
        self.session = None;
        self.stack = None;
        sdio::power_down_c6();
        self.transition(State::Off, Cause::Disabled);
    }

    pub fn ensure_station(&mut self) -> Result<&mut wifi::Rpc, Failure> {
        if !self.enabled {
            return Err(Failure::Disabled);
        }
        if self.session.as_ref().is_some_and(|rpc| !rpc.is_alive()) {
            self.session = None;
            self.stack = None;
            self.transition(State::Failed(Failure::LinkLost), Cause::LinkLost);
        }
        if self.session.is_none() {
            let mut rpc = self.open_initialized_rpc()?;
            match wifi::station::start_initialized(&mut rpc) {
                Some(0) => {}
                Some(status) => {
                    self.transition(
                        State::Failed(Failure::StartStatus(status)),
                        Cause::RpcStatus(status),
                    );
                    return Err(Failure::StartStatus(status));
                }
                None => {
                    self.transition(State::Failed(Failure::StartRpc), Cause::RpcFailed);
                    return Err(Failure::StartRpc);
                }
            }
            self.session = Some(rpc);
            self.transition(State::Idle, Cause::LinkReady);
        }
        self.session.as_mut().ok_or(Failure::LinkBringUp)
    }

    fn open_initialized_rpc(&mut self) -> Result<wifi::Rpc, Failure> {
        let Some((transport, _)) = wifi::bring_up() else {
            self.transition(State::Failed(Failure::LinkBringUp), Cause::RpcFailed);
            return Err(Failure::LinkBringUp);
        };
        let mut rpc = wifi::Rpc::new(transport);
        match wifi::station::initialize(&mut rpc) {
            Some(0) => Ok(rpc),
            Some(status) => {
                self.transition(
                    State::Failed(Failure::StartStatus(status)),
                    Cause::RpcStatus(status),
                );
                Err(Failure::StartStatus(status))
            }
            None => {
                self.transition(State::Failed(Failure::StartRpc), Cause::RpcFailed);
                Err(Failure::StartRpc)
            }
        }
    }

    /// Loads the profile which C6 NVS supplied at startup and begins the same
    /// managed association/DHCP path as the menu without opening any UI.
    pub fn begin_startup_auto_connect(&mut self) -> Result<bool, Failure> {
        self.clear_credentials();
        self.stack = None;
        self.source = None;
        self.ip_policy = IpPolicy::Unconfigured;
        self.attempt = 0;
        self.connected_since_ms = None;
        self.reconnecting = false;
        self.save_pending = false;
        self.profile_save_state = ProfileSaveState::NotRequested;

        let mut rpc = self.open_initialized_rpc()?;
        let mode = match wifi::station::mode(&mut rpc) {
            Some((0, mode)) => mode,
            Some((status, _)) => {
                let failure = Failure::ModeStatus(status);
                self.transition(State::Failed(failure), Cause::RpcStatus(status));
                return Err(failure);
            }
            None => {
                let failure = Failure::ModeRpc;
                self.transition(State::Failed(failure), Cause::RpcFailed);
                return Err(failure);
            }
        };
        let config = match wifi::station::station_config(&mut rpc) {
            Some((0, config)) => config,
            Some((status, _)) => {
                let failure = Failure::ConfigStatus(status);
                self.transition(State::Failed(failure), Cause::RpcStatus(status));
                return Err(failure);
            }
            None => {
                let failure = Failure::ConfigRpc;
                self.transition(State::Failed(failure), Cause::RpcFailed);
                return Err(failure);
            }
        };

        self.saved_profile_exists = !config.ssid().is_empty();
        let disabled = mode == wifi::station::WIFI_MODE_NULL
            && (self.saved_profile_exists || config.disabled_without_profile());
        if disabled {
            self.enabled = false;
            drop(config);
            drop(rpc);
            sdio::power_down_c6();
            self.transition(State::Off, Cause::Disabled);
            return Ok(false);
        }

        match wifi::station::start_initialized(&mut rpc) {
            Some(0) => {}
            Some(status) => {
                let failure = Failure::StartStatus(status);
                self.transition(State::Failed(failure), Cause::RpcStatus(status));
                return Err(failure);
            }
            None => {
                let failure = Failure::StartRpc;
                self.transition(State::Failed(failure), Cause::RpcFailed);
                return Err(failure);
            }
        }
        self.enabled = true;
        self.session = Some(rpc);
        self.transition(State::Idle, Cause::LinkReady);

        if config.ssid().is_empty() {
            if let Some(rpc) = self.session.as_mut() {
                let _ = wifi::station::set_storage(rpc, wifi::station::Storage::Ram);
            }
            return Ok(false);
        }

        self.credentials = Some(Credentials::new(config.ssid(), config.password()));
        drop(config);
        self.source = Some(ConnectionSource::MenuManaged);
        self.ip_policy = IpPolicy::Dhcp;
        self.attempt = 1;
        self.profile_save_state = ProfileSaveState::Saved;
        self.record_only(Cause::StartupProfile);
        self.send_pending_connect()?;
        Ok(true)
    }

    /// Starts a menu-managed association. The credential remains in RAM while
    /// this connection is active so Stage 5 can recover a lost station or C6
    /// link. A save request is committed to C6 NVS only after association.
    pub fn begin_menu_connect(
        &mut self,
        ssid: &[u8],
        password: &[u8],
        profile: ProfileChoice,
    ) -> Result<(), Failure> {
        self.prepare_connection_replacement()?;
        self.source = Some(ConnectionSource::MenuManaged);
        self.ip_policy = IpPolicy::Dhcp;
        self.credentials = Some(Credentials::new(ssid, password));
        self.attempt = 1;
        self.connected_since_ms = None;
        self.reconnecting = false;
        self.save_pending = profile == ProfileChoice::SaveAndAutoConnect;
        self.profile_save_state = if self.save_pending {
            ProfileSaveState::Pending
        } else {
            ProfileSaveState::NotRequested
        };
        self.send_pending_connect()
    }

    /// Cancels the currently managed association before a menu or CLI request
    /// installs another station configuration. ESP-IDF rejects or races a
    /// second connect request while the first association is still active,
    /// so replacement is a disconnect-and-wait operation rather than just a
    /// local state reset.
    pub fn prepare_connection_replacement(&mut self) -> Result<(), Failure> {
        if !self.enabled {
            return Err(Failure::Disabled);
        }
        let should_disconnect = self.session.as_ref().is_some_and(wifi::Rpc::is_alive)
            && (self.connected_since_ms.is_some()
                || matches!(
                    self.state,
                    State::Associating { .. }
                        | State::Associated(_)
                        | State::RequestingDhcp { .. }
                        | State::AssociatedNoLease(_)
                        | State::Online(_)
                ));

        // Invalidate retry timers and stop servicing the old IP stack before
        // asking the C6 to leave the AP. The saved NVS profile is untouched.
        self.generation = self.generation.wrapping_add(1).max(1);
        self.clear_credentials();
        self.stack = None;
        self.source = None;
        self.ip_policy = IpPolicy::Unconfigured;
        self.attempt = 0;
        self.connected_since_ms = None;
        self.reconnecting = false;
        self.profile_save_state = ProfileSaveState::NotRequested;

        if !should_disconnect {
            return Ok(());
        }

        let result = {
            let Some(rpc) = self.session.as_mut() else {
                return Ok(());
            };
            // Remove already-collected events from the association being
            // replaced before issuing the disconnect RPC.
            let _ = rpc.take_events();
            match wifi::station::disconnect(rpc) {
                Some(0) => match wifi::station::wait_for_disconnection(
                    rpc,
                    REPLACEMENT_DISCONNECT_TIMEOUT_MS,
                ) {
                    Some(_) => Ok(()),
                    None => Err(Failure::DisconnectTimedOut),
                },
                Some(status) => Err(Failure::DisconnectStatus(status)),
                None => Err(Failure::DisconnectRpc),
            }
        };

        match result {
            Ok(()) => {
                // Any event retained while waiting belongs to the old
                // generation too.
                if let Some(rpc) = self.session.as_mut() {
                    let _ = rpc.take_events();
                }
                self.transition(State::Idle, Cause::ReplacementDisconnected);
                Ok(())
            }
            Err(failure) => {
                self.transition(State::Failed(failure), cause_for_failure(failure));
                Err(failure)
            }
        }
    }

    fn send_pending_connect(&mut self) -> Result<(), Failure> {
        let Some(credentials) = self.credentials.as_ref() else {
            let failure = Failure::ConnectRpc;
            self.transition(State::Failed(failure), Cause::RpcFailed);
            return Err(failure);
        };
        // Avoid borrowing the credential store across link bring-up and the
        // RPC call. Both local copies are scrubbed immediately afterwards.
        let mut ssid = credentials.ssid;
        let ssid_length = credentials.ssid_length;
        let mut password = credentials.password;
        let password_length = credentials.password_length;
        self.generation = self.generation.wrapping_add(1).max(1);
        let generation = self.generation;

        let result = match self.ensure_station() {
            Ok(rpc) => {
                // Events already queued belong to the previous attempt. A
                // new generation never consumes them as its own outcome.
                let _ = rpc.take_events();
                match wifi::station::set_storage(rpc, wifi::station::Storage::Ram) {
                    Some(0) => Ok(wifi::station::connect(
                        rpc,
                        &ssid[..ssid_length],
                        &password[..password_length],
                    )),
                    Some(status) => Err(Failure::StorageStatus(status)),
                    None => Err(Failure::StorageRpc),
                }
            }
            Err(failure) => {
                zeroize(&mut ssid);
                zeroize(&mut password);
                let decision = match failure {
                    Failure::StartStatus(status) => {
                        wifi_retry::after_rpc_status(status, self.attempt)
                    }
                    _ => wifi_retry::after_rpc_failure(self.attempt),
                };
                return self.apply_retry_decision(failure, decision);
            }
        };
        zeroize(&mut ssid);
        zeroize(&mut password);

        match result {
            Ok(Some(0)) => {
                self.transition(
                    State::Associating {
                        deadline_ms: tick::now_ms().saturating_add(ASSOCIATION_TIMEOUT_MS),
                        attempt: self.attempt,
                        generation,
                    },
                    if self.attempt == 1 && !self.reconnecting {
                        Cause::ConnectRequested
                    } else {
                        Cause::RetryTimer
                    },
                );
                Ok(())
            }
            Ok(Some(status)) => {
                let failure = Failure::ConnectStatus(status);
                self.apply_retry_decision(
                    failure,
                    wifi_retry::after_rpc_status(status, self.attempt),
                )
            }
            Ok(None) => {
                let failure = Failure::ConnectRpc;
                self.apply_retry_decision(failure, wifi_retry::after_rpc_failure(self.attempt))
            }
            Err(failure) => {
                let decision = match failure {
                    Failure::StorageStatus(status) => {
                        wifi_retry::after_rpc_status(status, self.attempt)
                    }
                    _ => wifi_retry::after_rpc_failure(self.attempt),
                };
                self.apply_retry_decision(failure, decision)
            }
        }
    }

    /// Services transport backpressure, station events and DHCP once.
    pub fn service(&mut self) {
        if !self.enabled {
            if !matches!(self.state, State::Off) {
                self.transition(State::Off, Cause::Disabled);
            }
            return;
        }
        if self.session.as_ref().is_some_and(|rpc| !rpc.is_alive()) {
            let retry = self.retry_is_active();
            if retry && self.connected_since_ms.is_some() {
                self.reconnecting = true;
            }
            self.connected_since_ms = None;
            self.session = None;
            self.stack = None;
            let failure = Failure::LinkLost;
            if retry {
                let _ =
                    self.apply_retry_decision(failure, wifi_retry::after_rpc_failure(self.attempt));
            } else {
                self.reconnecting = false;
                self.push_notice(Notice::LinkLost);
                self.transition(State::Failed(failure), Cause::LinkLost);
            }
            return;
        }
        if self.session.is_some() && matches!(self.state, State::LinkDown) {
            self.transition(State::Idle, Cause::LinkReady);
        }

        let events = match (self.session.as_mut(), self.stack.as_mut()) {
            (Some(rpc), Some(stack)) => {
                stack.poll(rpc);
                rpc.take_events()
            }
            (Some(rpc), None) => {
                rpc.discard_station_frames();
                rpc.take_events()
            }
            _ => Vec::new(),
        };

        for event in &events {
            let Some(outcome) = wifi::station::outcome_from_event(event) else {
                continue;
            };
            let accepts_event = match self.state {
                State::Associating { generation, .. } => generation == self.generation,
                State::RetryWaiting { .. } | State::NeedsPassword(_) | State::Failed(_) => false,
                _ => true,
            };
            if !accepts_event {
                self.record_only(Cause::StaleEvent(event.msg_id));
                continue;
            }
            match outcome {
                wifi::station::Outcome::Connected {
                    ssid,
                    ssid_length,
                    channel,
                    ..
                } => {
                    let was_associating = matches!(self.state, State::Associating { .. });
                    let was_reconnecting = self.reconnecting;
                    let association = Association {
                        ssid,
                        ssid_length,
                        channel,
                    };
                    self.connected_since_ms = Some(tick::now_ms());
                    self.reconnecting = false;
                    if was_reconnecting || !was_associating {
                        self.push_notice(Notice::Reassociated);
                    }
                    self.transition(State::Associated(association), Cause::Connected);
                    if self.save_pending {
                        self.persist_profile();
                    }
                    if self.source == Some(ConnectionSource::MenuManaged)
                        && self.ip_policy == IpPolicy::Dhcp
                    {
                        self.start_menu_dhcp(association);
                    }
                }
                wifi::station::Outcome::Disconnected { reason } => {
                    let retry = self.retry_is_active();
                    if retry && self.connected_since_ms.is_some() {
                        self.reconnecting = true;
                    }
                    self.connected_since_ms = None;
                    self.stack = None;
                    let failure = Failure::Disconnected(reason);
                    if retry {
                        let _ = self.apply_retry_decision(
                            failure,
                            wifi_retry::after_disconnect(reason, self.attempt),
                        );
                    } else {
                        self.reconnecting = false;
                        self.push_notice(Notice::Disconnected(reason));
                        self.transition(State::Failed(failure), Cause::Disconnected(reason));
                    }
                }
                wifi::station::Outcome::TimedOut => {}
            }
        }

        match self.state {
            State::Associating {
                deadline_ms,
                generation,
                ..
            } if generation == self.generation && tick::now_ms() >= deadline_ms => {
                let failure = Failure::AssociationTimedOut;
                if self.retry_is_active() {
                    let _ =
                        self.apply_retry_decision(failure, wifi_retry::after_timeout(self.attempt));
                } else {
                    self.transition(State::Failed(failure), Cause::AssociationTimeout);
                }
            }
            State::RetryWaiting {
                deadline_ms,
                next_attempt,
                generation,
                ..
            } if generation == self.generation && tick::now_ms() >= deadline_ms => {
                self.attempt = next_attempt;
                let _ = self.send_pending_connect();
            }
            State::RequestingDhcp {
                association,
                deadline_ms,
            } => {
                if self.stack.as_ref().is_some_and(net::Stack::has_address) {
                    self.transition(State::Online(association), Cause::DhcpConfigured);
                } else if tick::now_ms() >= deadline_ms {
                    // Keep the DHCP socket alive. A later frame can still
                    // move this state to Online without another command.
                    self.transition(State::AssociatedNoLease(association), Cause::DhcpTimeout);
                }
            }
            State::AssociatedNoLease(association)
                if self.stack.as_ref().is_some_and(net::Stack::has_address) =>
            {
                self.transition(State::Online(association), Cause::DhcpConfigured);
            }
            State::Online(association)
                if !self.stack.as_ref().is_some_and(net::Stack::has_address) =>
            {
                let state = if self.ip_policy == IpPolicy::Dhcp {
                    State::AssociatedNoLease(association)
                } else {
                    State::Associated(association)
                };
                self.transition(state, Cause::DhcpLost);
            }
            _ => {}
        }

        if self.attempt != 0
            && matches!(
                self.state,
                State::RequestingDhcp { .. } | State::AssociatedNoLease(_) | State::Online(_)
            )
            && self.connected_since_ms.is_some_and(|since| {
                wifi_retry::should_reset_after_stable(tick::now_ms().saturating_sub(since))
            })
        {
            self.attempt = 0;
            self.record_only(Cause::StableConnection);
        }
    }

    fn retry_is_active(&self) -> bool {
        self.source == Some(ConnectionSource::MenuManaged) && self.credentials.is_some()
    }

    fn apply_retry_decision(
        &mut self,
        failure: Failure,
        mut decision: Decision,
    ) -> Result<(), Failure> {
        if self.startup_retry_policy && matches!(decision, Decision::RetryAfter(_)) {
            if self.attempt >= STARTUP_MAX_ATTEMPTS {
                self.clear_credentials();
                self.connected_since_ms = None;
                self.reconnecting = false;
                self.transition(State::Failed(failure), cause_for_failure(failure));
                return Err(failure);
            }
            decision = Decision::RetryAfter(STARTUP_RETRY_DELAY_MS);
        }
        match decision {
            Decision::Stop => {
                if self.save_pending {
                    self.profile_save_state = ProfileSaveState::Failed;
                }
                self.clear_credentials();
                self.connected_since_ms = None;
                self.reconnecting = false;
                if let Failure::Disconnected(reason) = failure {
                    self.transition(State::NeedsPassword(reason), Cause::Disconnected(reason));
                } else {
                    self.transition(State::Failed(failure), cause_for_failure(failure));
                }
                Err(failure)
            }
            Decision::RetryAfter(delay_ms) => {
                let next_attempt = self.attempt.saturating_add(1);
                // Keep the originating reason/status as its own history entry.
                // The following transition records the chosen backoff, so both
                // halves of the retry decision remain visible in `wifilog`.
                self.record_only(cause_for_failure(failure));
                self.transition(
                    State::RetryWaiting {
                        deadline_ms: tick::now_ms().saturating_add(delay_ms as u64),
                        next_attempt,
                        generation: self.generation,
                        failure,
                    },
                    Cause::RetryScheduled(delay_ms),
                );
                Ok(())
            }
        }
    }

    fn clear_credentials(&mut self) {
        self.credentials = None;
        self.save_pending = false;
    }

    fn persist_profile(&mut self) {
        self.profile_save_attempts = self.profile_save_attempts.saturating_add(1);
        let Some(credentials) = self.credentials.as_ref() else {
            self.save_pending = false;
            self.profile_save_state = ProfileSaveState::Failed;
            self.profile_save_failures = self.profile_save_failures.saturating_add(1);
            self.record_only(Cause::ProfileSaveFailed);
            return;
        };
        let mut ssid = credentials.ssid;
        let ssid_length = credentials.ssid_length;
        let mut password = credentials.password;
        let password_length = credentials.password_length;

        let saved = if let Some(rpc) = self.session.as_mut() {
            let selected = wifi::station::set_storage(rpc, wifi::station::Storage::Flash);
            let written = if selected == Some(0) {
                wifi::station::set_station_config(
                    rpc,
                    &ssid[..ssid_length],
                    &password[..password_length],
                )
            } else {
                None
            };
            // All ordinary connection paths select RAM again too, but restore
            // it now so no later operation can accidentally inherit FLASH.
            let _ = wifi::station::set_storage(rpc, wifi::station::Storage::Ram);
            written == Some(0)
        } else {
            false
        };
        zeroize(&mut ssid);
        zeroize(&mut password);

        self.save_pending = false;
        self.profile_save_state = if saved {
            ProfileSaveState::Saved
        } else {
            ProfileSaveState::Failed
        };
        if !saved {
            self.profile_save_failures = self.profile_save_failures.saturating_add(1);
        } else {
            self.saved_profile_exists = true;
        }
        self.record_only(if saved {
            Cause::ProfileSaved
        } else {
            Cause::ProfileSaveFailed
        });
    }

    /// Deletes the C6's persistent Wi-Fi settings without dropping an active
    /// association. The current connection may continue, but it will no
    /// longer auto-reconnect and the next boot has no saved profile.
    pub fn forget_saved_profile(&mut self) -> Result<(), Failure> {
        if self.enabled {
            let result = {
                let rpc = self.ensure_station()?;
                wifi::station::restore_persistent_settings(rpc)
            };
            match result {
                Some(0) => {}
                Some(status) => return Err(Failure::ConfigStatus(status)),
                None => return Err(Failure::ConfigRpc),
            }
            if let Some(rpc) = self.session.as_mut() {
                let _ = wifi::station::set_storage(rpc, wifi::station::Storage::Ram);
            }
        } else {
            // `forget` is also useful while the radio is disabled. Power the
            // C6 only for the flash operation, then write the empty-profile
            // OFF marker because restore also removes our marker.
            let mut rpc = self.open_initialized_rpc()?;
            let result = (|| {
                match wifi::station::restore_persistent_settings(&mut rpc) {
                    Some(0) => {}
                    Some(status) => return Err(Failure::ConfigStatus(status)),
                    None => return Err(Failure::ConfigRpc),
                }
                match wifi::station::set_storage(&mut rpc, wifi::station::Storage::Flash) {
                    Some(0) => {}
                    Some(status) => return Err(Failure::StorageStatus(status)),
                    None => return Err(Failure::StorageRpc),
                }
                match wifi::station::set_mode(&mut rpc, wifi::station::WIFI_MODE_STA) {
                    Some(0) => {}
                    Some(status) => return Err(Failure::ModeStatus(status)),
                    None => return Err(Failure::ModeRpc),
                }
                match wifi::station::set_disabled_without_profile(&mut rpc) {
                    Some(0) => {}
                    Some(status) => return Err(Failure::ConfigStatus(status)),
                    None => return Err(Failure::ConfigRpc),
                }
                match wifi::station::set_mode(&mut rpc, wifi::station::WIFI_MODE_NULL) {
                    Some(0) => Ok(()),
                    Some(status) => Err(Failure::ModeStatus(status)),
                    None => Err(Failure::ModeRpc),
                }
            })();
            drop(rpc);
            sdio::power_down_c6();
            if let Err(failure) = result {
                self.transition(State::Off, cause_for_failure(failure));
                return Err(failure);
            }
        }
        self.clear_credentials();
        self.source = None;
        self.save_pending = false;
        self.profile_save_state = ProfileSaveState::NotRequested;
        self.saved_profile_exists = false;
        self.profile_forgets = self.profile_forgets.saturating_add(1);
        self.record_only(Cause::ProfileForgotten);
        Ok(())
    }

    fn push_notice(&mut self, notice: Notice) {
        if self.notices.len() >= NOTICE_LIMIT {
            self.notices.remove(0);
        }
        self.notices.push(notice);
    }

    fn transition(&mut self, state: State, cause: Cause) {
        let transition = Transition {
            at_ms: tick::now_ms(),
            from: self.state.phase(),
            to: state.phase(),
            cause,
            attempt: self.attempt,
            generation: self.generation,
        };
        if self.history.len() >= HISTORY_LIMIT {
            self.history.remove(0);
        }
        self.history.push(transition);
        self.state = state;
    }

    fn record_only(&mut self, cause: Cause) {
        let phase = self.state.phase();
        let transition = Transition {
            at_ms: tick::now_ms(),
            from: phase,
            to: phase,
            cause,
            attempt: self.attempt,
            generation: self.generation,
        };
        if self.history.len() >= HISTORY_LIMIT {
            self.history.remove(0);
        }
        self.history.push(transition);
    }

    fn start_menu_dhcp(&mut self, association: Association) {
        if !tick::is_running() {
            self.transition(State::Failed(Failure::TickUnavailable), Cause::RpcFailed);
            return;
        }
        let mac = {
            let Some(rpc) = self.session.as_mut() else {
                self.transition(State::Failed(Failure::LinkLost), Cause::LinkLost);
                return;
            };
            match wifi::rpc::get_mac_address(rpc, wifi::rpc::WIFI_IF_STA) {
                Some((0, mac)) => mac,
                Some((status, _)) => {
                    self.transition(
                        State::Failed(Failure::MacStatus(status)),
                        Cause::RpcStatus(status),
                    );
                    return;
                }
                None => {
                    self.transition(State::Failed(Failure::MacRpc), Cause::RpcFailed);
                    return;
                }
            }
        };
        let Some(rpc) = self.session.as_mut() else {
            self.transition(State::Failed(Failure::LinkLost), Cause::LinkLost);
            return;
        };
        let mut stack = net::Stack::new(rpc, mac);
        stack.start_dhcp();
        self.stack = Some(stack);
        self.transition(
            State::RequestingDhcp {
                association,
                deadline_ms: tick::now_ms().saturating_add(DHCP_TIMEOUT_MS),
            },
            Cause::DhcpStarted,
        );
    }
}

fn retry_decision(failure: Failure, failed_attempts: u32) -> Decision {
    match failure {
        Failure::Disconnected(reason) => wifi_retry::after_disconnect(reason, failed_attempts),
        Failure::AssociationTimedOut => wifi_retry::after_timeout(failed_attempts),
        Failure::StartStatus(status)
        | Failure::ConnectStatus(status)
        | Failure::ConfigStatus(status)
        | Failure::StorageStatus(status)
        | Failure::DisconnectStatus(status)
        | Failure::ModeStatus(status)
        | Failure::StopStatus(status) => wifi_retry::after_rpc_status(status, failed_attempts),
        _ => wifi_retry::after_rpc_failure(failed_attempts),
    }
}

fn cause_for_failure(failure: Failure) -> Cause {
    match failure {
        Failure::Disconnected(reason) => Cause::Disconnected(reason),
        Failure::AssociationTimedOut => Cause::AssociationTimeout,
        Failure::ConnectStatus(status)
        | Failure::StartStatus(status)
        | Failure::MacStatus(status)
        | Failure::ConfigStatus(status)
        | Failure::StorageStatus(status)
        | Failure::DisconnectStatus(status)
        | Failure::ModeStatus(status)
        | Failure::StopStatus(status) => Cause::RpcStatus(status),
        Failure::DisconnectTimedOut => Cause::DisconnectTimeout,
        Failure::LinkLost => Cause::LinkLost,
        Failure::LinkBringUp
        | Failure::StartRpc
        | Failure::ConnectRpc
        | Failure::ConfigRpc
        | Failure::StorageRpc
        | Failure::DisconnectRpc
        | Failure::ModeRpc
        | Failure::StopRpc
        | Failure::TickUnavailable
        | Failure::MacRpc => Cause::RpcFailed,
        Failure::Disabled => Cause::Disabled,
    }
}

fn zeroize(bytes: &mut [u8]) {
    for byte in bytes {
        // A normal fill can be removed once the credential is dead.
        unsafe { core::ptr::write_volatile(byte, 0) };
    }
}
