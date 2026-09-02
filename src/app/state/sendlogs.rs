//! "Send logs": upload this session's log to the paired host, or to the developer.
//!
//! Reached from the Diagnostics screen's last row. A selected, paired, reachable host takes
//! the log over its management API (`POST /api/v1/client-logs`, the lane
//! `pf-client-core::logring` defines, under the mTLS identity the library fetch and the
//! stream already use) with no confirmation: its operator is trusted with the session
//! anyway, and the status line names the host.
//!
//! With no such host there is nowhere local to send it, so the developer upload is the
//! fallback and keeps its confirmation dialog: it explains that
//! the current session's log file will be uploaded to the developer; both buttons
//! (Cancel / Send) close the modal and return to Home. "Send" kicks off a
//! background multipart upload — the same worker-thread + channel shape as the
//! speed test and pairing ceremonies (see `app::state::speedtest`/`pairing`) — whose
//! result lands in the Home status bar. Nothing here blocks the UI thread.
//!
//! Rendering lives in `app::view::sendlogs`.
use crate::app::nav::ScreenKey;
use crate::app::App;
use crate::core::event::MenuEvent;
use crate::core::screen::Screen;
use crate::services::library::{self, LibraryError};
use std::path::Path;

/// The host caps a bundle at 1 MiB; send the newest bytes under it, with headroom for the
/// truncation note.
const MAX_LOG_BYTES: u64 = 768 * 1024;

/// Upload endpoint (see the Go service: POST multipart `file` field to `/upload`).
const UPLOAD_URL: &str = "https://www.upload.dyptan.dev/upload";

/// What the background upload thread reports back — a user-facing status line
/// either way, shown in the Home status bar by `drain_send_logs`.
pub(crate) enum SendLogsMsg {
    Ok(String),
    Err(String),
}

impl App {
    /// The host to send logs to — `None` when the developer fallback (and its confirmation)
    /// is what "Send logs" means right now. A pin is required, not merely used when known:
    /// the mgmt lane authorizes by certificate, and an unverified peer is not who a log
    /// bundle goes to (the rule `power_plan` states for the other write on this lane).
    pub(crate) fn send_logs_host(&self) -> Option<HostTarget> {
        let known = self.reachable_selected_host()?;
        let pin = known.fingerprint?;
        Some(HostTarget {
            name: known.name.clone(),
            addr: known.host.clone(),
            mgmt_port: known.mgmt_port.unwrap_or(library::DEFAULT_MGMT_PORT),
            identity: self.identity.clone(),
            pin,
        })
    }

    /// The Diagnostics row's action: straight to the host when there is one, otherwise the
    /// developer confirmation modal. Either way the outcome shows in the Home status bar,
    /// which is where both paths leave the user.
    pub(crate) fn send_logs_action(&mut self) {
        let Some(target) = self.send_logs_host() else {
            self.open_send_logs();
            return;
        };
        let status = format!("Sending logs to {}…", target.name);
        self.spawn_log_upload(status, move |path| upload_to_host(path, &target));
        self.nav.resume(Screen::Home);
    }

    /// Open the confirmation modal, defaulting focus to Cancel.
    pub(crate) fn open_send_logs(&mut self) {
        self.nav.enter(Screen::SendLogs, 1);
    }

    /// Left/Right toggle Cancel/Send; Confirm acts on the focused button. Both
    /// buttons (and Back) close the modal and return to Home — Send additionally
    /// starts the upload.
    pub(crate) fn handle_send_logs_event(&mut self, ev: MenuEvent) {
        if self.confirm_nav_event(ev) {
            return;
        }
        match ev {
            MenuEvent::Confirm => {
                if self.nav.cursor(ScreenKey::SendLogs) == 0 {
                    self.spawn_log_upload("Sending logs to the developer…".into(), upload_logs);
                }
                self.close_send_logs();
            }
            MenuEvent::Back => self.close_send_logs(),
            MenuEvent::Up | MenuEvent::Down | MenuEvent::Secondary | MenuEvent::Left | MenuEvent::Right => {}
        }
    }

    fn close_send_logs(&mut self) {
        // No cursor reset needed: `open_send_logs` enters at Cancel every time.
        self.nav.resume(Screen::Home);
    }

    /// Spawn the background upload of the on-disk log file, however `work` sends it. Sets
    /// `status` immediately; the outcome replaces it via `drain_send_logs`. Both
    /// destinations run through here, so the job/channel protocol is stated once.
    fn spawn_log_upload(&mut self, status: String, work: impl FnOnce(&Path) -> SendLogsMsg + Send + 'static) {
        let Some(path) = crate::logger::latest_log_file(&crate::services::store::app_dir()) else {
            self.set_home_status(Some("No logs to send yet.".into()), false);
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        self.jobs.send_logs = Some(rx);
        self.set_home_status(Some(status), false);
        tracing::info!("send logs: uploading {}", path.display());
        std::thread::spawn(move || {
            let _ = tx.send(work(&path));
        });
    }

    /// Drain the upload worker's result, if it has landed — called each tick
    /// alongside the other `drain_*`s. Returns whether anything changed.
    pub(crate) fn drain_send_logs(&mut self) -> bool {
        let Some(rx) = &self.jobs.send_logs else { return false };
        match rx.try_recv() {
            Ok(msg) => {
                match msg {
                    SendLogsMsg::Ok(s) => {
                        tracing::info!("send logs: {s}");
                        self.set_home_status(Some(s), false);
                    }
                    SendLogsMsg::Err(s) => {
                        tracing::warn!("send logs failed: {s}");
                        self.set_home_status(Some(s), false);
                    }
                }
                self.jobs.send_logs = None;
                true
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.jobs.send_logs = None;
                false
            }
        }
    }
}

/// Where a host-bound bundle goes and what it travels under, resolved on the UI thread and
/// moved to the worker — `services::power::ExitPlan`'s shape for the other mgmt-lane write.
/// No `Debug`, derived or otherwise: `identity` holds the client key PEM.
pub(crate) struct HostTarget {
    pub(crate) name: String,
    pub(crate) addr: String,
    pub(crate) mgmt_port: u16,
    pub(crate) identity: (String, String),
    pub(crate) pin: [u8; 32],
}

/// The newest [`MAX_LOG_BYTES`] of the log as text, cut at a line boundary so the bundle
/// never opens mid-line, with a note when anything was dropped. Reading the tail rather
/// than the file is what keeps a long session under the host's 1 MiB cap.
fn log_tail(path: &Path) -> Result<String, String> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let unreadable = |e: std::io::Error| format!("Couldn't read the log file: {e}");
    let mut f = std::fs::File::open(path).map_err(unreadable)?;
    let len = f.metadata().map_err(unreadable)?.len();
    let truncated = len > MAX_LOG_BYTES;
    if truncated {
        f.seek(SeekFrom::End(-(MAX_LOG_BYTES as i64))).map_err(unreadable)?;
    }
    let mut raw = Vec::with_capacity(len.min(MAX_LOG_BYTES) as usize);
    f.read_to_end(&mut raw).map_err(unreadable)?;
    // Cut on the raw bytes, before the one conversion: a tail seek lands mid-line (and can
    // land mid-UTF-8, hence lossy), and doing it here keeps the whole tail to one copy.
    let from = if truncated {
        raw.iter().position(|&b| b == b'\n').map_or(0, |i| i + 1)
    } else {
        0
    };
    let text = String::from_utf8_lossy(&raw[from..]);
    if text.trim().is_empty() {
        return Err("No logs to send yet.".into());
    }
    Ok(if truncated {
        format!("… older log lines truncated …\n{text}")
    } else {
        text.into_owned()
    })
}

/// POSTs the log tail to the host's `POST /api/v1/client-logs` as plain text, mTLS-
/// authenticated by this device's paired identity — the wire shape
/// `pf-client-core::logring::send_to_host` defines and the host console lists.
fn upload_to_host(path: &Path, target: &HostTarget) -> SendLogsMsg {
    let body = match log_tail(path) {
        Ok(b) => b,
        Err(e) => return SendLogsMsg::Err(e),
    };
    let agent = match library::agent(&target.identity, Some(target.pin)) {
        Ok(a) => a,
        Err(e) => return SendLogsMsg::Err(format!("Couldn't send logs to {}: {e}", target.name)),
    };
    let url = format!(
        "{}/api/v1/client-logs",
        library::base_url(&target.addr, target.mgmt_port)
    );
    match agent
        .post(url.as_str())
        .header("Content-Type", "text/plain; charset=utf-8")
        .send(body.as_bytes())
    {
        Ok(_) => SendLogsMsg::Ok(format!("Logs sent to {} — thank you!", target.name)),
        Err(e) => match library::classify(e) {
            LibraryError::Http(413) => SendLogsMsg::Err("Log file too large to send (1 MB limit).".into()),
            // `refusal_message`'s NotPaired line is about the power grant; uploading a bundle
            // needs no grant, only a pairing, so this lane words that one itself.
            LibraryError::NotPaired => {
                SendLogsMsg::Err(format!("{} refused the logs — pair with it again.", target.name))
            }
            // Everything else reads the same as any other refusal on this lane, prefixed with
            // the host so the Home bar says which one turned the bundle away.
            other => SendLogsMsg::Err(format!(
                "{}: {}",
                target.name,
                crate::app::view::hostpower::refusal_message(&other)
            )),
        },
    }
}

/// Reads the log file and POSTs it as a multipart `file` field. Runs on the upload
/// worker thread — never on the UI thread. Maps the service's status codes (see the
/// Go handler: 429 rate-limited, 413 too large) to friendly status lines.
fn upload_logs(path: &Path) -> SendLogsMsg {
    const BOUNDARY: &str = "----punktfunkwebos7f3a2c1b8e4d6f0a";
    let data = match std::fs::read(path) {
        Ok(d) if !d.is_empty() => d,
        Ok(_) => return SendLogsMsg::Err("No logs to send yet.".into()),
        Err(e) => return SendLogsMsg::Err(format!("Couldn't read the log file: {e}")),
    };
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("punktfunk-webos.log");

    let mut body = Vec::with_capacity(data.len() + 256);
    body.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n\
             Content-Type: text/plain\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&data);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());

    let content_type = format!("multipart/form-data; boundary={BOUNDARY}");
    let agent = ureq::Agent::new_with_defaults();
    match agent
        .post(UPLOAD_URL)
        .header("Content-Type", &content_type)
        .send(&body[..])
    {
        Ok(_) => SendLogsMsg::Ok("Logs sent to the developer — thank you!".into()),
        Err(ureq::Error::StatusCode(429)) => {
            SendLogsMsg::Err("Rate limited — wait a minute before sending logs again.".into())
        }
        Err(ureq::Error::StatusCode(413)) => SendLogsMsg::Err("Log file too large to send (4 MB limit).".into()),
        Err(ureq::Error::StatusCode(code)) => SendLogsMsg::Err(format!("Upload failed (HTTP {code}).")),
        Err(e) => SendLogsMsg::Err(format!("Couldn't reach the log server: {e}")),
    }
}
