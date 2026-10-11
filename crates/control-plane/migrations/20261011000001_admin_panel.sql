-- Administration panel (2026-10-11).
--
-- * Accounts created or reset by an administrator get a temporary password that must be
--   replaced at the first sign-in; until then the account cannot request sessions or
--   register a technician workstation.
-- * Organization-wide session listing and termination are separate permissions, held by
--   admins and owners only.

ALTER TABLE users ADD COLUMN must_change_password boolean NOT NULL DEFAULT false;

INSERT INTO permissions (name, description) VALUES
    ('session.view_all',  'List every session of the organization'),
    ('session.terminate', 'End any session of the organization');

INSERT INTO role_permissions (role, permission) VALUES
    ('admin', 'session.view_all'),
    ('admin', 'session.terminate'),
    ('owner', 'session.view_all'),
    ('owner', 'session.terminate');
