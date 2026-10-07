-- Aionix Connect Control Plane — initial schema.
-- PostgreSQL is the source of truth for accounts, organizations, devices, sessions and
-- audit. Ephemeral state (presence, offer delivery, rate-limit counters) lives in Redis.

-- ---------------------------------------------------------------------------
-- Accounts
-- ---------------------------------------------------------------------------

CREATE TABLE users (
    id                  uuid PRIMARY KEY,
    email               text NOT NULL,
    -- lower(trim(email)); the login identifier.
    email_normalized    text NOT NULL UNIQUE,
    display_name        text NOT NULL,
    -- Argon2id PHC string. NULL for accounts that can only use a future SSO/MFA method.
    password_hash       text,
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now(),
    disabled_at         timestamptz,
    failed_login_count  integer NOT NULL DEFAULT 0,
    locked_until        timestamptz,
    last_login_at       timestamptz,
    CONSTRAINT users_email_not_blank CHECK (length(email_normalized) > 2)
);

-- One row per login (refresh-token family). Access tokens carry its id (`sid`) and are
-- rejected as soon as the row is revoked or expired.
CREATE TABLE auth_sessions (
    id                    uuid PRIMARY KEY,
    user_id               uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- SHA-256 of the current refresh secret, and of the one it replaced (reuse detection).
    refresh_hash          text NOT NULL,
    previous_refresh_hash text,
    created_at            timestamptz NOT NULL DEFAULT now(),
    last_used_at          timestamptz NOT NULL DEFAULT now(),
    -- Sliding refresh expiry, bounded by the absolute expiry.
    expires_at            timestamptz NOT NULL,
    absolute_expires_at   timestamptz NOT NULL,
    revoked_at            timestamptz,
    revoked_reason        text,
    client_ip             text,
    user_agent            text
);
CREATE INDEX auth_sessions_user_idx ON auth_sessions (user_id) WHERE revoked_at IS NULL;

-- ---------------------------------------------------------------------------
-- Organizations, roles and permissions
-- ---------------------------------------------------------------------------

CREATE TABLE organizations (
    id          uuid PRIMARY KEY,
    name        text NOT NULL,
    slug        text NOT NULL UNIQUE,
    created_at  timestamptz NOT NULL DEFAULT now(),
    disabled_at timestamptz
);

CREATE TABLE roles (
    name        text PRIMARY KEY,
    description text NOT NULL,
    rank        integer NOT NULL UNIQUE
);

CREATE TABLE permissions (
    name        text PRIMARY KEY,
    description text NOT NULL
);

CREATE TABLE role_permissions (
    role       text NOT NULL REFERENCES roles(name) ON DELETE CASCADE,
    permission text NOT NULL REFERENCES permissions(name) ON DELETE CASCADE,
    PRIMARY KEY (role, permission)
);

CREATE TABLE memberships (
    org_id     uuid NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    user_id    uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role       text NOT NULL REFERENCES roles(name),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (org_id, user_id)
);
CREATE INDEX memberships_user_idx ON memberships (user_id);

INSERT INTO roles (name, description, rank) VALUES
    ('viewer',     'Read-only access to devices and history', 10),
    ('technician', 'Can request remote support sessions', 20),
    ('admin',      'Manages devices, enrollment and members', 30),
    ('owner',      'Full control of the organization', 40);

INSERT INTO permissions (name, description) VALUES
    ('device.view',        'List and look up the organization''s devices'),
    ('device.manage',      'Rename and organize devices'),
    ('device.revoke',      'Revoke a device identity'),
    ('enrollment.create',  'Create device enrollment tokens'),
    ('session.request',    'Request a remote support session to a device'),
    ('session.control',    'Send keyboard/mouse input in a session'),
    ('session.clipboard',  'Synchronize clipboard text in a session'),
    ('member.view',        'List organization members'),
    ('member.manage',      'Add members and change roles'),
    ('audit.view',         'Read audit and security events');

INSERT INTO role_permissions (role, permission) VALUES
    ('viewer', 'device.view'),
    ('viewer', 'member.view'),
    ('technician', 'device.view'),
    ('technician', 'member.view'),
    ('technician', 'session.request'),
    ('technician', 'session.control'),
    ('technician', 'session.clipboard'),
    ('admin', 'device.view'),
    ('admin', 'device.manage'),
    ('admin', 'device.revoke'),
    ('admin', 'enrollment.create'),
    ('admin', 'session.request'),
    ('admin', 'session.control'),
    ('admin', 'session.clipboard'),
    ('admin', 'member.view'),
    ('admin', 'member.manage'),
    ('admin', 'audit.view'),
    ('owner', 'device.view'),
    ('owner', 'device.manage'),
    ('owner', 'device.revoke'),
    ('owner', 'enrollment.create'),
    ('owner', 'session.request'),
    ('owner', 'session.control'),
    ('owner', 'session.clipboard'),
    ('owner', 'member.view'),
    ('owner', 'member.manage'),
    ('owner', 'audit.view');

CREATE TABLE customers (
    id         uuid PRIMARY KEY,
    org_id     uuid NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    name       text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX customers_org_idx ON customers (org_id);

-- ---------------------------------------------------------------------------
-- Devices
-- ---------------------------------------------------------------------------

-- Only the SHA-256 of an enrollment token is stored; the token is shown once.
CREATE TABLE enrollment_tokens (
    id          uuid PRIMARY KEY,
    org_id      uuid NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    customer_id uuid REFERENCES customers(id) ON DELETE SET NULL,
    token_hash  text NOT NULL UNIQUE,
    created_by  uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz NOT NULL,
    max_uses    integer NOT NULL CHECK (max_uses BETWEEN 1 AND 1000),
    uses        integer NOT NULL DEFAULT 0 CHECK (uses >= 0),
    revoked_at  timestamptz
);

-- The device's Ed25519 public key is its identity. Private keys never reach the server.
-- friendly_id is a human lookup handle only, never a credential.
CREATE TABLE devices (
    id                  uuid PRIMARY KEY,
    friendly_id         integer NOT NULL UNIQUE CHECK (friendly_id BETWEEN 100000000 AND 999999999),
    public_key          text NOT NULL UNIQUE,
    kind                text NOT NULL CHECK (kind IN ('agent', 'quick_support', 'technician')),
    org_id              uuid REFERENCES organizations(id) ON DELETE SET NULL,
    customer_id         uuid REFERENCES customers(id) ON DELETE SET NULL,
    -- Technician workstations belong to the user who registered them.
    owner_user_id       uuid REFERENCES users(id) ON DELETE SET NULL,
    enrollment_token_id uuid REFERENCES enrollment_tokens(id) ON DELETE SET NULL,
    display_name        text NOT NULL DEFAULT '',
    hostname            text NOT NULL DEFAULT '',
    os_name             text NOT NULL DEFAULT '',
    os_version          text NOT NULL DEFAULT '',
    app_version         text NOT NULL DEFAULT '',
    unattended_enabled  boolean NOT NULL DEFAULT false,
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now(),
    last_login_at       timestamptz,
    last_seen_at        timestamptz,
    revoked_at          timestamptz,
    revoked_by          uuid REFERENCES users(id) ON DELETE SET NULL,
    revoked_reason      text,
    CONSTRAINT devices_agent_has_org CHECK (kind <> 'agent' OR org_id IS NOT NULL),
    CONSTRAINT devices_technician_has_owner CHECK (kind <> 'technician' OR owner_user_id IS NOT NULL)
);
CREATE INDEX devices_org_idx ON devices (org_id);
CREATE INDEX devices_owner_idx ON devices (owner_user_id);

-- Single-use proof-of-possession challenges (registration and device login).
CREATE TABLE device_challenges (
    nonce_hash text PRIMARY KEY,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);
CREATE INDEX device_challenges_expiry_idx ON device_challenges (expires_at);

-- ---------------------------------------------------------------------------
-- Sessions
-- ---------------------------------------------------------------------------

CREATE TABLE sessions (
    id                   uuid PRIMARY KEY,
    host_device_id       uuid NOT NULL REFERENCES devices(id),
    technician_user_id   uuid NOT NULL REFERENCES users(id),
    technician_device_id uuid NOT NULL REFERENCES devices(id),
    -- Organization under whose authority the ticket was issued.
    org_id               uuid REFERENCES organizations(id) ON DELETE SET NULL,
    mode                 text NOT NULL CHECK (mode IN ('assisted', 'unattended')),
    permissions          text[] NOT NULL,
    status               text NOT NULL CHECK (status IN (
                             'requested', 'offered', 'accepted', 'rejected', 'approved',
                             'denied', 'connected', 'ended', 'failed', 'expired')),
    -- SHA-256 of the ticket's token id; the ticket itself is never stored.
    ticket_jti_hash      text NOT NULL UNIQUE,
    ticket_expires_at    timestamptz NOT NULL,
    requested_at         timestamptz NOT NULL DEFAULT now(),
    answered_at          timestamptz,
    approved_at          timestamptz,
    connected_at         timestamptz,
    ended_at             timestamptz,
    end_reason           text,
    transport            text
);
CREATE INDEX sessions_host_idx ON sessions (host_device_id, requested_at DESC);
CREATE INDEX sessions_technician_idx ON sessions (technician_user_id, requested_at DESC);
CREATE INDEX sessions_active_idx ON sessions (status)
    WHERE status IN ('requested', 'offered', 'accepted', 'approved', 'connected');

-- Lifecycle reports from both peers and the server (metadata only, no content).
CREATE TABLE session_events (
    id          bigserial PRIMARY KEY,
    session_id  uuid NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    source      text NOT NULL CHECK (source IN ('host', 'technician', 'server')),
    kind        text NOT NULL,
    transport   text,
    detail      text NOT NULL DEFAULT '',
    occurred_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX session_events_session_idx ON session_events (session_id, id);

-- ---------------------------------------------------------------------------
-- Audit (append-only)
-- ---------------------------------------------------------------------------

CREATE TABLE audit_events (
    id               bigserial PRIMARY KEY,
    occurred_at      timestamptz NOT NULL DEFAULT now(),
    category         text NOT NULL CHECK (category IN ('audit', 'security')),
    action           text NOT NULL,
    outcome          text NOT NULL CHECK (outcome IN ('success', 'denied', 'failure')),
    actor_user_id    uuid,
    actor_device_id  uuid,
    org_id           uuid,
    target_user_id   uuid,
    target_device_id uuid,
    session_id       uuid,
    client_ip        text,
    request_id       text,
    -- Metadata only. Never secrets, credentials, clipboard or screen content.
    details          jsonb NOT NULL DEFAULT '{}'::jsonb
);
CREATE INDEX audit_events_org_idx ON audit_events (org_id, id DESC);
CREATE INDEX audit_events_action_idx ON audit_events (action, id DESC);

CREATE FUNCTION audit_events_append_only() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'audit_events is append-only (% refused)', TG_OP
        USING ERRCODE = 'insufficient_privilege';
END;
$$;

CREATE TRIGGER audit_events_no_update_delete
    BEFORE UPDATE OR DELETE ON audit_events
    FOR EACH ROW EXECUTE FUNCTION audit_events_append_only();

CREATE TRIGGER audit_events_no_truncate
    BEFORE TRUNCATE ON audit_events
    FOR EACH STATEMENT EXECUTE FUNCTION audit_events_append_only();
