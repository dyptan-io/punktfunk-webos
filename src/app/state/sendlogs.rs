//! The "Send logs to developer" confirmation modal's logic and background upload.
//!
//! Reached from the Diagnostics screen's last row. A warning dialog explains that
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
use std::path::Path;
use std::time::Instant;

/// Upload endpoint (see the Go service: POST multipart `file` field to `/upload`).
const UPLOAD_URL: &str = "https://www.upload.dyptan.dev/upload";

/// What the background upload thread reports back — a user-facing status line
/// either way, shown in the Home status bar by `drain_send_logs`.
pub(crate) enum SendLogsMsg {
    Ok(String),
    Err(String),
}

impl App {
    /// Open the confirmation modal, defaulting focus to Cancel.
    pub(crate) fn open_send_logs(&mut self) {
        self.nav.enter(Screen::SendLogs, 1);
    }

    /// Left/Right toggle Cancel/Send; Confirm acts on the focused button. Both
    /// buttons (and Back) close the modal and return to Home — Send additionally
    /// starts the upload.
    pub(crate) fn handle_send_logs_event(&mut self, ev: MenuEvent) {
        match ev {
            MenuEvent::Left | MenuEvent::Right => {
                self.nav
                    .set_cursor(ScreenKey::SendLogs, 1 - self.nav.cursor(ScreenKey::SendLogs));
                self.render.modal.focus_anim = Some(Instant::now());
            }
            MenuEvent::Confirm => {
                if self.nav.cursor(ScreenKey::SendLogs) == 0 {
                    self.start_log_upload();
                }
                self.close_send_logs();
            }
            MenuEvent::Back => self.close_send_logs(),
            MenuEvent::Up | MenuEvent::Down | MenuEvent::Secondary => {}
        }
    }

    fn close_send_logs(&mut self) {
        // No cursor reset needed: `open_send_logs` enters at Cancel every time.
        self.nav.resume(Screen::Home);
    }

    /// Spawn the background upload of the on-disk log file. Sets an immediate
    /// "sending…" status; the outcome replaces it via `drain_send_logs`.
    fn start_log_upload(&mut self) {
        let Some(path) = crate::logger::latest_log_file(&crate::services::store::app_dir()) else {
            self.set_home_status(Some("No logs to send yet.".into()), false);
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        self.jobs.send_logs = Some(rx);
        self.set_home_status(Some("Sending logs to the developer…".into()), false);
        tracing::info!("send logs: uploading {}", path.display());
        std::thread::spawn(move || {
            let _ = tx.send(upload_logs(&path));
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
