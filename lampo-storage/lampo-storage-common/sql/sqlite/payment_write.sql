INSERT OR REPLACE INTO payments
    (id, payment_hash, direction, amount_msat, fee_msat, status, created_at, invoice)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
