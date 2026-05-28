#![no_std]

use soroban_sdk::{
    contract, contractimpl, contracttype, token, Address, BytesN, Env, String, Vec, Symbol,
};

// ============================================================
// DATA TYPES
// ============================================================

#[contracttype]
#[derive(Clone, PartialEq, Debug)]
pub enum MilestoneStatus {
    Pending,
    ProofSubmitted,
    Confirmed,
    Disputed,
    Resolved,
}

#[contracttype]
#[derive(Clone)]
pub struct Milestone {
    pub name: String,
    pub payment_percent: u32,
    pub proof_hash: String,
    pub status: MilestoneStatus,
}

#[contracttype]
#[derive(Clone, PartialEq, Debug)]
pub enum ShipmentStatus {
    Active,
    Completed,
    Cancelled,
}

#[contracttype]
#[derive(Clone)]
pub struct Shipment {
    pub id: String,
    pub buyer: Address,
    pub supplier: Address,
    pub logistics: Address,
    pub arbiter: Address,
    pub token: Address,
    pub total_amount: i128,
    pub released_amount: i128,
    pub milestones: Vec<Milestone>,
    pub status: ShipmentStatus,
    pub created_at: u32,
}

/// Cancellation policy stored separately (keeps Shipment within the 12-field contracttype limit).
#[contracttype]
#[derive(Clone)]
pub struct CancelPolicy {
    /// 0 = supplier cancellation disabled; >0 = ledgers after proof submission
    pub response_deadline: u32,
    /// basis points deducted from buyer refund on supplier cancellation (e.g. 500 = 5%)
    pub penalty_bps: u32,
}

/// Pending amendment proposal for a single milestone.
#[contracttype]
#[derive(Clone)]
pub struct AmendmentProposal {
    pub new_percent: u32,
    pub new_name: String,
    pub buyer_agreed: bool,
    pub supplier_agreed: bool,
}

// ============================================================
// STORAGE KEYS
// ============================================================

#[contracttype]
pub enum DataKey {
    Shipment(String),
    CancelPolicy(String),
    AllShipments,
    Admin,
    /// Ledger sequence when a milestone entered ProofSubmitted state.
    ProofSubmittedAt(String, u32),
    /// Pending amendment proposal.
    Amendment(String, u32),
}

// ============================================================
// ERRORS
// ============================================================

#[contracttype]
#[derive(Clone, Copy, PartialEq)]
#[repr(u32)]
pub enum ChainSettleError {
    ShipmentAlreadyExists = 1,
    ShipmentNotFound = 2,
    Unauthorized = 3,
    InvalidMilestoneIndex = 4,
    InvalidMilestoneStatus = 5,
    ShipmentNotActive = 6,
    InvalidPercentages = 7,
    InvalidAmount = 8,
    DisputeAlreadyOpen = 9,
}

// ============================================================
// CONTRACT
// ============================================================

#[contract]
pub struct ChainSettleContract;

#[contractimpl]
impl ChainSettleContract {

    // ----------------------------------------------------------
    // INIT
    // ----------------------------------------------------------

    pub fn init(env: Env, admin: Address) {
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
    }

    // ----------------------------------------------------------
    // UPGRADE  (#4)
    // ----------------------------------------------------------

    /// Replace the contract WASM in-place. Only callable by admin.
    pub fn upgrade(env: Env, admin: Address, new_wasm_hash: BytesN<32>) {
        admin.require_auth();
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic!("unauthorized"));
        if admin != stored_admin {
            panic!("unauthorized");
        }
        env.deployer().update_current_contract_wasm(new_wasm_hash.clone());
        env.events().publish(
            (Symbol::new(&env, "contract_upgraded"),),
            (new_wasm_hash, env.ledger().sequence()),
        );
    }

    /// Migration stub — call once after upgrade to perform any data-model changes.
    pub fn migrate(_env: Env) {
        // No-op for current version; implement data migrations here post-upgrade.
    }

    // ----------------------------------------------------------
    // CREATE SHIPMENT
    // ----------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn create_shipment(
        env: Env,
        shipment_id: String,
        buyer: Address,
        supplier: Address,
        logistics: Address,
        arbiter: Address,
        token: Address,
        total_amount: i128,
        milestones: Vec<Milestone>,
        response_deadline: u32,
        penalty_bps: u32,
    ) -> String {
        buyer.require_auth();

        if total_amount <= 0 {
            panic!("amount must be greater than zero");
        }

        let mut total_percent: u32 = 0;
        for i in 0..milestones.len() {
            total_percent += milestones.get(i).unwrap().payment_percent;
        }
        if total_percent != 100 {
            panic!("milestone percentages must sum to 100");
        }

        if env
            .storage()
            .persistent()
            .has(&DataKey::Shipment(shipment_id.clone()))
        {
            panic!("shipment already exists");
        }

        let token_client = token::Client::new(&env, &token);
        token_client.transfer(&buyer, &env.current_contract_address(), &total_amount);

        let shipment = Shipment {
            id: shipment_id.clone(),
            buyer,
            supplier,
            logistics,
            arbiter,
            token,
            total_amount,
            released_amount: 0,
            milestones,
            status: ShipmentStatus::Active,
            created_at: env.ledger().sequence(),
        };

        env.storage()
            .persistent()
            .set(&DataKey::Shipment(shipment_id.clone()), &shipment);
        env.storage()
            .persistent()
            .set(
                &DataKey::CancelPolicy(shipment_id.clone()),
                &CancelPolicy { response_deadline, penalty_bps },
            );
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::Shipment(shipment_id.clone()), 100_000, 6_300_000);

        env.events().publish(
            (Symbol::new(&env, "shipment_created"), shipment_id.clone()),
            shipment_id.clone(),
        );

        shipment_id
    }

    // ----------------------------------------------------------
    // SUBMIT PROOF
    // ----------------------------------------------------------

    pub fn submit_proof(
        env: Env,
        caller: Address,
        shipment_id: String,
        milestone_index: u32,
        proof_hash: String,
    ) {
        caller.require_auth();

        let mut shipment = Self::get_shipment_internal(&env, &shipment_id);

        if shipment.status != ShipmentStatus::Active {
            panic!("shipment is not active");
        }
        if milestone_index as usize >= shipment.milestones.len() as usize {
            panic!("invalid milestone index");
        }

        let mut milestone = shipment.milestones.get(milestone_index).unwrap();

        if milestone.status != MilestoneStatus::Pending {
            panic!("milestone is not in pending status");
        }
        if caller != shipment.supplier && caller != shipment.logistics {
            panic!("unauthorized");
        }

        milestone.proof_hash = proof_hash;
        milestone.status = MilestoneStatus::ProofSubmitted;
        shipment.milestones.set(milestone_index, milestone);

        env.storage()
            .persistent()
            .set(&DataKey::Shipment(shipment_id.clone()), &shipment);

        // Record the ledger at which proof was submitted (used by supplier_cancel).
        env.storage().persistent().set(
            &DataKey::ProofSubmittedAt(shipment_id.clone(), milestone_index),
            &env.ledger().sequence(),
        );

        env.events().publish(
            (Symbol::new(&env, "proof_submitted"), shipment_id.clone()),
            milestone_index,
        );
    }

    // ----------------------------------------------------------
    // CONFIRM MILESTONE
    // ----------------------------------------------------------

    pub fn confirm_milestone(
        env: Env,
        buyer: Address,
        shipment_id: String,
        milestone_index: u32,
    ) {
        buyer.require_auth();
        let mut shipment = Self::get_shipment_internal(&env, &shipment_id);

        if shipment.status != ShipmentStatus::Active {
            panic!("shipment is not active");
        }
        if buyer != shipment.buyer {
            panic!("unauthorized");
        }
        if milestone_index as usize >= shipment.milestones.len() as usize {
            panic!("invalid milestone index");
        }

        let mut milestone = shipment.milestones.get(milestone_index).unwrap();
        if milestone.status != MilestoneStatus::ProofSubmitted {
            panic!("milestone proof not yet submitted");
        }

        milestone.status = MilestoneStatus::Confirmed;
        shipment.milestones.set(milestone_index, milestone.clone());

        let payment = (shipment.total_amount * milestone.payment_percent as i128) / 100;
        shipment.released_amount += payment;

        let token_client = token::Client::new(&env, &shipment.token);
        token_client.transfer(&env.current_contract_address(), &shipment.supplier, &payment);

        let all_confirmed = (0..shipment.milestones.len()).all(|i| {
            shipment.milestones.get(i).unwrap().status == MilestoneStatus::Confirmed
        });
        if all_confirmed {
            shipment.status = ShipmentStatus::Completed;
        }

        env.storage()
            .persistent()
            .set(&DataKey::Shipment(shipment_id.clone()), &shipment);

        env.events().publish(
            (Symbol::new(&env, "milestone_confirmed"), shipment_id.clone()),
            (milestone_index, payment),
        );
    }

    // ----------------------------------------------------------
    // BATCH CONFIRM MILESTONES  (#8)
    // ----------------------------------------------------------

    /// Confirm multiple milestones in one invocation. Atomic — any failure reverts all.
    pub fn batch_confirm_milestones(
        env: Env,
        buyer: Address,
        shipment_id: String,
        milestone_indices: Vec<u32>,
    ) {
        buyer.require_auth();

        if milestone_indices.is_empty() {
            return;
        }

        let mut shipment = Self::get_shipment_internal(&env, &shipment_id);

        if shipment.status != ShipmentStatus::Active {
            panic!("shipment is not active");
        }
        if buyer != shipment.buyer {
            panic!("unauthorized");
        }

        // Validate all indices and statuses before mutating anything.
        for i in 0..milestone_indices.len() {
            let idx = milestone_indices.get(i).unwrap();
            if idx as usize >= shipment.milestones.len() as usize {
                panic!("invalid milestone index");
            }
            let m = shipment.milestones.get(idx).unwrap();
            if m.status != MilestoneStatus::ProofSubmitted {
                panic!("milestone proof not yet submitted");
            }
        }

        // Apply confirmations and emit events.
        for i in 0..milestone_indices.len() {
            let idx = milestone_indices.get(i).unwrap();
            let mut milestone = shipment.milestones.get(idx).unwrap();
            milestone.status = MilestoneStatus::Confirmed;
            shipment.milestones.set(idx, milestone.clone());

            let payment = (shipment.total_amount * milestone.payment_percent as i128) / 100;
            shipment.released_amount += payment;

            let token_client = token::Client::new(&env, &shipment.token);
            token_client.transfer(&env.current_contract_address(), &shipment.supplier, &payment);

            env.events().publish(
                (Symbol::new(&env, "milestone_confirmed"), shipment_id.clone()),
                (idx, payment),
            );
        }

        let all_confirmed = (0..shipment.milestones.len()).all(|i| {
            shipment.milestones.get(i).unwrap().status == MilestoneStatus::Confirmed
        });
        if all_confirmed {
            shipment.status = ShipmentStatus::Completed;
        }

        env.storage()
            .persistent()
            .set(&DataKey::Shipment(shipment_id.clone()), &shipment);
    }

    // ----------------------------------------------------------
    // RAISE DISPUTE
    // ----------------------------------------------------------

    pub fn raise_dispute(
        env: Env,
        buyer: Address,
        shipment_id: String,
        milestone_index: u32,
    ) {
        buyer.require_auth();

        let mut shipment = Self::get_shipment_internal(&env, &shipment_id);

        if shipment.status != ShipmentStatus::Active {
            panic!("shipment is not active");
        }
        if buyer != shipment.buyer {
            panic!("unauthorized");
        }

        let mut milestone = shipment.milestones.get(milestone_index).unwrap();
        if milestone.status != MilestoneStatus::ProofSubmitted {
            panic!("can only dispute a submitted proof");
        }

        milestone.status = MilestoneStatus::Disputed;
        shipment.milestones.set(milestone_index, milestone);

        env.storage()
            .persistent()
            .set(&DataKey::Shipment(shipment_id.clone()), &shipment);

        env.events().publish(
            (Symbol::new(&env, "dispute_raised"), shipment_id.clone()),
            milestone_index,
        );
    }

    // ----------------------------------------------------------
    // RESOLVE DISPUTE
    // ----------------------------------------------------------

    pub fn resolve_dispute(
        env: Env,
        arbiter: Address,
        shipment_id: String,
        milestone_index: u32,
        approve: bool,
    ) {
        arbiter.require_auth();

        let mut shipment = Self::get_shipment_internal(&env, &shipment_id);

        if shipment.status != ShipmentStatus::Active {
            panic!("shipment is not active");
        }
        if arbiter != shipment.arbiter {
            panic!("unauthorized");
        }

        let mut milestone = shipment.milestones.get(milestone_index).unwrap();
        if milestone.status != MilestoneStatus::Disputed {
            panic!("milestone is not in disputed status");
        }

        if approve {
            let payment = (shipment.total_amount * milestone.payment_percent as i128) / 100;
            shipment.released_amount += payment;
            let token_client = token::Client::new(&env, &shipment.token);
            token_client.transfer(&env.current_contract_address(), &shipment.supplier, &payment);
            milestone.status = MilestoneStatus::Resolved;
        } else {
            milestone.status = MilestoneStatus::Pending;
            milestone.proof_hash = String::from_str(&env, "");
        }

        shipment.milestones.set(milestone_index, milestone);

        let all_done = (0..shipment.milestones.len()).all(|i| {
            let s = shipment.milestones.get(i).unwrap().status;
            s == MilestoneStatus::Confirmed || s == MilestoneStatus::Resolved
        });
        if all_done {
            shipment.status = ShipmentStatus::Completed;
        }

        env.storage()
            .persistent()
            .set(&DataKey::Shipment(shipment_id.clone()), &shipment);

        env.events().publish(
            (Symbol::new(&env, "dispute_resolved"), shipment_id.clone()),
            (milestone_index, approve),
        );
    }

    // ----------------------------------------------------------
    // CANCEL SHIPMENT (buyer)
    // ----------------------------------------------------------

    pub fn cancel_shipment(env: Env, buyer: Address, shipment_id: String) {
        buyer.require_auth();

        let mut shipment = Self::get_shipment_internal(&env, &shipment_id);

        if shipment.status != ShipmentStatus::Active {
            panic!("shipment is not active");
        }
        if buyer != shipment.buyer {
            panic!("unauthorized");
        }

        for i in 0..shipment.milestones.len() {
            let m = shipment.milestones.get(i).unwrap();
            if m.status == MilestoneStatus::Confirmed || m.status == MilestoneStatus::Resolved {
                panic!("cannot cancel: milestones already confirmed");
            }
        }

        let refund = shipment.total_amount - shipment.released_amount;
        let token_client = token::Client::new(&env, &shipment.token);
        token_client.transfer(&env.current_contract_address(), &shipment.buyer, &refund);

        shipment.status = ShipmentStatus::Cancelled;

        env.storage()
            .persistent()
            .set(&DataKey::Shipment(shipment_id.clone()), &shipment);

        env.events().publish(
            (Symbol::new(&env, "shipment_cancelled"), shipment_id.clone()),
            refund,
        );
    }

    // ----------------------------------------------------------
    // SUPPLIER CANCEL  (#10)
    // ----------------------------------------------------------

    /// Supplier cancels after buyer_response_deadline_ledgers have passed
    /// with at least one milestone stuck in ProofSubmitted.
    /// Buyer receives refund minus supplier_penalty_bps of the remaining escrow.
    pub fn supplier_cancel(env: Env, supplier: Address, shipment_id: String) {
        supplier.require_auth();

        let mut shipment = Self::get_shipment_internal(&env, &shipment_id);

        if shipment.status != ShipmentStatus::Active {
            panic!("shipment is not active");
        }
        if supplier != shipment.supplier {
            panic!("unauthorized");
        }
        let policy: CancelPolicy = env
            .storage()
            .persistent()
            .get(&DataKey::CancelPolicy(shipment_id.clone()))
            .unwrap_or(CancelPolicy { response_deadline: 0, penalty_bps: 0 });

        if policy.response_deadline == 0 {
            panic!("supplier cancellation not enabled for this shipment");
        }

        // Find the earliest ProofSubmitted milestone and check deadline.
        let current_ledger = env.ledger().sequence();
        let mut deadline_passed = false;
        for i in 0..shipment.milestones.len() {
            let m = shipment.milestones.get(i).unwrap();
            if m.status == MilestoneStatus::ProofSubmitted {
                let submitted_at: u32 = env
                    .storage()
                    .persistent()
                    .get(&DataKey::ProofSubmittedAt(shipment_id.clone(), i))
                    .unwrap_or(0);
                if current_ledger >= submitted_at + policy.response_deadline {
                    deadline_passed = true;
                    break;
                }
            }
        }

        if !deadline_passed {
            panic!("buyer response deadline has not passed");
        }

        let remaining = shipment.total_amount - shipment.released_amount;
        let penalty = (remaining * policy.penalty_bps as i128) / 10_000;
        let refund = remaining - penalty;

        let token_client = token::Client::new(&env, &shipment.token);
        if penalty > 0 {
            token_client.transfer(&env.current_contract_address(), &shipment.supplier, &penalty);
        }
        if refund > 0 {
            token_client.transfer(&env.current_contract_address(), &shipment.buyer, &refund);
        }

        shipment.status = ShipmentStatus::Cancelled;

        env.storage()
            .persistent()
            .set(&DataKey::Shipment(shipment_id.clone()), &shipment);

        env.events().publish(
            (Symbol::new(&env, "supplier_cancellation"), shipment_id.clone()),
            (penalty, refund),
        );
    }

    // ----------------------------------------------------------
    // PROPOSE AMENDMENT  (#9)
    // ----------------------------------------------------------

    /// Buyer or supplier proposes an amendment to a Pending milestone.
    /// When both parties have proposed identical (new_percent, new_name), the amendment is applied.
    pub fn propose_amendment(
        env: Env,
        caller: Address,
        shipment_id: String,
        milestone_index: u32,
        new_percent: u32,
        new_name: String,
    ) {
        caller.require_auth();

        let mut shipment = Self::get_shipment_internal(&env, &shipment_id);

        if shipment.status != ShipmentStatus::Active {
            panic!("shipment is not active");
        }
        if caller != shipment.buyer && caller != shipment.supplier {
            panic!("unauthorized");
        }
        if milestone_index as usize >= shipment.milestones.len() as usize {
            panic!("invalid milestone index");
        }

        let milestone = shipment.milestones.get(milestone_index).unwrap();
        if milestone.status != MilestoneStatus::Pending {
            panic!("can only amend a pending milestone");
        }

        let amendment_key = DataKey::Amendment(shipment_id.clone(), milestone_index);

        let mut proposal: AmendmentProposal = env
            .storage()
            .temporary()
            .get(&amendment_key)
            .unwrap_or(AmendmentProposal {
                new_percent,
                new_name: new_name.clone(),
                buyer_agreed: false,
                supplier_agreed: false,
            });

        // If the stored proposal has different terms, reset it.
        if proposal.new_percent != new_percent || proposal.new_name != new_name {
            proposal = AmendmentProposal {
                new_percent,
                new_name: new_name.clone(),
                buyer_agreed: false,
                supplier_agreed: false,
            };
        }

        if caller == shipment.buyer {
            proposal.buyer_agreed = true;
        } else {
            proposal.supplier_agreed = true;
        }

        env.events().publish(
            (Symbol::new(&env, "amendment_proposed"), shipment_id.clone()),
            (milestone_index, new_percent),
        );

        if proposal.buyer_agreed && proposal.supplier_agreed {
            // Validate new percentages sum to 100.
            let mut total: u32 = 0;
            for i in 0..shipment.milestones.len() {
                if i == milestone_index {
                    total += new_percent;
                } else {
                    total += shipment.milestones.get(i).unwrap().payment_percent;
                }
            }
            if total != 100 {
                panic!("milestone percentages must sum to 100");
            }

            let mut m = shipment.milestones.get(milestone_index).unwrap();
            m.payment_percent = new_percent;
            m.name = new_name;
            shipment.milestones.set(milestone_index, m);

            env.storage()
                .persistent()
                .set(&DataKey::Shipment(shipment_id.clone()), &shipment);

            env.storage().temporary().remove(&amendment_key);

            env.events().publish(
                (Symbol::new(&env, "amendment_accepted"), shipment_id.clone()),
                milestone_index,
            );
        } else {
            env.storage().temporary().set(&amendment_key, &proposal);
        }
    }

    // ----------------------------------------------------------
    // READ-ONLY QUERIES
    // ----------------------------------------------------------

    pub fn get_shipment(env: Env, shipment_id: String) -> Shipment {
        Self::get_shipment_internal(&env, &shipment_id)
    }

    pub fn get_milestone(env: Env, shipment_id: String, milestone_index: u32) -> Milestone {
        let shipment = Self::get_shipment_internal(&env, &shipment_id);
        shipment
            .milestones
            .get(milestone_index)
            .unwrap_or_else(|| panic!("invalid milestone index"))
    }

    pub fn get_escrow_balance(env: Env, shipment_id: String) -> i128 {
        let shipment = Self::get_shipment_internal(&env, &shipment_id);
        shipment.total_amount - shipment.released_amount
    }

    // ----------------------------------------------------------
    // INTERNAL HELPERS
    // ----------------------------------------------------------

    fn get_shipment_internal(env: &Env, shipment_id: &String) -> Shipment {
        env.storage()
            .persistent()
            .get(&DataKey::Shipment(shipment_id.clone()))
            .unwrap_or_else(|| panic!("shipment not found"))
    }
}

mod test;
