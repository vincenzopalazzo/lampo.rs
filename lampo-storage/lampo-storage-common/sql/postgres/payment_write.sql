INSERT INTO payments
    (id, payment_hash, direction, amount_msat, fee_msat, status, created_at, invoice)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
ON CONFLICT (id) DO UPDATE SET
    payment_hash = EXCLUDED.payment_hash,
    direction    = EXCLUDED.direction,
    amount_msat  = EXCLUDED.amount_msat,
    fee_msat     = EXCLUDED.fee_msat,
    status       = EXCLUDED.status,
    created_at   = EXCLUDED.created_at,
    invoice      = EXCLUDED.invoice
