INSERT INTO kv (primary_namespace, secondary_namespace, key, value)
VALUES ($1, $2, $3, $4)
ON CONFLICT (primary_namespace, secondary_namespace, key)
DO UPDATE SET value = EXCLUDED.value
