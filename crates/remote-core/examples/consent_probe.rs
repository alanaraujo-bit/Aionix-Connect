//! Shows the real host approval prompt and prints the decision.
//!
//! cargo run -p aionix-remote-core --example consent_probe [-- <timeout seconds>]
//!
//! Nothing is approved automatically: click "Permitir" or "Recusar", or let it time out.

#[cfg(windows)]
#[tokio::main]
async fn main() {
    use aionix_remote_core::session::{
        Permissions,
        approval::{ApprovalPrompt, ApprovalRequest, WindowsApprovalPrompt, request_approval},
    };
    use std::time::Duration;

    tracing_subscriber::fmt().with_env_filter("info").init();
    aionix_platform::windows::init_dpi_awareness();
    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    let request = ApprovalRequest {
        session_id: uuid::Uuid::new_v4(),
        technician_name: "Ana Souza".into(),
        technician_email: "ana.souza@suporte-exemplo.com.br".into(),
        organization: Some("Suporte Exemplo & Cia".into()),
        permissions: Permissions::all(),
        timeout: Duration::from_secs(secs),
    };
    let prompt: &dyn ApprovalPrompt = &WindowsApprovalPrompt;
    let decision = request_approval(prompt, &request).await;
    println!("decision: {decision:?}");
}

#[cfg(not(windows))]
fn main() {}
