CREATE TABLE IF NOT EXISTS mail_cleanup_plans (
  id TEXT PRIMARY KEY,
  provider TEXT NOT NULL CHECK(provider IN ('google', 'microsoft')),
  plan TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending',
  created_at INTEGER NOT NULL,
  executed_at INTEGER
);

