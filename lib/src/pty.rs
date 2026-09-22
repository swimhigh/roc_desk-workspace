//! Local terminal (`portable-pty`, Windows ConPTY under the hood), ported
//! 1:1 from the host's `src-tauri/src/pty/mod.rs`. Remote (SSH/Agent)
//! terminal sessions stay a `roc_desk-ssh` concern -- that tool hasn't been
//! split out of the host yet.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Arc;

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, RwLock};
use uuid::Uuid;

use roc_desk_core::error::AppError;

enum PtyCommand {
    Data(Vec<u8>),
    Resize { rows: u16, cols: u16 },
}

struct PtyChannel {
    cmd_tx: mpsc::UnboundedSender<PtyCommand>,
    /// Dropping `Child` does not kill the underlying process (same semantics
    /// as `std::process::Child`) -- it must be killed explicitly, or closing
    /// a terminal tab would only stop listening for output while the
    /// PowerShell/bash process kept running in the background.
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

/// Local terminal manager: one PTY per open terminal tab. Reads happen on a
/// dedicated OS thread (blocking `read()` can't be awaited directly), writes
/// go through an `mpsc` queue processed by a single task, mirroring the
/// "one task owns the channel" approach used by remote SSH sessions.
pub struct LocalPtyManager {
    channels: RwLock<HashMap<Uuid, PtyChannel>>,
}

impl Default for LocalPtyManager {
    fn default() -> Self {
        Self {
            channels: RwLock::new(HashMap::new()),
        }
    }
}

fn default_shell() -> CommandBuilder {
    #[cfg(target_os = "windows")]
    {
        CommandBuilder::new("powershell.exe")
    }
    #[cfg(not(target_os = "windows"))]
    {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        CommandBuilder::new(shell)
    }
}

impl LocalPtyManager {
    /// Opens a local terminal with `cwd` as its starting directory (mirrors
    /// VS Code's integrated terminal defaulting to the open project's root
    /// rather than the user's home directory).
    pub async fn open(
        &self,
        cwd: String,
        rows: u16,
        cols: u16,
        app_handle: AppHandle,
    ) -> Result<Uuid, AppError> {
        let id = Uuid::new_v4();
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<PtyCommand>();

        let (master, writer, child) =
            tokio::task::spawn_blocking(move || -> Result<_, AppError> {
                let pty_system = native_pty_system();
                let pair = pty_system
                    .openpty(PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    })
                    .map_err(|e| AppError::Internal(format!("open pty failed: {e}")))?;

                let mut cmd = default_shell();
                cmd.cwd(&cwd);
                let child = pair
                    .slave
                    .spawn_command(cmd)
                    .map_err(|e| AppError::Internal(format!("spawn shell failed: {e}")))?;
                // The parent side no longer needs the slave end once the
                // child owns its own reference; not dropping it means the
                // master-side reader would never see EOF.
                drop(pair.slave);

                let reader = pair
                    .master
                    .try_clone_reader()
                    .map_err(|e| AppError::Internal(format!("clone pty reader failed: {e}")))?;
                let writer = pair
                    .master
                    .take_writer()
                    .map_err(|e| AppError::Internal(format!("take pty writer failed: {e}")))?;

                spawn_reader_thread(id, reader, app_handle.clone());
                Ok((pair.master, writer, child))
            })
            .await
            .map_err(|e| AppError::Internal(e.to_string()))??;

        let mut writer = writer;
        tokio::spawn(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    PtyCommand::Data(bytes) => {
                        if tokio::task::block_in_place(|| writer.write_all(&bytes)).is_err() {
                            break;
                        }
                    }
                    PtyCommand::Resize { rows, cols } => {
                        let _ = master.resize(PtySize {
                            rows,
                            cols,
                            pixel_width: 0,
                            pixel_height: 0,
                        });
                    }
                }
            }
        });

        self.channels
            .write()
            .await
            .insert(id, PtyChannel { cmd_tx, child });
        Ok(id)
    }

    pub async fn write(&self, id: Uuid, data: Vec<u8>) -> Result<(), AppError> {
        let channels = self.channels.read().await;
        let ch = channels
            .get(&id)
            .ok_or_else(|| AppError::NotFound(format!("pty channel not found: {id}")))?;
        ch.cmd_tx
            .send(PtyCommand::Data(data))
            .map_err(|_| AppError::Internal("pty task has stopped".into()))
    }

    pub async fn resize(&self, id: Uuid, rows: u16, cols: u16) -> Result<(), AppError> {
        let channels = self.channels.read().await;
        let ch = channels
            .get(&id)
            .ok_or_else(|| AppError::NotFound(format!("pty channel not found: {id}")))?;
        ch.cmd_tx
            .send(PtyCommand::Resize { rows, cols })
            .map_err(|_| AppError::Internal("pty task has stopped".into()))
    }

    pub async fn close(&self, id: Uuid) -> Result<(), AppError> {
        if let Some(mut ch) = self.channels.write().await.remove(&id) {
            let _ = tokio::task::spawn_blocking(move || ch.child.kill()).await;
        }
        Ok(())
    }
}

fn spawn_reader_thread(id: Uuid, mut reader: Box<dyn Read + Send>, app_handle: AppHandle) {
    std::thread::spawn(move || {
        // The frontend needs an IPC round-trip plus a React mount before it
        // has `listen("pty:data", ...)` wired up; a short delay avoids the
        // shell's first prompt/MOTD being emitted into the void before
        // anyone is listening.
        std::thread::sleep(std::time::Duration::from_millis(200));
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let _ = app_handle.emit(
                        "pty:data",
                        serde_json::json!({ "channelId": id, "data": buf[..n].to_vec() }),
                    );
                }
                Err(_) => break,
            }
        }
        let _ = app_handle.emit(
            "pty:status",
            serde_json::json!({ "channelId": id, "status": "disconnected" }),
        );
    });
}

pub type SharedLocalPtyManager = Arc<LocalPtyManager>;
