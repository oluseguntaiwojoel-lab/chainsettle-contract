# TODO - Shipment on-chain audit log (bounded)

## Plan summary
Implement bounded `shipment.audit_log` with ring-buffer (max 20), append an `AuditEntry` on every state-changing function, and expose `get_audit_log(shipment_id)`.

## Steps
1. Inspect and update `chainsettle-contract/contracts/chainsetttle/src/lib.rs`:
   - Add `AuditEntry` (+ any supporting `AuditAction` if needed) types.
   - Extend `Shipment` to include `audit_log: Vec<AuditEntry>`.
   - Add helper `append_audit_entry` implementing cap=20 oldest eviction.
2. Update every state-changing function in `lib.rs` to call `append_audit_entry` before persisting shipment:
   - `create_shipment`, `top_up_escrow`, `submit_proof`, `confirm_milestone`, `release_held_payment`,
     `batch_confirm_milestones`, `raise_dispute`, `resolve_dispute`, `cancel_shipment`, `supplier_cancel`,
     `propose_amendment`, `transfer_buyer`, `transfer_supplier`, `propose_arbiter_rotation`,
     `claim_auto_confirmation`.
3. Add read-only query `get_audit_log(env, shipment_id) -> Vec<AuditEntry>`.
4. Add/extend tests in `chainsettle-contract/contracts/chainsetttle/src/test.rs`:
   - Verify log grows on state changes.
   - Verify cap eviction drops oldest and returns chronological order.
   - Verify multiple function types append entries.
5. Run `cargo test` for the Soroban contract crate and fix any compile/test failures.

