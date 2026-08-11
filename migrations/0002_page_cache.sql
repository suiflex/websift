-- Bounded page cache shared by research fetches.
-- The extraction bound is part of the key because a different bound produces different Markdown.
CREATE TABLE page_cache (
    profile TEXT NOT NULL,
    url TEXT NOT NULL,
    max_chars INTEGER NOT NULL,
    final_url TEXT NOT NULL,
    title TEXT,
    markdown TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    truncated INTEGER NOT NULL DEFAULT 0,
    fetched_at INTEGER NOT NULL,
    PRIMARY KEY (profile, url, max_chars)
);

CREATE INDEX page_cache_profile_fetched ON page_cache(profile, fetched_at);
