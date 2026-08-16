DELETE FROM kv
WHERE primary_namespace = $1 AND secondary_namespace = $2 AND key = $3
