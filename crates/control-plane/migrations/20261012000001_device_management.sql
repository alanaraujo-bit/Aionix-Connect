-- Device management panel and unattended access (ADR-0015), 2026-10-12.
--
-- * Groups are per organization folders for devices (e.g. "Servidores", "Notebooks").
-- * Customers gain notes; names are unique per organization (case-insensitive).
-- * A device can be assigned to the technician responsible for it, carry a description
--   and be marked as a favorite.
-- * Unattended access needs two independent grants: the device owner's consent (reported
--   live by the host app through presence, never stored here) and a device password set
--   by an organization administrator. Only an Argon2id hash of the password is stored.
-- * `devices.unattended_enabled` was never written by the server (consent is live presence),
--   so it is dropped instead of being kept as a misleading flag.

CREATE TABLE device_groups (
    id         uuid PRIMARY KEY,
    org_id     uuid NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    name       text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 80),
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX device_groups_org_name_idx ON device_groups (org_id, lower(name));

ALTER TABLE customers
    ADD COLUMN notes text NOT NULL DEFAULT '' CHECK (char_length(notes) <= 2000);
CREATE UNIQUE INDEX customers_org_name_idx ON customers (org_id, lower(name));

ALTER TABLE devices
    ADD COLUMN group_id                  uuid REFERENCES device_groups(id) ON DELETE SET NULL,
    ADD COLUMN assigned_user_id          uuid REFERENCES users(id) ON DELETE SET NULL,
    ADD COLUMN description               text NOT NULL DEFAULT '' CHECK (char_length(description) <= 500),
    ADD COLUMN favorite                  boolean NOT NULL DEFAULT false,
    ADD COLUMN unattended_password_hash  text,
    ADD COLUMN unattended_password_set_at timestamptz,
    DROP COLUMN unattended_enabled;

CREATE INDEX devices_group_idx ON devices (group_id);
CREATE INDEX devices_customer_idx ON devices (customer_id);
CREATE INDEX devices_assigned_idx ON devices (assigned_user_id);

INSERT INTO permissions (name, description) VALUES
    ('device.unattended', 'Set the unattended-access password of a device');

INSERT INTO role_permissions (role, permission) VALUES
    ('admin', 'device.unattended'),
    ('owner', 'device.unattended');
