//! Visible local approval of assisted sessions.
//!
//! An assisted session never reaches `Establishing` without an explicit
//! [`ApprovalDecision::Approved`] from an [`ApprovalPrompt`]. Implementations must show
//! the request to the person in front of the host and only report `Approved` after a
//! deliberate action ("Permitir"). There is no auto-approving implementation in this
//! crate; test doubles live in the test suites.
//!
//! The coordinator enforces the timeout itself ([`request_approval`]), so a prompt that
//! never answers results in [`ApprovalDecision::TimedOut`], and dropping the future
//! (technician cancelled, host shutting down) must withdraw the prompt.

use std::time::Duration;

use async_trait::async_trait;
use uuid::Uuid;

use super::ticket::Permissions;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequest {
    pub session_id: Uuid,
    /// From the signed ticket.
    pub technician_name: String,
    pub technician_email: String,
    pub organization: Option<String>,
    pub permissions: Permissions,
    /// The request is refused automatically after this long.
    pub timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    Approved,
    Rejected,
    TimedOut,
    /// The prompt could not be displayed. Treated as a refusal.
    Unavailable(String),
}

#[async_trait]
pub trait ApprovalPrompt: Send + Sync {
    /// Shows the request and waits for the local user's answer.
    async fn ask(&self, request: &ApprovalRequest) -> ApprovalDecision;
}

/// Grace added to the prompt's own countdown before the coordinator gives up on it.
const TIMEOUT_SLACK: Duration = Duration::from_millis(500);

/// Asks `prompt`, enforcing `request.timeout`.
pub async fn request_approval(
    prompt: &dyn ApprovalPrompt,
    request: &ApprovalRequest,
) -> ApprovalDecision {
    match tokio::time::timeout(request.timeout + TIMEOUT_SLACK, prompt.ask(request)).await {
        Ok(decision) => decision,
        Err(_) => ApprovalDecision::TimedOut,
    }
}

/// User-facing strings of the consent prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentText {
    pub title: String,
    pub heading: String,
    /// Who is asking (sanitized name).
    pub requester: String,
    /// E-mail and organization, " · " separated (sanitized).
    pub requester_detail: String,
    /// What allowing means.
    pub body: String,
    /// The emphasized last line.
    pub caution: String,
    pub allow: String,
    pub deny: String,
    /// `{s}` is replaced by the remaining seconds.
    pub countdown: String,
}

/// Brazilian Portuguese prompt text (primary product language).
pub fn consent_text_pt_br(request: &ApprovalRequest) -> ConsentText {
    let name = sanitize(&request.technician_name);
    let name = if name.is_empty() {
        "Técnico não identificado".to_owned()
    } else {
        name
    };
    let mut detail = Vec::new();
    let email = sanitize(&request.technician_email);
    if !email.is_empty() {
        detail.push(email);
    }
    if let Some(org) = request.organization.as_deref().map(sanitize)
        && !org.is_empty()
    {
        detail.push(org);
    }
    let mut can = vec!["ver a sua tela"];
    if request.permissions.control {
        can.push("usar o mouse e o teclado");
    }
    if request.permissions.clipboard {
        can.push("trocar textos copiados (área de transferência)");
    }
    ConsentText {
        title: aionix_brand::brand().product_name.clone(),
        heading: "Um técnico quer acessar este computador".into(),
        requester: name,
        requester_detail: detail.join(" · "),
        body: format!(
            "Se você permitir, o técnico poderá {} até o fim da sessão. Você pode encerrar o acesso a qualquer momento.",
            join_pt(&can)
        ),
        caution: "Permita somente se você reconhece esta solicitação.".into(),
        allow: "Permitir".into(),
        deny: "Recusar".into(),
        countdown: "Recusa automática em {s} s".into(),
    }
}

fn join_pt(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [init @ .., last] => format!("{} e {last}", init.join(", ")),
    }
}

/// Ticket fields are shown verbatim to the user: strip control and bidirectional
/// override characters (no fake extra lines or reversed text) and bound the length.
fn sanitize(s: &str) -> String {
    const MAX_CHARS: usize = 80;
    let clean: String = s
        .chars()
        .filter(|c| {
            !c.is_control() && !matches!(*c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
        })
        .collect();
    let clean = clean.trim();
    if clean.chars().count() > MAX_CHARS {
        let mut cut: String = clean.chars().take(MAX_CHARS - 1).collect();
        cut.push('…');
        cut
    } else {
        clean.to_owned()
    }
}

/// Native Windows consent window (see `aionix_platform::windows::consent`).
#[cfg(windows)]
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsApprovalPrompt;

#[cfg(windows)]
#[async_trait]
impl ApprovalPrompt for WindowsApprovalPrompt {
    async fn ask(&self, request: &ApprovalRequest) -> ApprovalDecision {
        use aionix_platform::windows::consent::{ConsentPrompt, ConsentResult, ConsentWindow};

        let text = consent_text_pt_br(request);
        let (tx, rx) = tokio::sync::oneshot::channel();
        let window = ConsentWindow::show(
            ConsentPrompt {
                window_title: text.title,
                heading: text.heading,
                requester: text.requester,
                requester_detail: text.requester_detail,
                body: text.body,
                caution: text.caution,
                allow_label: text.allow,
                deny_label: text.deny,
                countdown_template: text.countdown,
                timeout: request.timeout,
                // Guards against a click or key press that was meant for something else.
                allow_delay: Duration::from_millis(1500),
            },
            move |result| {
                let _ = tx.send(result);
            },
        );
        let window = match window {
            Ok(w) => w,
            Err(e) => return ApprovalDecision::Unavailable(e.to_string()),
        };
        // If this future is dropped, `window` is dropped too and withdraws the prompt.
        let result = rx.await;
        drop(window);
        match result {
            Ok(ConsentResult::Allowed) => ApprovalDecision::Approved,
            Ok(ConsentResult::Denied | ConsentResult::Dismissed) => ApprovalDecision::Rejected,
            Ok(ConsentResult::TimedOut) => ApprovalDecision::TimedOut,
            Ok(ConsentResult::Failed(e)) => ApprovalDecision::Unavailable(e),
            Err(_) => ApprovalDecision::Unavailable("consent window vanished".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    fn request(timeout: Duration) -> ApprovalRequest {
        ApprovalRequest {
            session_id: Uuid::new_v4(),
            technician_name: "Ana Souza".into(),
            technician_email: "ana@example.com".into(),
            organization: Some("Suporte Exemplo".into()),
            permissions: Permissions::all(),
            timeout,
        }
    }

    struct Answer(ApprovalDecision);
    #[async_trait]
    impl ApprovalPrompt for Answer {
        async fn ask(&self, _: &ApprovalRequest) -> ApprovalDecision {
            self.0.clone()
        }
    }

    /// Never answers; records whether it was withdrawn (dropped).
    struct Silent(Arc<AtomicBool>);
    #[async_trait]
    impl ApprovalPrompt for Silent {
        async fn ask(&self, _: &ApprovalRequest) -> ApprovalDecision {
            struct Withdraw(Arc<AtomicBool>);
            impl Drop for Withdraw {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::SeqCst);
                }
            }
            let _guard = Withdraw(self.0.clone());
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn approval_accepted_and_rejected_are_passed_through() {
        let r = request(Duration::from_secs(5));
        assert_eq!(
            request_approval(&Answer(ApprovalDecision::Approved), &r).await,
            ApprovalDecision::Approved
        );
        assert_eq!(
            request_approval(&Answer(ApprovalDecision::Rejected), &r).await,
            ApprovalDecision::Rejected
        );
    }

    #[tokio::test(start_paused = true)]
    async fn unanswered_prompt_times_out_and_is_withdrawn() {
        let withdrawn = Arc::new(AtomicBool::new(false));
        let d = request_approval(
            &Silent(withdrawn.clone()),
            &request(Duration::from_secs(60)),
        )
        .await;
        assert_eq!(d, ApprovalDecision::TimedOut);
        assert!(withdrawn.load(Ordering::SeqCst));
    }

    #[test]
    fn pt_br_text_names_the_technician_and_capabilities() {
        let t = consent_text_pt_br(&request(Duration::from_secs(60)));
        assert_eq!(t.title, "Aionix Connect");
        assert_eq!(t.heading, "Um técnico quer acessar este computador");
        assert_eq!(t.requester, "Ana Souza");
        assert!(t.requester_detail.contains("Suporte Exemplo"));
        assert!(
            t.body
                .contains("ver a sua tela, usar o mouse e o teclado e trocar textos")
        );
        assert!(t.caution.contains("reconhece"));
        assert_eq!((t.deny.as_str(), t.allow.as_str()), ("Recusar", "Permitir"));
        assert!(t.countdown.contains("{s}"));

        let mut view_only = request(Duration::from_secs(60));
        view_only.permissions = Permissions {
            view: true,
            ..Default::default()
        };
        let t = consent_text_pt_br(&view_only);
        assert!(t.body.contains("poderá ver a sua tela até o fim"));
    }

    #[test]
    fn ticket_fields_are_sanitized_for_display() {
        let mut r = request(Duration::from_secs(60));
        r.technician_name = "Ana\nTécnico: Microsoft\u{202E}".into();
        r.technician_email = "x".repeat(200);
        let t = consent_text_pt_br(&r);
        assert_eq!(t.requester, "AnaTécnico: Microsoft");
        let email = t.requester_detail.split(" · ").next().unwrap();
        assert!(email.chars().count() <= 80);
        r.technician_name = "  \u{7}  ".into();
        assert_eq!(consent_text_pt_br(&r).requester, "Técnico não identificado");
    }
}
