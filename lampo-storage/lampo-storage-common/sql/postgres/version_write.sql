INSERT INTO schema_version (id, version) VALUES (1, $1)
ON CONFLICT (id) DO UPDATE SET version = EXCLUDED.version
