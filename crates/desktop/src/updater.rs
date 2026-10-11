//! In-app updates for the installed desktop apps.
//!
//! Checks the signed channel 15 s after start, every 6 h and whenever the user clicks
//! "Verificar atualizações". A newer verified release is downloaded in the background
//! into `%LOCALAPPDATA%\Aionix Connect\updates`; the app decides when to apply it
//! (the host only with no session or prompt open, the technician when the user agrees).
//! Applying runs the verified setup with `--update --wait-pid <pid>` and closes the app;
//! the setup swaps the files and starts the new version.
//!
//! Development builds (no `AIONIX_UPDATE_BASE`) never check.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aionix_update::{Manifest, Product, UpdateClient, Version};
use egui::{Color32, RichText, Ui};
use tokio::sync::Notify;
use tracing::{info, warn};

use crate::common::{self, UPDATE_BASE, VERSION};
use crate::theme::Palette;

const FIRST_CHECK: Duration = Duration::from_secs(15);
const PERIOD: Duration = Duration::from_secs(6 * 3600);

#[derive(Debug, Clone)]
pub enum Phase {
    /// Development build: no update channel compiled in.
    Disabled,
    Idle,
    Checking,
    UpToDate,
    Downloading {
        version: Version,
        fraction: f32,
    },
    Ready {
        manifest: Manifest,
        installer: PathBuf,
    },
    Failed(String),
}

struct Inner {
    phase: Mutex<Phase>,
    last_check: Mutex<Option<Instant>>,
    wake: Notify,
    ctx: egui::Context,
}

/// Handle shared with the UI.
#[derive(Clone)]
pub struct Updater {
    inner: Arc<Inner>,
}

impl Updater {
    /// Starts the background checker for `product`.
    pub fn start(rt: &tokio::runtime::Handle, product: Product, ctx: egui::Context) -> Self {
        let inner = Arc::new(Inner {
            phase: Mutex::new(Phase::Idle),
            last_check: Mutex::new(None),
            wake: Notify::new(),
            ctx,
        });
        let updater = Self {
            inner: inner.clone(),
        };
        let (Some(base), Ok(current)) = (UPDATE_BASE, VERSION.parse::<Version>()) else {
            *inner.phase.lock().unwrap() = Phase::Disabled;
            return updater;
        };
        let client = match UpdateClient::new(base, product, current) {
            Ok(c) => c,
            Err(e) => {
                *inner.phase.lock().unwrap() = Phase::Failed(e.to_string());
                return updater;
            }
        };
        let dir = updates_dir();
        clean_downloads(&dir, current);
        rt.spawn(async move {
            tokio::select! {
                _ = tokio::time::sleep(FIRST_CHECK) => {}
                _ = inner.wake.notified() => {}
            }
            loop {
                run_check(&inner, &client, &dir).await;
                tokio::select! {
                    _ = tokio::time::sleep(PERIOD) => {}
                    _ = inner.wake.notified() => {}
                }
            }
        });
        updater
    }

    pub fn phase(&self) -> Phase {
        self.inner.phase.lock().unwrap().clone()
    }

    pub fn last_check(&self) -> Option<Instant> {
        *self.inner.last_check.lock().unwrap()
    }

    /// "Verificar atualizações".
    pub fn check_now(&self) {
        let mut phase = self.inner.phase.lock().unwrap();
        if matches!(
            *phase,
            Phase::Disabled | Phase::Checking | Phase::Downloading { .. }
        ) {
            return;
        }
        if !matches!(*phase, Phase::Ready { .. }) {
            *phase = Phase::Checking;
        }
        drop(phase);
        self.inner.wake.notify_one();
        self.inner.ctx.request_repaint();
    }

    /// Verified update waiting to be installed.
    pub fn ready(&self) -> Option<Manifest> {
        match &*self.inner.phase.lock().unwrap() {
            Phase::Ready { manifest, .. } => Some(manifest.clone()),
            _ => None,
        }
    }

    /// Starts the verified setup in update mode. On success the caller must close the
    /// app right away (the setup waits for this process to exit). `minimized`: the
    /// window was minimized, so the new version starts minimized too.
    pub fn apply(&self, minimized: bool) -> Result<(), String> {
        let Phase::Ready {
            manifest,
            installer,
        } = self.phase()
        else {
            return Err("nenhuma atualização pronta".into());
        };
        // Re-verify right before executing: the file sat on disk since the download.
        match aionix_update::sha256_file(&installer) {
            Ok(h) if h == manifest.installer.sha256 => {}
            _ => {
                let _ = std::fs::remove_file(&installer);
                *self.inner.phase.lock().unwrap() =
                    Phase::Failed("o instalador baixado foi alterado; baixe novamente".into());
                return Err("instalador alterado".into());
            }
        }
        info!(version = %manifest.version, "applying update");
        let mut cmd = std::process::Command::new(&installer);
        cmd.arg("--update")
            .arg("--wait-pid")
            .arg(std::process::id().to_string());
        if minimized {
            cmd.arg("--minimized");
        }
        cmd.spawn()
            .map(|_| ())
            .map_err(|e| format!("não foi possível iniciar o instalador: {e}"))
    }
}

async fn run_check(inner: &Inner, client: &UpdateClient, dir: &Path) {
    if matches!(*inner.phase.lock().unwrap(), Phase::Ready { .. }) {
        return;
    }
    let set = |p: Phase| {
        *inner.phase.lock().unwrap() = p;
        inner.ctx.request_repaint();
    };
    set(Phase::Checking);
    let result = client.check().await;
    *inner.last_check.lock().unwrap() = Some(Instant::now());
    let manifest = match result {
        Ok(None) => return set(Phase::UpToDate),
        Ok(Some(m)) => m,
        Err(e) => {
            warn!(error = %e, "update check failed");
            return set(Phase::Failed(e.to_string()));
        }
    };
    info!(version = %manifest.version, "update available; downloading");
    let version = manifest.version;
    set(Phase::Downloading {
        version,
        fraction: 0.0,
    });
    let mut last = Instant::now();
    let result = client
        .download(&manifest, dir, |done, total| {
            if last.elapsed() > Duration::from_millis(150) || done == total {
                last = Instant::now();
                let fraction = if total > 0 {
                    done as f32 / total as f32
                } else {
                    0.0
                };
                *inner.phase.lock().unwrap() = Phase::Downloading { version, fraction };
                inner.ctx.request_repaint();
            }
        })
        .await;
    match result {
        Ok(installer) => {
            info!(version = %version, path = %installer.display(), "update ready");
            set(Phase::Ready {
                manifest,
                installer,
            });
        }
        Err(e) => {
            warn!(error = %e, "update download failed");
            set(Phase::Failed(e.to_string()));
        }
    }
}

/// `%LOCALAPPDATA%\Aionix Connect\updates`.
pub fn updates_dir() -> PathBuf {
    common::data_dir("updates", None)
}

/// Removes partial downloads and installers that are not newer than this build.
fn clean_downloads(dir: &Path, current: Version) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let stale = name.ends_with(".part")
            || name
                .strip_suffix(".exe")
                .and_then(|n| n.rsplit('-').next())
                .and_then(|v| v.parse::<Version>().ok())
                .is_some_and(|v| v <= current);
        if stale {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// One-line footer: "Versão 0.2.0 · <status> · [Verificar atualizações]".
pub fn footer(ui: &mut Ui, updater: &Updater, p: &Palette) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(
            RichText::new(format!("Versão {VERSION}"))
                .size(12.0)
                .color(p.muted),
        );
        let (text, color): (String, Color32) = match updater.phase() {
            Phase::Disabled => ("build de desenvolvimento".into(), p.muted),
            Phase::Idle => (String::new(), p.muted),
            Phase::Checking => ("verificando atualizações…".into(), p.muted),
            Phase::UpToDate => ("atualizado".into(), p.success),
            Phase::Downloading { version, fraction } => (
                format!("baixando {version} ({:.0}%)", fraction * 100.0),
                p.info,
            ),
            Phase::Ready { manifest, .. } => (
                format!("versão {} pronta para instalar", manifest.version),
                p.accent,
            ),
            Phase::Failed(_) => ("não foi possível verificar atualizações".into(), p.warning),
        };
        if !text.is_empty() {
            ui.label(RichText::new("·").size(12.0).color(p.muted));
            let r = ui.label(RichText::new(text).size(12.0).color(color));
            if let Phase::Failed(e) = updater.phase() {
                r.on_hover_text(e);
            }
        }
        let busy = matches!(
            updater.phase(),
            Phase::Checking | Phase::Downloading { .. } | Phase::Disabled
        );
        if !busy {
            ui.label(RichText::new("·").size(12.0).color(p.muted));
            let link = ui.add(
                egui::Label::new(
                    RichText::new("Verificar atualizações")
                        .size(12.0)
                        .color(p.accent)
                        .underline(),
                )
                .sense(egui::Sense::click()),
            );
            if link
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                updater.check_now();
            }
        }
    });
}
