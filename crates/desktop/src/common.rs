//! Pieces shared by the Technician and host applications: data directories, logging,
//! the device transport bound to the enrolled identity, and pt-BR user-facing text.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use aionix_client::{ClientError, enroll::DeviceSession};
use aionix_control_api::ErrorCode;
use aionix_protocol::ALPN;
use aionix_remote_core::session::state::{RejectReason, SessionState, SessionStatus};
use aionix_transport::{RelayEndpoint, Transport, TransportConfig};
use tracing::{info, warn};

/// Server compiled into release builds (`AIONIX_DEFAULT_SERVER`, set by
/// infra/release.ps1); the local development stack otherwise. `--server` and the
/// host's `aionix-host.json` still override it.
pub const DEFAULT_SERVER: &str = match option_env!("AIONIX_DEFAULT_SERVER") {
    Some(s) => s,
    None => "http://127.0.0.1:8080",
};

/// Update channel base URL (`AIONIX_UPDATE_BASE`); development builds have none and
/// never look for updates.
pub const UPDATE_BASE: Option<&str> = option_env!("AIONIX_UPDATE_BASE");

/// Version of this build (the workspace version).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// `%LOCALAPPDATA%\Aionix Connect\<app>` unless the launcher passes `--data-dir`.
pub fn data_dir(app: &str, explicit: Option<PathBuf>) -> PathBuf {
    explicit.unwrap_or_else(|| {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(&aionix_brand::brand().identifiers.data_dir_name)
            .join(app)
    })
}

/// Logs to `<dir>/logs/<app>.log` (and stderr when a console is attached).
pub fn init_logging(dir: &Path, app: &str) -> PathBuf {
    let logs = dir.join("logs");
    let _ = std::fs::create_dir_all(&logs);
    let path = logs.join(format!("{app}.log"));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path);
    let filter = tracing_subscriber::EnvFilter::try_from_env("AIONIX_LOG").unwrap_or_else(|_| {
        "info,aionix_remote_core=info,iroh=warn,iroh_relay=warn,wgpu=warn,naga=warn,eframe=warn"
            .into()
    });
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_target(true);
    match file {
        Ok(f) => {
            let _ = builder.with_writer(std::sync::Mutex::new(f)).try_init();
        }
        Err(_) => {
            let _ = builder.try_init();
        }
    }
    std::panic::set_hook(Box::new(|info| {
        tracing::error!(panic = %info, "panic");
    }));
    path
}

fn relay_endpoints(grants: &[aionix_control_api::rest::RelayGrant]) -> Vec<RelayEndpoint> {
    grants
        .iter()
        .map(|g| RelayEndpoint {
            url: g.url.clone(),
            token: Some(g.token.clone()),
            qad_port: None,
        })
        .collect()
}

/// Binds the QUIC endpoint to the device's enrolled key (the host's key is what tickets
/// name; the workstation's key is what the host checks the technician against), with
/// the relay grants issued at device login. Keeps the grants fresh in the background.
pub async fn bind_transport(session: Arc<DeviceSession>) -> Result<Transport, String> {
    let grants = session.relays().await.map_err(|e| e.to_string())?;
    let transport = Transport::bind(TransportConfig {
        secret_key: *session.key().secret_bytes(),
        relays: relay_endpoints(&grants),
        alpns: vec![ALPN.to_vec()],
        port: 0,
        relay_only: false,
    })
    .await
    .map_err(|e| e.to_string())?;
    if !transport.wait_online(Duration::from_secs(10)).await {
        warn!("relay not reachable yet; continuing with direct paths");
    }
    let t = transport.clone();
    tokio::spawn(async move {
        loop {
            let expires = grants_min_expiry(&session).await;
            tokio::time::sleep(
                expires
                    .saturating_sub(Duration::from_secs(120))
                    .max(Duration::from_secs(30)),
            )
            .await;
            session.invalidate().await;
            match session.relays().await {
                Ok(fresh) => {
                    for r in relay_endpoints(&fresh) {
                        if let Err(e) = t.update_relay_token(&r).await {
                            warn!(error = %e, "relay token update failed");
                        }
                    }
                    info!("relay grants renewed");
                }
                Err(e) => warn!(error = %e, "relay grant renewal failed"),
            }
        }
    });
    Ok(transport)
}

async fn grants_min_expiry(session: &DeviceSession) -> Duration {
    session
        .relays()
        .await
        .ok()
        .and_then(|g| g.iter().map(|r| r.expires_in).min())
        .map(|s| Duration::from_secs(s.max(60) as u64))
        .unwrap_or(Duration::from_secs(600))
}

/// pt-BR description of a session state for status lines.
pub fn state_text(state: SessionState) -> &'static str {
    match state {
        SessionState::Idle | SessionState::Requesting => "Conectando...",
        SessionState::Authenticating => "Autenticando...",
        SessionState::AwaitingApproval => "Aguardando autorização...",
        SessionState::Establishing => "Estabelecendo conexão segura...",
        SessionState::Connected => "Conectado",
        SessionState::Degraded => "Conectado (rede instável)",
        SessionState::Reconnecting => "Reconectando...",
        SessionState::Rejected => "Acesso recusado",
        SessionState::Failed => "Erro ao conectar",
        SessionState::Ending => "Encerrando...",
        SessionState::Ended => "Sessão encerrada",
    }
}

/// Human explanation of a terminal status (pt-BR).
pub fn outcome_text(status: &SessionStatus) -> String {
    match (&status.state, &status.reject_reason, &status.failure) {
        (SessionState::Rejected, Some(r), _) => match r {
            RejectReason::UserRejected => "O usuário do computador recusou o acesso.".into(),
            RejectReason::ApprovalTimeout => {
                "Ninguém respondeu à solicitação no computador remoto a tempo.".into()
            }
            RejectReason::Busy => "O dispositivo já está em outra sessão.".into(),
            RejectReason::InvalidTicket(_) => {
                "O dispositivo não aceitou a autorização desta sessão.".into()
            }
            RejectReason::ApprovalUnavailable(_) => {
                "Não foi possível exibir a solicitação no computador remoto.".into()
            }
            RejectReason::Policy(p) => policy_text(p),
        },
        (SessionState::Failed, _, Some(f)) => failure_text(f),
        (SessionState::Ended, _, _) => "A sessão foi encerrada.".into(),
        _ => String::new(),
    }
}

fn policy_text(p: &str) -> String {
    let lower = p.to_lowercase();
    if lower.contains("busy") {
        "O dispositivo já está em outra sessão.".into()
    } else if lower.contains("permission") || lower.contains("forbidden") {
        "Você não tem permissão para acessar este dispositivo.".into()
    } else {
        format!("Acesso não autorizado ({p}).")
    }
}

fn failure_text(f: &str) -> String {
    let lower = f.to_lowercase();
    if lower.contains("offline") {
        "Dispositivo offline.".into()
    } else if lower.contains("not found") || lower.contains("devicenotfound") {
        "Dispositivo não encontrado.".into()
    } else if lower.contains("did not answer")
        || lower.contains("timeout")
        || lower.contains("timed out")
    {
        "O dispositivo não respondeu a tempo.".into()
    } else if lower.contains("could not resume") || lower.contains("refused to resume") {
        "A conexão caiu e não pôde ser retomada.".into()
    } else if lower.contains("ended by the") || lower.contains("host ended") {
        "A sessão foi encerrada.".into()
    } else {
        format!("Erro ao conectar: {f}")
    }
}

/// pt-BR message for a Control Plane client error.
pub fn client_error_text(e: &ClientError) -> String {
    match e {
        ClientError::Network(_) => "Servidor indisponível. Verifique a conexão.".into(),
        ClientError::Api { code, detail, .. } => match code {
            ErrorCode::InvalidCredentials => "E-mail ou senha incorretos.".into(),
            ErrorCode::AccountLocked => {
                "Conta bloqueada temporariamente após várias tentativas. Tente mais tarde.".into()
            }
            ErrorCode::Unauthorized | ErrorCode::TokenExpired => {
                "Sua sessão expirou. Entre novamente.".into()
            }
            ErrorCode::EnrollmentInvalid => "Código de cadastro inválido ou expirado.".into(),
            ErrorCode::DeviceBusy => "O dispositivo já está em outra sessão.".into(),
            ErrorCode::DeviceTimeout => "O dispositivo não respondeu a tempo.".into(),
            ErrorCode::Unavailable | ErrorCode::Internal => {
                "Servidor temporariamente indisponível.".into()
            }
            ErrorCode::RateLimited => "Muitas tentativas. Aguarde alguns instantes.".into(),
            ErrorCode::DeviceRevoked => "Este dispositivo foi revogado pelo administrador.".into(),
            ErrorCode::DeviceOffline => "Dispositivo offline.".into(),
            ErrorCode::NotFound => "Não encontrado.".into(),
            ErrorCode::Forbidden => "Sem permissão para esta ação.".into(),
            _ => format!("O servidor recusou a solicitação: {detail}"),
        },
        ClientError::Decode(d) => format!("Resposta inesperada do servidor: {d}"),
        ClientError::Local(d) => d.clone(),
    }
}

/// `482 193 771` style display of a friendly id.
pub fn friendly(id: aionix_shared::FriendlyId) -> String {
    id.to_string()
}
