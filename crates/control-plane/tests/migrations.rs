//! Database migrations against a real, empty PostgreSQL database.

use aionix_control_plane::{db, testing::TestDb};

#[tokio::test]
async fn migrations_apply_to_an_empty_database_and_are_idempotent() {
    let Some(tdb) = TestDb::create_unmigrated().await else {
        return;
    };
    assert!(
        db::check_ready(&tdb.pool).await.is_err(),
        "an empty database must not report ready"
    );
    db::migrate(&tdb.pool).await.unwrap();
    db::migrate(&tdb.pool).await.unwrap();
    db::check_ready(&tdb.pool).await.unwrap();

    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT table_name::text FROM information_schema.tables
         WHERE table_schema = 'public' ORDER BY table_name",
    )
    .fetch_all(&tdb.pool)
    .await
    .unwrap();
    for t in [
        "audit_events",
        "auth_sessions",
        "customers",
        "device_challenges",
        "devices",
        "enrollment_tokens",
        "memberships",
        "organizations",
        "permissions",
        "role_permissions",
        "roles",
        "session_events",
        "sessions",
        "users",
    ] {
        assert!(tables.iter().any(|x| x == t), "missing table {t}: {tables:?}");
    }
    let roles: Vec<String> = sqlx::query_scalar("SELECT name FROM roles ORDER BY rank")
        .fetch_all(&tdb.pool)
        .await
        .unwrap();
    assert_eq!(roles, ["viewer", "technician", "admin", "owner"]);
    // Viewers cannot request sessions; technicians can.
    let viewer_can: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM role_permissions WHERE role = 'viewer' AND permission = 'session.request')",
    )
    .fetch_one(&tdb.pool)
    .await
    .unwrap();
    assert!(!viewer_can);
    tdb.cleanup().await;
}

#[tokio::test]
async fn audit_events_are_append_only() {
    let Some(tdb) = TestDb::create().await else {
        return;
    };
    aionix_control_plane::audit::AuditRecord::new(
        "test.event",
        aionix_control_plane::audit::Outcome::Success,
    )
    .write(&tdb.pool)
    .await
    .unwrap();
    let update = sqlx::query("UPDATE audit_events SET action = 'tampered'")
        .execute(&tdb.pool)
        .await;
    assert!(update.is_err(), "UPDATE must be refused");
    let delete = sqlx::query("DELETE FROM audit_events").execute(&tdb.pool).await;
    assert!(delete.is_err(), "DELETE must be refused");
    let truncate = sqlx::query("TRUNCATE audit_events").execute(&tdb.pool).await;
    assert!(truncate.is_err(), "TRUNCATE must be refused");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action = 'test.event'")
        .fetch_one(&tdb.pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
    tdb.cleanup().await;
}

#[tokio::test]
async fn database_constraints_protect_identities() {
    let Some(tdb) = TestDb::create().await else {
        return;
    };
    // Friendly ids must have nine digits; agents need an organization.
    let bad_friendly = sqlx::query(
        "INSERT INTO devices (id, friendly_id, public_key, kind) VALUES (gen_random_uuid(), 12, 'k1', 'quick_support')",
    )
    .execute(&tdb.pool)
    .await;
    assert!(bad_friendly.is_err());
    let orphan_agent = sqlx::query(
        "INSERT INTO devices (id, friendly_id, public_key, kind) VALUES (gen_random_uuid(), 123456789, 'k2', 'agent')",
    )
    .execute(&tdb.pool)
    .await;
    assert!(orphan_agent.is_err());
    tdb.cleanup().await;
}
