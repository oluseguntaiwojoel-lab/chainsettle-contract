#![no_std]

use soroban_sdk::{
    contract, contractimpl, contracttype, token, Address, Env, String, Vec, Symbol,
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
    // Confirmed but payment held until release_after_ledger
    ConfirmedHeld,
}

#[contracttype]
#[derive(Clone)]
pub struct Milestone {
    pub name: String,
    pub payment_percent: u32,
    pub proof_hash: String,
    pub status: MilestoneStatus,
    // Set when holdback_ledgers > 0 and milestone is confirmed
    pub release_after_ledger: u32,
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
    // Issue #1: enforce sequential milestone ordering
    pub sequential: bool,
    // Issue #4: ledgers to hold payment after confirmation (0 = immediate)
    pub holdback_ledgers: u32,
}

// ============================================================
// STORAGE KEYS
// ============================================================

#[contracttype]
pub enum DataKey {
    Shipment(String),
    AllShipments,
    Admin,
    // Issue #2: allowed token whitelist
    AllowedTokens,
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
    // ISSUE #2: TOKEN WHITELIST MANAGEMENT
    // ----------------------------------------------------------

    /// Admin adds a token to the allowed list.
    /// When the list is non-empty, only listed tokens are accepted.
    pub fn add_allowed_token(env: Env, token: Address) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic!("not initialised"));
        admin.require_auth();

        let mut list: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::AllowedTokens)
            .unwrap_or_else(|| Vec::new(&env));

        for i in 0..list.len() {
            if list.get(i).unwrap() == token {
                return; // already present
            }
        }
        list.push_back(token);
        env.storage().instance().set(&DataKey::AllowedTokens, &list);
    }

    /// Admin removes a token from the allowed list.
    pub fn remove_allowed_token(env: Env, token: Address) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic!("not initialised"));
        admin.require_auth();

        let list: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::AllowedTokens)
            .unwrap_or_else(|| Vec::new(&env));

        let mut new_list: Vec<Address> = Vec::new(&env);
        for i in 0..list.len() {
            let t = list.get(i).unwrap();
            if t != token {
                new_list.push_back(t);
            }
        }
        env.storage().instance().set(&DataKey::AllowedTokens, &new_list);
    }

    // ----------------------------------------------------------
    // CREATE SHIPMENT
    // ----------------------------------------------------------

    /// Create a new shipment and lock funds in escrow.
    /// sequential=true enforces in-order milestone proof submission.
    /// holdback_ledgers>0 delays payment transfer after confirmation.
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
        sequential: bool,
        holdback_ledgers: u32,
    ) -> String {
        buyer.require_auth();

        if total_amount <= 0 {
            panic!("amount must be greater than zero");
        }

        // Issue #2: enforce token whitelist when non-empty
        let allowed: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::AllowedTokens)
            .unwrap_or_else(|| Vec::new(&env));
        if allowed.len() > 0 {
            let mut found = false;
            for i in 0..allowed.len() {
                if allowed.get(i).unwrap() == token {
                    found = true;
                    break;
                }
            }
            if !found {
                panic!("unauthorized");
            }
        }

        let mut total_percent: u32 = 0;
        for i in 0..milestones.len() {
            let m = milestones.get(i).unwrap();
            total_percent += m.payment_percent;
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
            sequential,
            holdback_ledgers,
        };

        env.storage()
            .persistent()
            .set(&DataKey::Shipment(shipment_id.clone()), &shipment);

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

    /// Supplier or logistics party submits proof for a milestone.
    /// Issue #1: when sequential=true, all prior milestones must be Confirmed or Resolved.
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

        let idx = milestone_index as usize;
        if idx >= shipment.milestones.len() as usize {
            panic!("invalid milestone index");
        }

        // Issue #1: sequential enforcement
        if shipment.sequential && milestone_index > 0 {
            for i in 0..milestone_index {
                let prev = shipment.milestones.get(i).unwrap();
                if prev.status != MilestoneStatus::Confirmed
                    && prev.status != MilestoneStatus::Resolved
                {
                    panic!("milestone is not in pending status");
                }
            }
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

        env.events().publish(
            (Symbol::new(&env, "proof_submitted"), shipment_id.clone()),
            milestone_index,
        );
    }

    // ----------------------------------------------------------
    // CONFIRM MILESTONE
    // ----------------------------------------------------------

    /// Buyer confirms a milestone.
    /// Issue #4: when holdback_ledgers > 0, records release_after_ledger instead of
    /// transferring immediately; status becomes ConfirmedHeld.
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

        let idx = milestone_index as usize;
        if idx >= shipment.milestones.len() as usize {
            panic!("invalid milestone index");
        }

        let mut milestone = shipment.milestones.get(milestone_index).unwrap();

        if milestone.status != MilestoneStatus::ProofSubmitted {
            panic!("milestone proof not yet submitted");
        }

        let payment = (shipment.total_amount * milestone.payment_percent as i128) / 100;

        if shipment.holdback_ledgers > 0 {
            // Issue #4: hold payment
            milestone.release_after_ledger =
                env.ledger().sequence() + shipment.holdback_ledgers;
            milestone.status = MilestoneStatus::ConfirmedHeld;
            shipment.milestones.set(milestone_index, milestone.clone());

            env.storage()
                .persistent()
                .set(&DataKey::Shipment(shipment_id.clone()), &shipment);

            env.events().publish(
                (Symbol::new(&env, "payment_held"), shipment_id.clone()),
                (milestone_index, milestone.release_after_ledger),
            );
        } else {
            milestone.status = MilestoneStatus::Confirmed;
            shipment.milestones.set(milestone_index, milestone.clone());
            shipment.released_amount += payment;

            let token_client = token::Client::new(&env, &shipment.token);
            token_client.transfer(
                &env.current_contract_address(),
                &shipment.supplier,
                &payment,
            );

            let all_done = Self::all_milestones_done(&shipment);
            if all_done {
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
    }

    // ----------------------------------------------------------
    // ISSUE #4: RELEASE HELD PAYMENT
    // ----------------------------------------------------------

    /// Anyone can call this once the holdback window has passed.
    /// Transfers the held payment to the supplier.
    pub fn release_held_payment(env: Env, shipment_id: String, milestone_index: u32) {
        let mut shipment = Self::get_shipment_internal(&env, &shipment_id);

        if shipment.status != ShipmentStatus::Active {
            panic!("shipment is not active");
        }

        let mut milestone = shipment.milestones.get(milestone_index).unwrap();

        if milestone.status != MilestoneStatus::ConfirmedHeld {
            panic!("milestone is not in pending status");
        }

        if env.ledger().sequence() < milestone.release_after_ledger {
            panic!("holdback period not yet expired");
        }

        let payment = (shipment.total_amount * milestone.payment_percent as i128) / 100;
        milestone.status = MilestoneStatus::Confirmed;
        milestone.release_after_ledger = 0;
        shipment.milestones.set(milestone_index, milestone);
        shipment.released_amount += payment;

        let token_client = token::Client::new(&env, &shipment.token);
        token_client.transfer(&env.current_contract_address(), &shipment.supplier, &payment);

        let all_done = Self::all_milestones_done(&shipment);
        if all_done {
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
    // RAISE DISPUTE
    // ----------------------------------------------------------

    /// Buyer raises a dispute on a ProofSubmitted or ConfirmedHeld milestone.
    /// Issue #4: disputing a ConfirmedHeld milestone cancels the holdback.
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

        if milestone.status != MilestoneStatus::ProofSubmitted
            && milestone.status != MilestoneStatus::ConfirmedHeld
        {
            panic!("can only dispute a submitted proof");
        }

        // Issue #4: cancel holdback if within window
        milestone.release_after_ledger = 0;
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
            token_client.transfer(
                &env.current_contract_address(),
                &shipment.supplier,
                &payment,
            );

            milestone.status = MilestoneStatus::Resolved;
        } else {
            milestone.status = MilestoneStatus::Pending;
            milestone.proof_hash = String::from_str(&env, "");
        }

        shipment.milestones.set(milestone_index, milestone);

        let all_done = Self::all_milestones_done(&shipment);
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
    // CANCEL SHIPMENT
    // ----------------------------------------------------------

    /// Cancel the shipment and refund unreleased escrow to the buyer.
    /// Issue #3: allowed even after some milestones are Confirmed; already-released
    /// funds stay with the supplier. Blocked if any milestone is Disputed.
    pub fn cancel_shipment(env: Env, buyer: Address, shipment_id: String) {
        buyer.require_auth();

        let mut shipment = Self::get_shipment_internal(&env, &shipment_id);

        if shipment.status != ShipmentStatus::Active {
            panic!("shipment is not active");
        }

        if buyer != shipment.buyer {
            panic!("unauthorized");
        }

        // Issue #3: block cancellation if any milestone is Disputed
        for i in 0..shipment.milestones.len() {
            let m = shipment.milestones.get(i).unwrap();
            if m.status == MilestoneStatus::Disputed {
                panic!("cannot cancel: dispute must be resolved first");
            }
        }

        let refund = shipment.total_amount - shipment.released_amount;
        if refund > 0 {
            let token_client = token::Client::new(&env, &shipment.token);
            token_client.transfer(&env.current_contract_address(), &shipment.buyer, &refund);
        }

        shipment.status = ShipmentStatus::Cancelled;

        env.storage()
            .persistent()
            .set(&DataKey::Shipment(shipment_id.clone()), &shipment);

        // Issue #3: event now carries refunded_amount
        env.events().publish(
            (Symbol::new(&env, "shipment_cancelled"), shipment_id.clone()),
            refund,
        );
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

    fn all_milestones_done(shipment: &Shipment) -> bool {
        (0..shipment.milestones.len()).all(|i| {
            let s = shipment.milestones.get(i).unwrap().status;
            s == MilestoneStatus::Confirmed || s == MilestoneStatus::Resolved
        })
    }
}

mod test;
