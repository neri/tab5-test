//! Incremental GUI radio operations, borrowing the existing Manager.
//!
//! Stages 0..9 initialize/enable, 10..15 replace a connection,
//! 20..22 scan, 30..35 persist OFF, 40..43 save after association and
//! start DHCP, and 50..51 forget. Each stage submits or reaps one RPC.
//! A token belongs to the requesting screen; shared recovery work has token 0.
use super::*;

#[derive(Clone, Copy)]
pub(super) enum GuiKind {
    Scan,
    Connect,
    Retry,
    Enable,
    Disable,
    Forget,
    Associated(Association),
}
pub(super) struct GuiJob {
    token: u32,
    kind: GuiKind,
    stage: u8,
    sent: bool,
    found: i32,
    deadline: u64,
    failed: bool,
}
impl GuiJob {
    pub(super) fn new(kind: GuiKind, stage: u8) -> Self {
        Self {
            token: 0,
            kind,
            stage,
            sent: false,
            found: 0,
            deadline: 0,
            failed: false,
        }
    }
}

impl Manager {
    pub fn gui_busy(&self) -> bool {
        self.gui_job.is_some() || self.gui_followup.is_some()
    }
    pub fn enter_gui(&mut self) {
        self.gui_mode = true;
        if let Some(rpc) = self.session.as_mut() {
            rpc.set_gui_mode(true);
        }
    }
    pub fn leave_gui(&mut self) {
        // Finish a submitted operation in the manager on the console side;
        // no departed screen handler or credential buffer is retained.
        self.gui_mode = false;
        if let Some(rpc) = self.session.as_mut() {
            rpc.set_gui_mode(false);
        }
    }
    fn begin_gui(&mut self, kind: GuiKind, stage: u8) -> Result<u32, Failure> {
        if self.gui_job.is_some() || self.gui_followup.is_some() {
            return Err(Failure::ConnectRpc);
        }
        self.gui_reply = None;
        self.gui_sequence = self.gui_sequence.wrapping_add(1).max(1);
        let mut job = GuiJob::new(kind, stage);
        job.token = self.gui_sequence;
        self.gui_job = Some(job);
        Ok(self.gui_sequence)
    }
    pub fn gui_scan(&mut self) -> Result<u32, Failure> {
        if !self.enabled {
            return Err(Failure::Disabled);
        }
        self.begin_gui(GuiKind::Scan, if self.session.is_some() { 20 } else { 0 })
    }
    pub fn gui_enable(&mut self, enabled: bool) -> Result<u32, Failure> {
        self.begin_gui(
            if enabled {
                GuiKind::Enable
            } else {
                GuiKind::Disable
            },
            if self.session.is_some() {
                if enabled { 4 } else { 30 }
            } else {
                0
            },
        )
    }
    pub fn gui_forget(&mut self) -> Result<u32, Failure> {
        self.begin_gui(GuiKind::Forget, if self.session.is_some() { 50 } else { 0 })
    }
    pub fn gui_connect(&mut self, ssid: &[u8], password: &[u8]) -> Result<u32, Failure> {
        if !self.enabled {
            return Err(Failure::Disabled);
        }
        let token = self.begin_gui(
            GuiKind::Connect,
            if self.session.is_some() { 10 } else { 0 },
        )?;
        self.generation = self.generation.wrapping_add(1).max(1);
        self.credentials = Some(Credentials::new(ssid, password));
        self.stack = None;
        self.source = Some(ConnectionSource::MenuManaged);
        self.ip_policy = IpPolicy::Dhcp;
        self.attempt = 1;
        self.connected_since_ms = None;
        self.reconnecting = false;
        self.save_pending = true;
        self.profile_save_state = ProfileSaveState::Pending;
        Ok(token)
    }
    pub fn gui_result(
        &mut self,
        token: u32,
    ) -> Option<Result<Option<Vec<wifi::station::AccessPoint>>, Failure>> {
        if self.gui_reply.as_ref().is_some_and(|(id, _)| *id == token) {
            self.gui_reply.take().map(|(_, result)| result)
        } else {
            None
        }
    }
    fn complete_gui(
        &mut self,
        job: &GuiJob,
        result: Result<Option<Vec<wifi::station::AccessPoint>>, Failure>,
    ) {
        if job.token != 0 {
            self.gui_reply = Some((job.token, result));
        }
    }

    pub(super) fn service_gui_job(&mut self) {
        let mut job = self.gui_job.take().expect("GUI job");
        let now = tick::now_ms();
        // Scanning and persistence do not suppress link-loss invalidation or
        // DHCP maintenance. A new policy operation waits in one shared slot.
        if matches!(
            job.kind,
            GuiKind::Scan | GuiKind::Forget | GuiKind::Associated(_)
        ) && job.stage != 0
        {
            let mode = self.gui_mode;
            self.gui_mode = true;
            let waiting = self.gui_followup.take();
            self.service();
            let generated = self.gui_job.take();
            self.gui_followup = generated.or(waiting);
            self.gui_mode = mode;
            if matches!(job.kind, GuiKind::Associated(_))
                && !matches!(
                    self.state,
                    State::Associated(_)
                        | State::Online(_)
                        | State::RequestingDhcp { .. }
                        | State::AssociatedNoLease(_)
                )
            {
                if let Some(rpc) = self.session.as_mut() {
                    rpc.cancel_call();
                }
                self.complete_gui(&job, Err(Failure::LinkLost));
                return;
            }
        }
        if job.stage == 0 {
            let bringup = self
                .gui_bringup
                .get_or_insert_with(|| alloc::boxed::Box::new(wifi::hosted::BringUp::new()));
            match bringup.poll(now) {
                Some(Ok(transport)) => {
                    self.gui_bringup = None;
                    let mut rpc = wifi::Rpc::new(transport);
                    rpc.set_gui_mode(self.gui_mode);
                    self.session = Some(rpc);
                    job.stage = 1;
                }
                Some(Err(())) => {
                    self.gui_bringup = None;
                    self.gui_fail(&job, Failure::LinkBringUp);
                    return;
                }
                None => {}
            }
            self.gui_job = Some(job);
            return;
        }
        let Some(rpc) = self.session.as_mut() else {
            self.complete_gui(&job, Err(Failure::LinkLost));
            return;
        };
        // Collect data and notifications while a request is awaiting its UID.
        if let Some(stack) = self.stack.as_mut() {
            stack.poll(rpc);
        } else {
            rpc.discard_station_frames();
        }
        if job.stage == 10
            && !job.sent
            && !matches!(
                self.state,
                State::Associating { .. }
                    | State::Associated(_)
                    | State::Online(_)
                    | State::RequestingDhcp { .. }
                    | State::AssociatedNoLease(_)
            )
        {
            let _ = rpc.take_events();
            job.stage = 11;
            self.gui_job = Some(job);
            return;
        }
        if job.stage == 15 {
            let disconnected = rpc.take_events().iter().any(|e| {
                matches!(
                    wifi::station::outcome_from_event(e),
                    Some(wifi::station::Outcome::Disconnected { .. })
                )
            });
            if disconnected {
                job.stage = 11;
            } else if now >= job.deadline {
                self.complete_gui(&job, Err(Failure::DisconnectTimedOut));
                self.mark_disconnected();
                return;
            }
            self.gui_job = Some(job);
            return;
        }
        if !job.sent {
            let (id, mut body) = self.gui_request(&job);
            let sent = self
                .session
                .as_mut()
                .is_some_and(|rpc| rpc.begin_call(id, &body));
            zeroize(&mut body);
            if !sent {
                self.gui_fail(&job, Failure::ConnectRpc);
                return;
            }
            job.sent = true;
            self.gui_job = Some(job);
            return;
        }
        let Some(reply) = rpc.poll_call() else {
            self.gui_job = Some(job);
            return;
        };
        job.sent = false;
        let Ok(mut payload) = reply else {
            self.gui_fail(&job, Failure::ConnectRpc);
            return;
        };
        let status_field = if job.stage == 43 { 2 } else { 1 };
        let mut status = 0;
        let mut reader = wifi::proto::Reader::new(&payload);
        while let Some((field, value)) = reader.next_field() {
            if field == status_field {
                status = value.as_i32();
            }
        }
        // Disconnect on an idle station may return NOT_CONNECT. Other errors
        // are reported, with RAM storage restoration after a failed save.
        if status != 0 && job.stage != 30 {
            if (40..=42).contains(&job.stage) {
                job.failed = true;
                if job.stage != 42 {
                    job.stage = 42;
                    self.gui_job = Some(job);
                    zeroize(&mut payload);
                    return;
                }
            } else {
                zeroize(&mut payload);
                self.gui_fail(
                    &job,
                    if job.stage == 10 {
                        Failure::DisconnectStatus(status)
                    } else {
                        Failure::ConnectStatus(status)
                    },
                );
                return;
            }
        }
        let mut done = false;
        match job.stage {
            1 => job.stage = 2,
            2 => job.stage = 3,
            3 => {
                job.stage = if matches!(job.kind, GuiKind::Enable) {
                    4
                } else {
                    6
                }
            }
            4 => {
                if let Some((0, config)) = wifi::station::parse_station_config_response(&payload) {
                    self.saved_profile_exists = !config.ssid().is_empty();
                    if self.saved_profile_exists {
                        self.credentials = Some(Credentials::new(config.ssid(), config.password()));
                    }
                }
                job.stage = 5;
            }
            5 => job.stage = if self.saved_profile_exists { 6 } else { 9 },
            9 => job.stage = 6,
            6 => job.stage = 7,
            7 => job.stage = 8,
            8 => {
                if matches!(job.kind, GuiKind::Enable) {
                    self.enabled = true;
                    self.transition(State::Idle, Cause::LinkReady);
                }
                job.stage = match job.kind {
                    GuiKind::Scan => 20,
                    GuiKind::Disable => 30,
                    GuiKind::Forget => 50,
                    GuiKind::Enable if self.credentials.is_some() => {
                        self.source = Some(ConnectionSource::MenuManaged);
                        self.ip_policy = IpPolicy::Dhcp;
                        self.attempt = 1;
                        self.profile_save_state = ProfileSaveState::Saved;
                        10
                    }
                    GuiKind::Enable => {
                        done = true;
                        8
                    }
                    _ => 10,
                };
            }
            10 => {
                if status == 0
                    && matches!(
                        self.state,
                        State::Associating { .. }
                            | State::Online(_)
                            | State::Associated(_)
                            | State::RequestingDhcp { .. }
                            | State::AssociatedNoLease(_)
                    )
                {
                    job.stage = 15;
                    job.deadline = now.saturating_add(REPLACEMENT_DISCONNECT_TIMEOUT_MS as u64);
                } else {
                    let _ = rpc.take_events();
                    job.stage = 11;
                }
            }
            11 => job.stage = 12,
            12 => {
                let _ = rpc.take_events();
                job.stage = 13;
            }
            13 => {
                self.generation = self.generation.wrapping_add(1).max(1);
                self.transition(
                    State::Associating {
                        deadline_ms: now.saturating_add(ASSOCIATION_TIMEOUT_MS),
                        attempt: self.attempt,
                        generation: self.generation,
                    },
                    Cause::ConnectRequested,
                );
                done = true;
            }
            20 => job.stage = 21,
            21 => {
                let mut reader = wifi::proto::Reader::new(&payload);
                while let Some((field, value)) = reader.next_field() {
                    if field == 2 {
                        job.found = value.as_i32().clamp(0, 64);
                    }
                }
                if job.found == 0 {
                    self.complete_gui(&job, Ok(Some(Vec::new())));
                    zeroize(&mut payload);
                    return;
                }
                job.stage = 22;
            }
            22 => {
                let mut points = Vec::new();
                let mut reader = wifi::proto::Reader::new(&payload);
                while let Some((field, value)) = reader.next_field() {
                    if field == 3 && points.len() < 64 {
                        if let Some(p) = wifi::station::parse_access_point(value.as_bytes()) {
                            points.push(p);
                        }
                    }
                }
                self.complete_gui(&job, Ok(Some(points)));
                zeroize(&mut payload);
                return;
            }
            30 => {
                self.clear_credentials();
                self.stack = None;
                self.source = None;
                job.stage = 31;
            }
            31 => job.stage = if self.saved_profile_exists { 34 } else { 32 },
            32 => job.stage = 33,
            33 => job.stage = 34,
            34 => job.stage = 35,
            35 => {
                self.finish_gui_off();
                done = true;
            }
            40 => {
                self.profile_save_attempts = self.profile_save_attempts.saturating_add(1);
                job.stage = 41;
            }
            41 => job.stage = 42,
            42 => {
                self.save_pending = false;
                self.profile_save_state = if job.failed {
                    ProfileSaveState::Failed
                } else {
                    ProfileSaveState::Saved
                };
                if job.failed {
                    self.profile_save_failures = self.profile_save_failures.saturating_add(1);
                } else {
                    self.saved_profile_exists = true;
                }
                self.record_only(if job.failed {
                    Cause::ProfileSaveFailed
                } else {
                    Cause::ProfileSaved
                });
                job.stage = 43;
            }
            43 => {
                let mut mac = None;
                let mut reader = wifi::proto::Reader::new(&payload);
                while let Some((field, value)) = reader.next_field() {
                    if field == 1 {
                        let bytes = value.as_bytes();
                        if bytes.len() == 6 {
                            let mut m = [0; 6];
                            m.copy_from_slice(bytes);
                            mac = Some(m);
                        }
                    }
                }
                if let (Some(mac), GuiKind::Associated(association)) = (mac, job.kind) {
                    let mut stack = net::Stack::new(rpc, mac);
                    stack.start_dhcp();
                    self.stack = Some(stack);
                    self.transition(
                        State::RequestingDhcp {
                            association,
                            deadline_ms: now.saturating_add(DHCP_TIMEOUT_MS),
                        },
                        Cause::DhcpStarted,
                    );
                } else {
                    self.complete_gui(&job, Err(Failure::MacRpc));
                    zeroize(&mut payload);
                    return;
                }
                done = true;
            }
            50 => {
                self.clear_credentials();
                self.source = None;
                self.saved_profile_exists = false;
                self.profile_forgets = self.profile_forgets.saturating_add(1);
                self.record_only(Cause::ProfileForgotten);
                job.stage = if self.enabled { 51 } else { 31 };
            }
            51 => done = true,
            _ => done = true,
        }
        zeroize(&mut payload);
        if done {
            self.complete_gui(&job, Ok(None));
        } else {
            self.gui_job = Some(job);
        }
    }
    fn gui_fail(&mut self, job: &GuiJob, failure: Failure) {
        self.complete_gui(&job, Err(failure));
        match job.kind {
            GuiKind::Disable | GuiKind::Enable => self.finish_gui_off(),
            GuiKind::Connect | GuiKind::Retry => {
                let _ = self
                    .apply_retry_decision(failure, retry_decision(failure, self.attempt.max(1)));
            }
            GuiKind::Associated(_) => {
                if self.save_pending {
                    self.save_pending = false;
                    self.profile_save_state = ProfileSaveState::Failed;
                    self.profile_save_failures = self.profile_save_failures.saturating_add(1);
                    self.record_only(Cause::ProfileSaveFailed);
                }
                self.transition(State::Failed(failure), cause_for_failure(failure));
            }
            _ => {}
        }
    }
    fn finish_gui_off(&mut self) {
        self.clear_credentials();
        self.stack = None;
        self.session = None;
        self.source = None;
        self.enabled = false;
        sdio::power_down_c6();
        self.transition(State::Off, Cause::Disabled);
    }
    fn gui_request(&self, job: &GuiJob) -> (u32, Vec<u8>) {
        let one = |id, field, value| {
            let mut b = [0; 16];
            let mut w = wifi::proto::Writer::new(&mut b);
            w.int32_field(field, value);
            let n = w.finish().unwrap_or(0);
            (id, b[..n].to_vec())
        };
        match job.stage {
            1 => (278, wifi::station::init_request().unwrap_or_default()),
            2 | 6 | 32 => one(260, 1, 1),
            3 => (280, Vec::new()),
            4 => one(285, 1, 0),
            5 | 31 | 40 => one(313, 1, wifi::station::Storage::Flash as i32),
            7 => one(270, 1, 0),
            8 | 11 | 42 | 51 => one(313, 1, wifi::station::Storage::Ram as i32),
            10 | 30 => (283, Vec::new()),
            12 | 41 => {
                let c = self.credentials.as_ref();
                (
                    284,
                    wifi::station::config_request(
                        c.map_or(&[], |c| &c.ssid[..c.ssid_length]),
                        c.map_or(&[], |c| &c.password[..c.password_length]),
                        false,
                    )
                    .unwrap_or_default(),
                )
            }
            13 => (282, Vec::new()),
            // The slave scan blocks its RPC response, never this host loop.
            20 => (286, alloc::vec![16, 1, 24, 0]),
            21 => (288, Vec::new()),
            22 => one(289, 1, job.found),
            9 => (
                284,
                wifi::station::config_request(&[], &[], false).unwrap_or_default(),
            ),
            33 => (
                284,
                wifi::station::config_request(&[], &[], true).unwrap_or_default(),
            ),
            34 => (281, Vec::new()),
            35 => one(260, 1, 0),
            43 => one(257, 1, 0),
            50 => (291, Vec::new()),
            _ => (0, Vec::new()),
        }
    }
}
