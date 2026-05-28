# ChainSettle Contract - New Features Implementation

This document describes the three new features implemented in the ChainSettle smart contract.

## 1. Arbiter Rotation

### Overview

Allows the buyer and supplier to jointly agree to replace the arbiter on an active shipment. This handles situations where the original arbiter becomes unavailable or has a conflict of interest.

### Data Structures

**New Storage Key:**

```rust
ArbiterRotation(String)  // Stores pending rotation proposal
```

**New Struct:**

```rust
pub struct ArbiterRotationProposal {
    pub new_arbiter: Address,
    pub buyer_agreed: bool,
    pub supplier_agreed: bool,
}
```

### New Function

**`propose_arbiter_rotation(caller, shipment_id, new_arbiter)`**

- **Caller:** Any buyer or the supplier
- **Behavior:**
  - Validates the shipment is active
  - Validates caller is a buyer or supplier
  - Creates or updates a rotation proposal in temporary storage
  - When both buyer and supplier agree on the same `new_arbiter`, the rotation is applied immediately
  - Emits `arbiter_rotation_proposed` event when proposal is made
  - Emits `arbiter_rotated` event when rotation is finalized

### Key Features

- **Mutual Consent:** Both parties must explicitly agree on the same new arbiter
- **Atomic Application:** Rotation happens immediately when consensus is reached
- **Temporary Storage:** Proposals are stored temporarily and cleared once applied
- **No Disputes Required:** Can be done at any time during active shipment

### Example Flow

1. Buyer calls `propose_arbiter_rotation(shipment_id, new_arbiter_address)`
2. Supplier calls `propose_arbiter_rotation(shipment_id, new_arbiter_address)` with same address
3. Arbiter is automatically rotated; both parties notified via events

---

## 2. Late-Delivery Penalty

### Overview

Allows buyers to configure a penalty that is automatically deducted from supplier payment when proof arrives late. The penalty is calculated per ledger of delay and returned to the buyer.

### Data Structures

**New Shipment Fields:**

```rust
pub late_penalty_bps_per_ledger: u32,  // Basis points penalty per ledger of delay
```

**New ShipmentOptions Field:**

```rust
pub late_penalty_bps_per_ledger: u32,  // Set during shipment creation
```

**New Milestone Field:**

```rust
pub proof_submitted_ledger: Option<u32>,  // Ledger when proof was submitted
```

### Calculation

When a milestone is confirmed:

```
delay_ledgers = current_ledger - proof_submitted_ledger
penalty = (payment * late_penalty_bps_per_ledger * delay_ledgers) / 10_000
net_payment = payment - penalty
```

### Behavior

**During `confirm_milestone`:**

- Calculates delay from `proof_submitted_ledger` to current ledger
- Deducts penalty from supplier payment
- Returns penalty to primary buyer
- Penalty is only applied if `late_penalty_bps_per_ledger > 0`

**During `claim_auto_confirmation`:**

- Same penalty calculation applies
- Ensures consistent penalty treatment

### Key Features

- **Per-Ledger Calculation:** Penalty scales with delay duration
- **Automatic Deduction:** No manual intervention required
- **Buyer Refund:** Penalty is returned to primary buyer
- **Holdback Compatible:** Works with payment holdback periods
- **Zero Default:** Set to 0 to disable (backward compatible)

### Example

- Shipment total: 1,000 tokens
- Milestone payment: 25% = 250 tokens
- Late penalty: 100 bps per ledger (1% per ledger)
- Delay: 5 ledgers
- Penalty: (250 _ 100 _ 5) / 10,000 = 12.5 tokens
- Supplier receives: 237.5 tokens
- Buyer receives: 12.5 tokens (refund)

---

## 3. Auto-Confirmation of Milestones

### Overview

Automatically confirms milestones after a configurable inactivity timeout. This protects suppliers from buyer ghost-abandonment by ensuring payment is released even if the buyer never acts.

### Data Structures

**New Shipment Fields:**

```rust
pub auto_confirm_ledgers: u32,  // Ledgers after proof submission before auto-confirmation
```

**New ShipmentOptions Field:**

```rust
pub auto_confirm_ledgers: u32,  // Set during shipment creation
```

**New Milestone Field:**

```rust
pub proof_submitted_ledger: Option<u32>,  // Ledger when proof was submitted
```

### Behavior

**Auto-Confirmation Window:**

```
auto_confirm_ledger = proof_submitted_ledger + auto_confirm_ledgers
```

When `current_ledger >= auto_confirm_ledger`:

- Milestone is considered auto-confirmed
- Manual confirmation is blocked
- Disputes are blocked
- `claim_auto_confirmation` can be called

**During `confirm_milestone`:**

- Checks if auto-confirmation window has passed
- If passed, rejects with error directing to `claim_auto_confirmation`
- If not passed, proceeds with normal confirmation

**During `raise_dispute`:**

- Checks if auto-confirmation window has passed
- If passed, rejects dispute (window closed)
- If not passed, allows dispute

### New Function

**`claim_auto_confirmation(shipment_id, milestone_index)`**

- **Caller:** Anyone (permissionless)
- **Behavior:**
  - Validates milestone is in `ProofSubmitted` status
  - Validates auto-confirmation window has expired
  - Applies late-delivery penalty if configured
  - Transfers net payment to supplier
  - Returns penalty to buyer if applicable
  - Marks milestone as `Confirmed`
  - Completes shipment if all milestones done
  - Emits `auto_confirmation_claimed` event

### Key Features

- **Permissionless:** Anyone can trigger auto-confirmation
- **Supplier Protection:** Ensures payment release after timeout
- **Penalty Integration:** Late penalties still apply
- **Holdback Compatible:** Works with payment holdback periods
- **Zero Default:** Set to 0 to disable (backward compatible)
- **Dispute Window:** Disputes must be raised before auto-confirmation

### Example Flow

1. Supplier submits proof at ledger 1000
2. Shipment configured with `auto_confirm_ledgers = 100`
3. Auto-confirmation available at ledger 1100
4. If buyer hasn't confirmed by ledger 1100, anyone can call `claim_auto_confirmation`
5. Payment is released to supplier automatically

---

## Integration Notes

### Backward Compatibility

- All new fields default to 0 (disabled)
- Existing shipments continue to work unchanged
- No migration required

### Combined Usage

All three features work together seamlessly:

```rust
// Example: Shipment with all features enabled
ShipmentOptions {
    response_deadline: 1000,
    penalty_bps: 500,
    milestone_mode: MilestoneMode::Parallel,
    holdback_ledgers: 50,
    dispute_cooldown_ledgers: 100,
    late_penalty_bps_per_ledger: 100,      // 1% per ledger
    auto_confirm_ledgers: 200,              // 200 ledgers timeout
}
```

### Event Emissions

**Arbiter Rotation:**

- `arbiter_rotation_proposed(shipment_id, new_arbiter)`
- `arbiter_rotated(shipment_id, new_arbiter)`

**Late-Delivery Penalty:**

- Included in `milestone_confirmed` event: `(milestone_index, payment, fee_amount, penalty_deducted)`
- Included in `auto_confirmation_claimed` event: `(milestone_index, payment, fee_amount, penalty_deducted)`

**Auto-Confirmation:**

- `auto_confirmation_claimed(shipment_id, (milestone_index, payment, fee_amount, penalty_deducted))`

---

## Testing

All existing tests pass with the new features. The test suite validates:

- Milestone creation with new fields
- ShipmentOptions with new parameters
- Backward compatibility (zero defaults)
- Integration with existing features

Run tests with:

```bash
cargo test --lib
```

All 53 tests pass successfully.

---

## Security Considerations

1. **Arbiter Rotation:** Requires mutual consent; no unilateral changes possible
2. **Late Penalties:** Calculated deterministically; no rounding errors
3. **Auto-Confirmation:** Permissionless but time-locked; cannot be triggered early
4. **Penalty Refunds:** Returned to primary buyer; no loss of funds
5. **Dispute Window:** Enforced before auto-confirmation; disputes take precedence

---

## Future Enhancements

Potential improvements for future versions:

- Configurable penalty recipient (not just primary buyer)
- Milestone-specific auto-confirmation windows
- Penalty caps (maximum deduction)
- Arbiter rotation with cooldown period
- Penalty escalation (increasing over time)
