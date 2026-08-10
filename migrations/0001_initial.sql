CREATE TABLE runtime_instances (
    id TEXT NOT NULL,
    profile TEXT NOT NULL,
    started_at TEXT NOT NULL,
    heartbeat_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    PRIMARY KEY (id, profile)
);

CREATE TABLE crawl_jobs (
    id TEXT NOT NULL,
    profile TEXT NOT NULL,
    request TEXT NOT NULL,
    idempotency_key TEXT,
    state TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    terminal_reason TEXT,
    PRIMARY KEY (id, profile),
    UNIQUE (profile, idempotency_key)
);

CREATE TABLE crawl_urls (
    id TEXT NOT NULL,
    profile TEXT NOT NULL,
    job_id TEXT NOT NULL,
    normalized_url TEXT NOT NULL,
    discovered_from TEXT,
    depth INTEGER NOT NULL DEFAULT 0,
    state TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    lease_owner TEXT,
    lease_expires_at TEXT,
    next_attempt_at TEXT,
    final_url TEXT,
    error TEXT,
    PRIMARY KEY (id, profile),
    UNIQUE (profile, job_id, normalized_url),
    FOREIGN KEY (job_id, profile) REFERENCES crawl_jobs(id, profile) ON DELETE CASCADE
);

CREATE TABLE documents (
    id TEXT NOT NULL,
    profile TEXT NOT NULL,
    job_id TEXT NOT NULL,
    canonical_url TEXT NOT NULL,
    metadata TEXT,
    content_hash TEXT,
    extraction_version TEXT,
    PRIMARY KEY (id, profile),
    UNIQUE (profile, job_id, canonical_url),
    FOREIGN KEY (job_id, profile) REFERENCES crawl_jobs(id, profile) ON DELETE CASCADE
);

CREATE TABLE artifacts (
    id TEXT NOT NULL,
    profile TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    media_type TEXT NOT NULL,
    size INTEGER NOT NULL,
    hash TEXT,
    retention_deadline TEXT,
    PRIMARY KEY (id, profile),
    UNIQUE (profile, relative_path)
);

CREATE INDEX crawl_jobs_profile_updated ON crawl_jobs(profile, updated_at, id);
CREATE INDEX crawl_urls_profile_job_state ON crawl_urls(profile, job_id, state, id);
CREATE INDEX documents_profile_job ON documents(profile, job_id, id);
