PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;
PRAGMA synchronous = FULL;

CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS principals (
    id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    is_operator INTEGER NOT NULL DEFAULT 0,
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS auth_tokens (
    token_hash TEXT PRIMARY KEY,
    principal_id TEXT NOT NULL REFERENCES principals(id) ON DELETE CASCADE,
    label TEXT NOT NULL,
    expires_at_ms INTEGER,
    revoked_at_ms INTEGER,
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    slug TEXT NOT NULL UNIQUE,
    default_channel_id TEXT NOT NULL UNIQUE,
    archived_at_ms INTEGER,
    version INTEGER NOT NULL DEFAULT 1,
    created_by TEXT NOT NULL REFERENCES principals(id),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS project_memberships (
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    principal_id TEXT NOT NULL REFERENCES principals(id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK(role IN ('viewer','artist','developer','approver','release_manager','admin')),
    version INTEGER NOT NULL DEFAULT 1,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY(project_id, principal_id)
);

CREATE TABLE IF NOT EXISTS art_channels (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    head_revision_id TEXT,
    version INTEGER NOT NULL DEFAULT 1,
    UNIQUE(project_id, name)
);

CREATE TABLE IF NOT EXISTS art_proposals (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    channel_id TEXT NOT NULL REFERENCES art_channels(id) ON DELETE CASCADE,
    base_revision_id TEXT,
    state_hash TEXT NOT NULL,
    title TEXT NOT NULL,
    description TEXT NOT NULL,
    resources_json TEXT NOT NULL,
    object_hashes_json TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('open','accepted','rejected')),
    created_by TEXT NOT NULL REFERENCES principals(id),
    accepted_revision_id TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS art_reviews (
    id TEXT PRIMARY KEY,
    proposal_id TEXT NOT NULL REFERENCES art_proposals(id) ON DELETE CASCADE,
    principal_id TEXT NOT NULL REFERENCES principals(id),
    verdict TEXT NOT NULL CHECK(verdict IN ('approve','request_changes','comment')),
    body TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS art_revisions (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    channel_id TEXT NOT NULL REFERENCES art_channels(id) ON DELETE CASCADE,
    parent_revision_id TEXT,
    proposal_id TEXT NOT NULL UNIQUE REFERENCES art_proposals(id),
    state_hash TEXT NOT NULL,
    state_json TEXT NOT NULL,
    created_by TEXT NOT NULL REFERENCES principals(id),
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS leases (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    channel_id TEXT NOT NULL REFERENCES art_channels(id) ON DELETE CASCADE,
    resource_id TEXT NOT NULL,
    base_revision_id TEXT,
    workstream TEXT NOT NULL,
    intended_action TEXT NOT NULL,
    cell_hash TEXT,
    holder_principal_id TEXT NOT NULL REFERENCES principals(id),
    holder_session_id TEXT NOT NULL,
    renewal_counter INTEGER NOT NULL DEFAULT 0,
    acquired_at_ms INTEGER NOT NULL,
    expires_at_ms INTEGER NOT NULL,
    released_at_ms INTEGER,
    release_reason TEXT,
    outcome_reference TEXT
);
CREATE INDEX IF NOT EXISTS idx_leases_active_resource
    ON leases(project_id, channel_id, resource_id, expires_at_ms, released_at_ms);

CREATE TABLE IF NOT EXISTS objects (
    content_hash TEXT PRIMARY KEY,
    sha256 TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    media_type TEXT NOT NULL,
    storage_path TEXT NOT NULL,
    integrity_status TEXT NOT NULL CHECK(integrity_status IN ('verified','corrupt','quarantined')),
    created_at_ms INTEGER NOT NULL,
    verified_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS project_objects (
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    content_hash TEXT NOT NULL REFERENCES objects(content_hash) ON DELETE RESTRICT,
    purpose TEXT NOT NULL,
    referenced_by TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY(project_id, content_hash)
);

CREATE TABLE IF NOT EXISTS uploads (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    expected_hash TEXT NOT NULL,
    expected_size INTEGER NOT NULL,
    media_type TEXT NOT NULL,
    token_hash TEXT NOT NULL,
    temp_path TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('negotiated','received','completed','expired','failed')),
    actual_sha256 TEXT,
    expires_at_ms INTEGER NOT NULL,
    created_by TEXT NOT NULL REFERENCES principals(id),
    created_at_ms INTEGER NOT NULL,
    UNIQUE(project_id, expected_hash, status)
);

CREATE TABLE IF NOT EXISTS download_transfers (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    content_hash TEXT NOT NULL REFERENCES objects(content_hash) ON DELETE CASCADE,
    token_hash TEXT NOT NULL,
    expires_at_ms INTEGER NOT NULL,
    created_by TEXT NOT NULL REFERENCES principals(id),
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS builds (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    logical_hash TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('queued','running','succeeded','failed','cancelled')),
    evidence_json TEXT NOT NULL,
    created_by TEXT NOT NULL REFERENCES principals(id),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS build_attempts (
    id TEXT PRIMARY KEY,
    build_id TEXT NOT NULL REFERENCES builds(id) ON DELETE CASCADE,
    status TEXT NOT NULL,
    evidence_json TEXT NOT NULL,
    created_by TEXT NOT NULL REFERENCES principals(id),
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS releases (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    bundle_hash TEXT NOT NULL,
    environment TEXT NOT NULL,
    target_json TEXT NOT NULL,
    evidence_json TEXT NOT NULL,
    request_hash TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('draft','approved','running','succeeded','failed','rollback_required','rolled_back')),
    created_by TEXT NOT NULL REFERENCES principals(id),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS release_approvals (
    id TEXT PRIMARY KEY,
    release_id TEXT NOT NULL REFERENCES releases(id) ON DELETE CASCADE,
    principal_id TEXT NOT NULL REFERENCES principals(id),
    request_hash TEXT NOT NULL,
    evidence_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    UNIQUE(release_id, principal_id, request_hash)
);

CREATE TABLE IF NOT EXISTS idempotency_records (
    principal_id TEXT NOT NULL REFERENCES principals(id),
    project_id TEXT NOT NULL,
    route TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_hash TEXT NOT NULL,
    response_status INTEGER NOT NULL,
    response_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    expires_at_ms INTEGER NOT NULL,
    PRIMARY KEY(principal_id, project_id, route, idempotency_key)
);

CREATE TABLE IF NOT EXISTS audit_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    project_id TEXT NOT NULL,
    actor_principal_id TEXT NOT NULL,
    action TEXT NOT NULL,
    aggregate_type TEXT NOT NULL,
    aggregate_id TEXT NOT NULL,
    outcome TEXT NOT NULL,
    details_json TEXT NOT NULL,
    occurred_at_ms INTEGER NOT NULL,
    previous_hash TEXT,
    event_hash TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_audit_project_sequence ON audit_events(project_id, sequence);

CREATE TABLE IF NOT EXISTS outbox_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    project_id TEXT NOT NULL,
    category TEXT NOT NULL,
    event_type TEXT NOT NULL,
    aggregate_id TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    occurred_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_outbox_project_sequence ON outbox_events(project_id, sequence);

INSERT OR IGNORE INTO schema_migrations(version, applied_at_ms)
VALUES (1, CAST(strftime('%s','now') AS INTEGER) * 1000);
