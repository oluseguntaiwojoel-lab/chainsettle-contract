# TODO - Shipment on-chain audit log (bounded)

## Plan (approved)
1. Update lib.rs data model:
   - Keep/ensure `Shipment.audit_log: Vec<AuditEntry>` exists.
   - Implement `append_audit_entry` helper with cap=20 (evict oldest).
2. Wire audit logging into all state-changing entrypoints.
3. Add read-only query `get_audit_log(env, shipment_id)`.
4. Run `cargo test` (tests may fail; we’ll fix compile issues first).

## Progress
- [ ] Step 1: Implement bounded append helper
- [ ] Step 2: Append audit entries in all state-changing functions
- [ ] Step 3: Add get_audit_log query
- [ ] Step 4: Run cargo test and fix compile/test issues

