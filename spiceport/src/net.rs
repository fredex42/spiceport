//! The network side: a tokio runtime on a background thread running the
//! SPICE session, so SDL can keep the main thread (macOS requires it).

use std::io::IsTerminal;
use std::sync::Arc;
use std::thread::JoinHandle;

use log::{error, info};
use spice_client::{ClientCommand, EventSink, SessionConfig};
use tokio::sync::mpsc;

pub struct NetThread {
    pub commands: mpsc::UnboundedSender<ClientCommand>,
    handle: JoinHandle<Result<(), spice_client::ClientError>>,
}

impl NetThread {
    /// Start the session. `on_exit` runs on the network thread when the
    /// session ends, to wake the UI.
    pub fn spawn(
        mut cfg: SessionConfig,
        events: Arc<dyn EventSink>,
        on_exit: impl FnOnce() + Send + 'static,
    ) -> anyhow::Result<Self> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("spice-net")
            .build()?;
        let quit = tx.clone();
        let handle = std::thread::Builder::new()
            .name("spice-session".into())
            .spawn(move || {
                let r = rt.block_on(async move {
                    tokio::spawn(async move {
                        if tokio::signal::ctrl_c().await.is_ok() {
                            info!("interrupted");
                            let _ = quit.send(ClientCommand::Quit);
                        }
                    });
                    let mut prompted = false;
                    loop {
                        match spice_client::run(cfg.clone(), events.clone(), &mut rx).await {
                            Err(e)
                                if e.is_auth_failure()
                                    && !prompted
                                    && std::io::stdin().is_terminal() =>
                            {
                                prompted = true;
                                error!("{e}");
                                let pw = tokio::task::spawn_blocking(|| {
                                    rpassword::prompt_password("Password: ")
                                })
                                .await
                                .expect("password prompt panicked");
                                match pw {
                                    Ok(pw) => cfg.password = Some(pw),
                                    Err(_) => return Err(e),
                                }
                            }
                            r => return r,
                        }
                    }
                });
                on_exit();
                // Don't wait for stray blocking tasks (e.g. stdin readers).
                rt.shutdown_background();
                r
            })?;
        Ok(Self {
            commands: tx,
            handle,
        })
    }

    pub fn send(&self, cmd: ClientCommand) {
        let _ = self.commands.send(cmd);
    }

    pub fn join(self) -> Result<(), spice_client::ClientError> {
        self.handle.join().unwrap_or(Ok(()))
    }

    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }
}
