//! The app's API client against the real Control Plane (PostgreSQL + Redis) for the
//! administration panel: the same calls the desktop app makes, end to end.

use aionix_client::{ApiClient, ClientError, authority::UserSession};
use aionix_control_api::{
    ErrorCode,
    rest::{CreateAccountRequest, OrgRole},
};
use aionix_control_plane::testing::{TEST_PASSWORD, TestApi, org_member};

#[tokio::test]
async fn admin_panel_calls_round_trip() {
    let Some(api) = TestApi::start().await else {
        return;
    };
    let client = ApiClient::new(&api.base).unwrap();
    let (org, _, owner_email) = org_member(&api.state.db, OrgRole::Owner).await;
    let owner = UserSession::login(client.clone(), &owner_email, TEST_PASSWORD)
        .await
        .unwrap();
    let access = owner.access_token().await.unwrap();
    let profile = owner.profile().await;
    assert!(!profile.must_change_password);
    assert!(profile.memberships[0].role.can_manage());

    // Create a friend's account; the temporary password signs in, flagged.
    let created = client
        .create_account(
            &access,
            org,
            &CreateAccountRequest {
                email: "amigo@example.test".into(),
                display_name: "Amigo Teste".into(),
                role: OrgRole::Technician,
            },
        )
        .await
        .unwrap();
    assert!(created.member.must_change_password);
    let friend = UserSession::login(
        client.clone(),
        "amigo@example.test",
        &created.temporary_password,
    )
    .await
    .unwrap();
    assert!(friend.profile().await.must_change_password);
    let friend_access = friend.access_token().await.unwrap();
    client
        .change_password(
            &friend_access,
            &created.temporary_password,
            "uma senha só minha",
        )
        .await
        .unwrap();

    // Duplicate e-mail is a conflict the app explains.
    let dup = client
        .create_account(
            &access,
            org,
            &CreateAccountRequest {
                email: "amigo@example.test".into(),
                display_name: "Outro".into(),
                role: OrgRole::Viewer,
            },
        )
        .await;
    assert!(matches!(
        dup,
        Err(ClientError::Api {
            code: ErrorCode::Conflict,
            ..
        })
    ));

    // Members, role change, reset, sign-out, disable, enable.
    let members = client.org_members(&access, org).await.unwrap().members;
    assert_eq!(members.len(), 2);
    let m = members
        .iter()
        .find(|m| m.email == "amigo@example.test")
        .unwrap();
    assert!(!m.must_change_password);
    assert!(m.last_login_at.is_some());
    let user = m.user_id;
    let promoted = client
        .set_member_role(&access, org, "amigo@example.test", OrgRole::Admin)
        .await
        .unwrap();
    assert_eq!(promoted.role, OrgRole::Admin);
    let reset = client
        .reset_member_password(&access, org, user)
        .await
        .unwrap();
    assert_eq!(reset.temporary_password.len(), 19);
    let out = client.sign_out_member(&access, org, user).await.unwrap();
    assert_eq!(out.ended_sessions, 0);
    client.disable_member(&access, org, user).await.unwrap();
    assert!(
        client
            .org_members(&access, org)
            .await
            .unwrap()
            .members
            .iter()
            .any(|m| m.user_id == user && m.disabled)
    );
    let enabled = client.enable_member(&access, org, user).await.unwrap();
    assert!(!enabled.disabled && enabled.must_change_password);

    // Sessions and history decode.
    assert!(
        client
            .org_sessions(&access, org)
            .await
            .unwrap()
            .sessions
            .is_empty()
    );
    let audit = client.org_audit(&access, org, 100).await.unwrap().events;
    assert!(
        audit
            .iter()
            .any(|e| e.action == "user.created" && e.target_name.as_deref() == Some("Amigo Teste"))
    );
    assert!(audit.iter().any(|e| e.action == "auth.password_changed"));

    // An account that does not manage: refused, and the app can tell.
    let viewer = UserSession::login(
        client.clone(),
        "amigo@example.test",
        &reset.temporary_password,
    )
    .await
    .unwrap();
    let viewer_access = viewer.access_token().await.unwrap();
    client
        .change_password(
            &viewer_access,
            &reset.temporary_password,
            "outra senha bem longa",
        )
        .await
        .unwrap();
    client
        .set_member_role(&access, org, "amigo@example.test", OrgRole::Technician)
        .await
        .unwrap();
    let denied = client.org_sessions(&viewer_access, org).await;
    assert!(matches!(
        denied,
        Err(ClientError::Api {
            code: ErrorCode::Forbidden,
            ..
        })
    ));
    api.stop().await;
}
