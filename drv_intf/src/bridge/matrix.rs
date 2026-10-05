//! Captain-visible live acceptance run, advanced once per cockpit frame.
use super::*;

const HARNESSES: [&str; 4] = ["pi", "codex", "claude", "cursor"];
const EDGE_TIMEOUT: Duration = Duration::from_secs(150);

pub(super) struct HandoffMatrix {
    seats: Vec<usize>,
    edges: Vec<Edge>,
    current: usize,
    started: Instant,
    work_id: Option<String>,
    /// Handoff receipts already accounted for.
    seen: usize,
}
struct Edge {
    from: usize,
    to: usize,
    status: String,
    reason: String,
}
impl HandoffMatrix {
    fn new(seats: Vec<usize>) -> Self {
        Self {
            seats,
            edges: (0..4)
                .flat_map(|from| {
                    (0..4).filter(move |to| *to != from).map(move |to| Edge {
                        from,
                        to,
                        status: "pending".into(),
                        reason: String::new(),
                    })
                })
                .collect(),
            current: 0,
            started: Instant::now(),
            work_id: None,
            seen: 0,
        }
    }
    fn running(&self) -> bool {
        self.current < self.edges.len()
    }
}
impl Ship {
    pub(super) fn start_handoff_matrix(&mut self) -> Result<()> {
        ensure!(
            !self.demo,
            "/test-handoffs requires live Bridge; relaunch without --demo"
        );
        if self.matrix.as_ref().is_some_and(HandoffMatrix::running) {
            self.matrix_view = true;
            return Ok(());
        }
        ensure!(
            self.obs.crew.iter().all(|a| !a.turn_active && a.queue == 0),
            "Wait for crew turns and mailboxes to finish before /test-handoffs"
        );
        let seats = self
            .matrix
            .as_ref()
            .map(|m| m.seats.clone())
            .unwrap_or_default();
        ensure!(
            !seats.is_empty() || self.obs.crew.len() <= 4,
            "Matrix needs four free seats; start a fresh flight"
        );
        ensure!(
            seats
                .iter()
                .all(|pid| self.obs.crew[*pid - 1].state == "idle"),
            "A previous test seat failed; F10 and launch a fresh flight to rerun"
        );
        self.matrix = Some(HandoffMatrix::new(seats));
        self.matrix_view = true;
        self.telemetry = false;
        self.logs = false;
        self.game.active = false;
        self.scroll = 0;
        self.last_lines = 0;
        self.record(
            "HANDOFF_MATRIX / starting 12 directed live edges / 150s per edge / F10 cleans up",
        );
        Ok(())
    }
    pub fn handoff_matrix_text(&self) -> Option<String> {
        let m = self.matrix.as_ref()?;
        let passed = m.edges.iter().filter(|e| e.status == "PASS").count();
        let failed = m.edges.iter().filter(|e| e.status == "FAIL").count();
        let state = if m.running() { "RUNNING" } else { "COMPLETE" };
        let mut text = format!(
            "HANDOFF MATRIX / {state} / {passed} pass / {failed} fail / {} pending\nLive ACP · 150s per edge · F10 stops all seats\nTest seats remain available; select a crew seat to inspect its COMMS.\n\n| From | To | Result | Evidence / reason |\n| --- | --- | --- | --- |\n",
            12 - passed - failed
        );
        for e in &m.edges {
            text.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                HARNESSES[e.from],
                HARNESSES[e.to],
                e.status,
                e.reason.replace(['\n', '|'], " ")
            ));
        }
        Some(text)
    }
    pub(super) fn ensure_matrix_idle(&self) -> Result<()> {
        ensure!(
            !self.matrix.as_ref().is_some_and(HandoffMatrix::running),
            "Handoff matrix owns test traffic; inspect logs/crew or press F10 to stop"
        );
        Ok(())
    }
    fn booting(&self) -> bool {
        self.obs.crew.iter().any(|a| a.state == "booting")
    }
    pub fn log_paths(&self) -> String {
        format!(
            "Flight journals: .unvrs/flight-*.jsonl · Seat recorders: {}",
            self.dir.display()
        )
    }
    pub(super) fn tick_handoff_matrix(&mut self) {
        let Some(mut matrix) = self.matrix.take() else {
            return;
        };
        if matrix.running()
            && let Err(e) = self.advance_matrix(&mut matrix)
        {
            self.finish_matrix_edge(&mut matrix, false, &format!("{e:#}"));
        }
        self.matrix = Some(matrix);
    }
    fn finish_matrix_edge(&mut self, m: &mut HandoffMatrix, pass: bool, reason: &str) {
        if !pass && m.work_id.is_some() {
            for profile in [m.edges[m.current].from, m.edges[m.current].to] {
                let pid = m.seats[profile];
                if self.obs.crew[pid - 1].turn_active || self.obs.crew[pid - 1].queue > 0 {
                    self.stop_matrix_seat(pid);
                }
            }
        }
        let edge = &mut m.edges[m.current];
        edge.status = if pass { "PASS" } else { "FAIL" }.into();
        edge.reason = reason.chars().take(240).collect();
        self.record(&format!(
            "HANDOFF_MATRIX / {} → {} / {} / {}",
            HARNESSES[edge.from], HARNESSES[edge.to], edge.status, edge.reason
        ));
        m.current += 1;
        m.work_id = None;
        m.seen = self.obs.handoffs.len();
        m.started = Instant::now();
        if !m.running() {
            let passed = m.edges.iter().filter(|e| e.status == "PASS").count();
            self.notice = format!(
                "HANDOFF MATRIX COMPLETE / {passed} pass / {} fail / F10 cleans up",
                12 - passed
            );
            self.record(&self.notice.clone());
        }
    }
    fn advance_matrix(&mut self, m: &mut HandoffMatrix) -> Result<()> {
        // Boot one profile at a time through the normal seat path. Failed boots
        // stay visible and their edges fail; they never stall the other profiles.
        if m.seats.len() < 4 {
            if self.booting() {
                return Ok(());
            }
            let harness = HARNESSES[m.seats.len()];
            let pid = self.spawn_profile(
                1,
                2,
                &format!("TEST {}", harness.to_uppercase()),
                "Live handoff matrix",
                harness,
            )?;
            m.seats.push(pid);
            m.started = Instant::now();
            return Ok(());
        }
        if self.booting() {
            return Ok(());
        }
        let from_profile = m.edges[m.current].from;
        let to_profile = m.edges[m.current].to;
        let from = m.seats[from_profile];
        let to = m.seats[to_profile];
        for pid in [from, to] {
            let a = &self.obs.crew[pid - 1];
            ensure!(
                !matches!(a.state.as_str(), "offline" | "blocked"),
                "{} PID {pid} {}: {}",
                a.harness,
                a.state,
                a.transcript
                    .lines()
                    .rev()
                    .find(|l| !l.is_empty())
                    .unwrap_or("no diagnostic")
            );
        }
        if m.started.elapsed() >= EDGE_TIMEOUT {
            // A timed-out turn must not leak a late handoff into a later edge.
            for pid in [from, to] {
                if self.obs.crew[pid - 1].turn_active || self.obs.crew[pid - 1].queue > 0 {
                    self.stop_matrix_seat(pid);
                }
            }
            bail!("150s timeout; active test seats stopped; inspect seat recorders");
        }
        // The kernel routes seat handoffs itself; a transfer the matrix did not ask for
        // fails the edge instead of being refused up front.
        if let Some(stray) = self.obs.handoffs[m.seen..]
            .iter()
            .find(|h| m.work_id.as_deref() != Some(h.work_id.as_str()))
        {
            bail!(
                "Unexpected transfer {} from PID {} during the matrix",
                stray.work_id,
                stray.from_pid
            );
        }
        if let Some(work_id) = &m.work_id {
            let delivered = self.obs.handoffs.iter().find(|h| &h.work_id == work_id);
            if let Some(h) = delivered {
                ensure!(
                    h.from_pid == from && h.to_pid == Some(to),
                    "Unexpected owner for matrix work_id"
                );
                m.edges[m.current].status = "delivered".into();
                m.edges[m.current].reason = format!("{work_id}; owner PID {to}; awaiting reply");
                let target = &self.obs.crew[to - 1];
                let source = &self.obs.crew[from - 1];
                if target.state == "idle"
                    && target.queue == 0
                    && source.state == "idle"
                    && source.queue == 0
                {
                    ensure!(
                        target
                            .reply
                            .lines()
                            .rev()
                            .map(str::trim)
                            .find(|line| !line.is_empty())
                            == Some("HANDOFF_OK"),
                        "Target completed without HANDOFF_OK"
                    );
                    self.finish_matrix_edge(
                        m,
                        true,
                        &format!("{work_id}; owner PID {to}; HANDOFF_OK"),
                    );
                }
            } else if self.obs.crew[from - 1].state == "idle" && self.obs.crew[from - 1].queue == 0
            {
                bail!("Source completed without invoking ctl handoff");
            }
            return Ok(());
        }
        if [from, to]
            .iter()
            .any(|pid| self.obs.crew[*pid - 1].state != "idle" || self.obs.crew[*pid - 1].queue > 0)
        {
            return Ok(());
        }
        let mut summary = drv_hdff::HandoffSummary::new(
            from,
            2,
            HARNESSES[to_profile],
            "Reply exactly HANDOFF_OK. Do not call tools, send messages or delegate.",
        );
        summary.to_pid = Some(to);
        summary.goal = "Verify live directed handoff and work ownership".into();
        summary.done = vec![format!("Source {} connected", HARNESSES[from_profile])];
        let path = self.dir.join(format!("matrix-{}.toon", m.current));
        fs::write(
            &path,
            format!(
                "work_id: {}\nto_rank: 2\nto_pid: {to}\nto_harness: {}\nnext_focus: {}\ngoal: {}\ndone[1]: {}\nopen[0]:\nartifacts[0]:\n",
                uke::scalar(&summary.work_id),
                HARNESSES[to_profile],
                uke::scalar(&summary.next_focus),
                uke::scalar(&summary.goal),
                uke::scalar(&summary.done[0]),
            ),
        )?;
        let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
        let command = format!(
            "{} ctl handoff {}",
            quote(&env::current_exe()?.to_string_lossy()),
            quote(&path.to_string_lossy())
        );
        self.enqueue(from, format!("Run exactly this shell command: {command}. Then reply REQUESTED. Do not read or edit files, send messages, or delegate separately."))?;
        m.work_id = Some(summary.work_id);
        m.seen = self.obs.handoffs.len();
        m.started = Instant::now();
        m.edges[m.current].status = "requesting".into();
        m.edges[m.current].reason = format!("PID {from} invoking ctl handoff → PID {to}");
        self.record(&format!(
            "HANDOFF_MATRIX / {} → {} / requesting",
            HARNESSES[from_profile], HARNESSES[to_profile]
        ));
        Ok(())
    }
    fn stop_matrix_seat(&mut self, pid: usize) {
        if let Err(e) = self.console.cmd("stop_seat", json!({"pid": pid})) {
            self.notice = format!("Matrix could not stop PID {pid}: {e}");
        }
        let a = &mut self.obs.crew[pid - 1];
        a.queue = 0;
        a.turn_active = false;
        a.state = "offline".into();
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::settle;
    use super::*;
    #[test]
    #[ignore = "console seats left the kernel in 0.8 (C0); drv_intf is frozen"]
    fn matrix_covers_directed_edges_and_rejects_false_success() {
        let mut ship = Ship::new(true).unwrap();
        assert!(ship.start_handoff_matrix().is_err());
        let cursor = ship
            .spawn_profile(1, 2, "CURSOR", "test", "cursor")
            .unwrap();
        let mut m = HandoffMatrix::new(vec![1, 2, 3, cursor]);
        assert_eq!(m.edges.len(), 12);
        assert!(m.edges.iter().all(|e| e.from != e.to));
        settle(&mut ship, 100);
        ship.advance_matrix(&mut m).unwrap();
        let id = m.work_id.clone().unwrap();
        // The demo copilot answers without running ctl handoff.
        settle(&mut ship, 2500);
        assert!(
            ship.advance_matrix(&mut m)
                .unwrap_err()
                .to_string()
                .contains("without invoking")
        );
        let mut h = drv_hdff::HandoffSummary::new(1, 2, "pi", "test");
        h.work_id = id;
        h.to_pid = Some(2);
        ship.handoff(1, h).unwrap();
        // The demo scout answers the card without HANDOFF_OK.
        settle(&mut ship, 2500);
        assert!(
            ship.advance_matrix(&mut m)
                .unwrap_err()
                .to_string()
                .contains("without HANDOFF_OK")
        );
        ship.obs.crew[1].reply = "Cannot return HANDOFF_OK".into();
        assert!(ship.advance_matrix(&mut m).is_err());
        ship.obs.crew[1].reply = "Adapter startup notice\n\nHANDOFF_OK".into();
        ship.advance_matrix(&mut m).unwrap();
        assert_eq!(m.edges[0].status, "PASS");
        m.started = Instant::now() - EDGE_TIMEOUT;
        assert!(
            ship.advance_matrix(&mut m)
                .unwrap_err()
                .to_string()
                .contains("timeout")
        );
        ship.matrix = Some(m);
        assert!(ship.handoff_matrix_text().unwrap().contains("1 pass"));
        ship.input = "interfering captain message".into();
        assert!(ship.submit().is_err(), "the matrix owns test traffic");
        ship.matrix.as_mut().unwrap().current = 12;
        assert!(ship.ensure_matrix_idle().is_ok());
    }
    #[test]
    #[ignore = "console seats left the kernel in 0.8 (C0); drv_intf is frozen"]
    fn matrix_edges_route_through_the_kernel_with_receipts() {
        let mut ship = Ship::new(true).unwrap();
        let seats = HARNESSES
            .iter()
            .map(|h| ship.spawn_profile(1, 2, "TEST", "matrix", h).unwrap())
            .collect();
        let mut m = HandoffMatrix::new(seats);
        settle(&mut ship, 100);
        for index in 0..3 {
            ship.advance_matrix(&mut m).unwrap();
            let h = drv_hdff::handoff_summary(
                &fs::read_to_string(ship.dir.join(format!("matrix-{index}.toon"))).unwrap(),
            )
            .unwrap();
            let from = m.seats[m.edges[index].from];
            let to = m.seats[m.edges[index].to];
            ship.handoff(from, h).unwrap();
            settle(&mut ship, 3000);
            for pid in [from, to] {
                ship.obs.crew[pid - 1].state = "idle".into();
                ship.obs.crew[pid - 1].queue = 0;
            }
            ship.obs.crew[to - 1].reply = "HANDOFF_OK".into();
            ship.advance_matrix(&mut m).unwrap();
            assert_eq!(m.edges[index].status, "PASS", "{}", m.edges[index].reason);
            let receipt = ship.obs.handoffs.last().unwrap();
            assert_eq!(receipt.from_harness, HARNESSES[m.edges[index].from]);
            assert_eq!(
                receipt.to_harness.as_deref(),
                Some(HARNESSES[m.edges[index].to])
            );
            assert_eq!(receipt.to_pid, Some(to));
        }
    }
}
