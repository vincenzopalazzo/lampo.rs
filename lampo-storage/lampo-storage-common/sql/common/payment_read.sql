SELECT id, payment_hash, direction, amount_msat, fee_msat, status, created_at, invoice
FROM payments WHERE id = $1
