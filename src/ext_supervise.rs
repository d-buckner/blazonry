// SPDX-License-Identifier: AGPL-3.0-only
//! The server supervises the extension host process (ADR-0004).
//!
//! ADR-0002 put plugin loading in a separate process, because a plugin that
//! segfaults must not take the server down. It assumed a separate container to
//! supply the restart, and nothing in blazonry brings the host back: a panel that
//! lost its host stayed lost until someone restarted a container.
//!
//! One container changes who supplies the restart, not whether the process
//! boundary exists. The server is PID 1 here, so it owns the child's lifecycle:
//! it starts the host, restarts it with backoff when it dies, and takes it down
//! when the server is asked to stop. That is more than a container restart policy
//! did, because a policy cannot tell a plugin that crashed once from a plugin that
//! crashes every time it is loaded.
//!
//! The backoff is what makes a permanently bad plugin visible instead of hot. A
//! child that survives `STABLE_SECS` counts as healthy and the delay resets, so
//! three restarts spread over an afternoon are not the same event as three in a
//! second.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Notify;

/// A child that lived this long is considered healthy, so the backoff resets.
const STABLE_SECS: u64 = 60;
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// The delay after the nth consecutive restart, before a child counts as stable.
fn backoff(consecutive: u32) -> Duration {
    (BACKOFF_MIN * (1u32 << consecutive.min(5))).min(BACKOFF_MAX)
}

/// A running supervisor. Dropping it does not stop the child; call `stop`.
pub struct Supervisor {
    stop: Arc<Notify>,
}

impl Supervisor {
    /// Start the host and keep it running until `stop` is called.
    pub fn start(program: PathBuf, args: Vec<String>) -> Supervisor {
        let stop = Arc::new(Notify::new());
        let watch = stop.clone();
        tokio::spawn(async move {
            let mut consecutive = 0u32;
            loop {
                let started = Instant::now();
                tracing::info!(program = ?program, ?args, "extension host: starting");
                let mut child = match tokio::process::Command::new(&program)
                    .args(&args)
                    .kill_on_drop(true)
                    .spawn()
                {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!(%e, program = ?program, "extension host: cannot start");
                        // Nothing is running, so the loop would spin without a
                        // wait. Back off and try again rather than give up.
                        let wait = backoff(consecutive);
                        tokio::select! {
                            _ = tokio::time::sleep(wait) => {}
                            _ = watch.notified() => return,
                        }
                        consecutive += 1;
                        continue;
                    }
                };
                let pid = child.id().unwrap_or(0);
                tracing::info!(pid, "extension host: running");
                tokio::select! {
                    status = child.wait() => match status {
                        Ok(s) => tracing::warn!(?s, pid, "extension host exited"),
                        Err(e) => tracing::error!(%e, pid, "extension host: wait failed"),
                    },
                    _ = watch.notified() => {
                        // A container signals PID 1, and PID 1's children do not
                        // get it, so the host has to be taken down here or it
                        // outlives the server and the next start finds a stale
                        // socket behind it.
                        tracing::info!(pid, "extension host: stopping with the server");
                        let _ = child.start_kill();
                        let _ = child.wait().await;
                        return;
                    }
                }
                if started.elapsed() >= Duration::from_secs(STABLE_SECS) {
                    consecutive = 0;
                } else {
                    consecutive += 1;
                    if consecutive == 3 {
                        tracing::warn!(
                            program = ?program,
                            "extension host has restarted 3 times without staying up; a plugin \
                             that fails to load will loop here"
                        );
                    }
                }
                let wait = backoff(consecutive);
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = watch.notified() => return,
                }
            }
        });
        Supervisor { stop }
    }

    /// Stop restarting, and take the running child down with the server.
    pub fn stop(&self) {
        self.stop.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_clamps() {
        assert_eq!(backoff(0), BACKOFF_MIN);
        assert!(backoff(1) > backoff(0));
        assert!(backoff(2) > backoff(1));
        assert_eq!(backoff(9), BACKOFF_MAX, "the delay must stop growing");
    }

    /// The property the whole module exists for: a child that dies comes back, and
    /// `stop` ends the loop instead of a restart.
    #[tokio::test]
    async fn a_child_that_exits_is_started_again() {
        let dir = std::env::temp_dir().join(format!("blazonry-sup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let counter = dir.join("count");
        let _ = std::fs::remove_file(&counter);
        // Each run appends a line, so the file length is the number of starts.
        let script = dir.join("tick.sh");
        std::fs::write(
            &script,
            format!("#!/bin/sh\necho x >> '{}'\n", counter.display()),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();

        let sup = Supervisor::start(script.clone(), vec![]);
        // BACKOFF_MIN is 1s, so a few seconds is several starts.
        tokio::time::sleep(Duration::from_millis(2600)).await;
        sup.stop();
        tokio::time::sleep(Duration::from_millis(300)).await;

        let starts = std::fs::read_to_string(&counter).unwrap().lines().count();
        assert!(
            (2..=6).contains(&starts),
            "expected the child to be restarted a few times, saw {starts}"
        );
        // After stop, nothing more may be appended.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let after = std::fs::read_to_string(&counter).unwrap().lines().count();
        assert_eq!(starts, after, "the supervisor kept restarting after stop");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
