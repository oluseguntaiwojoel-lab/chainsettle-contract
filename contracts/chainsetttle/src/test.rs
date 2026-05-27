#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger as _},
    token, vec, Address, Env, String,
};

// ============================================================
// TEST HELPERS
// ============================================================

fn setup() -> (Env, Address, Address, Address, Address, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(ChainSettleContract, ());

    let token_admin = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(token_admin.clone())
        .address();
    let token_client = token::StellarAssetClient::new(&env, &token_id);

    let buyer = Address::generate(&env);
    let supplier = Address::generate(&env);
    let logistics = Address::generate(&env);
    let arbiter = Address::generate(&env);

    token_client.mint(&buyer, &10_000_000_000);

    let client = ChainSettleContractClient::new(&env, &contract_id);
    client.init(&buyer);

    (env, contract_id, token_id, buyer, supplier, logistics, arbiter)
}

fn build_milestones(env: &Env) -> Vec<Milestone> {
    vec![
        env,
        Milestone {
            name: String::from_str(env, "Goods Dispatched"),
            payment_percent: 25,
            proof_hash: String::from_str(env, ""),
            status: MilestoneStatus::Pending,
            release_after_ledger: 0,
        },
        Milestone {
            name: String::from_str(env, "In Transit"),
            payment_percent: 50,
            proof_hash: String::from_str(env, ""),
            status: MilestoneStatus::Pending,
            release_after_ledger: 0,
        },
        Milestone {
            name: String::from_str(env, "Delivered"),
            payment_percent: 25,
            proof_hash: String::from_str(env, ""),
            status: MilestoneStatus::Pending,
            release_after_ledger: 0,
        },
    ]
}

/// Helper: create a shipment with explicit sequential + holdback flags.
fn create(
    client: &ChainSettleContractClient,
    env: &Env,
    id: &str,
    buyer: &Address,
    supplier: &Address,
    logistics: &Address,
    arbiter: &Address,
    token_id: &Address,
    amount: i128,
    sequential: bool,
    holdback: u32,
) {
    client.create_shipment(
        &String::from_str(env, id),
        buyer,
        supplier,
        logistics,
        arbiter,
        token_id,
        &amount,
        &build_milestones(env),
        &sequential,
        &holdback,
    );
}

// ============================================================
// EXISTING TESTS (updated for new create_shipment signature)
// ============================================================

#[test]
fn test_create_shipment_success() {
    let (env, contract_id, token_id, buyer, supplier, logistics, arbiter) = setup();
    let client = ChainSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let total_amount: i128 = 1_000_000_000;
    create(&client, &env, "SHIP-001", &buyer, &supplier, &logistics, &arbiter, &token_id, total_amount, false, 0);

    assert_eq!(token_client.balance(&buyer), 10_000_000_000 - total_amount);
    assert_eq!(token_client.balance(&contract_id), total_amount);

    let shipment = client.get_shipment(&String::from_str(&env, "SHIP-001"));
    assert_eq!(shipment.status, ShipmentStatus::Active);
    assert_eq!(shipment.total_amount, total_amount);
    assert_eq!(shipment.released_amount, 0);
    assert_eq!(shipment.milestones.len(), 3);
    assert!(!shipment.sequential);
    assert_eq!(shipment.holdback_ledgers, 0);
}

#[test]
#[should_panic(expected = "milestone percentages must sum to 100")]
fn test_create_shipment_invalid_percentages() {
    let (env, contract_id, token_id, buyer, supplier, logistics, arbiter) = setup();
    let client = ChainSettleContractClient::new(&env, &contract_id);

    let bad_milestones = vec![
        &env,
        Milestone {
            name: String::from_str(&env, "Step 1"),
            payment_percent: 30,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Pending,
            release_after_ledger: 0,
        },
        Milestone {
            name: String::from_str(&env, "Step 2"),
            payment_percent: 30,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Pending,
            release_after_ledger: 0,
        },
        Milestone {
            name: String::from_str(&env, "Step 3"),
            payment_percent: 30,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Pending,
            release_after_ledger: 0,
        },
    ];

    client.create_shipment(
        &String::from_str(&env, "SHIP-BAD"),
        &buyer,
        &supplier,
        &logistics,
        &arbiter,
        &token_id,
        &1_000_000_000,
        &bad_milestones,
        &false,
        &0,
    );
}

#[test]
fn test_full_shipment_lifecycle() {
    let (env, contract_id, token_id, buyer, supplier, logistics, arbiter) = setup();
    let client = ChainSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let total_amount: i128 = 1_000_000_000;
    create(&client, &env, "SHIP-FULL", &buyer, &supplier, &logistics, &arbiter, &token_id, total_amount, false, 0);

    let id = String::from_str(&env, "SHIP-FULL");
    client.submit_proof(&supplier, &id, &0, &String::from_str(&env, "ipfs://dispatch"));
    client.confirm_milestone(&buyer, &id, &0);

    client.submit_proof(&logistics, &id, &1, &String::from_str(&env, "ipfs://transit"));
    client.confirm_milestone(&buyer, &id, &1);

    client.submit_proof(&supplier, &id, &2, &String::from_str(&env, "ipfs://delivered"));
    client.confirm_milestone(&buyer, &id, &2);

    let shipment = client.get_shipment(&id);
    assert_eq!(shipment.status, ShipmentStatus::Completed);
    assert_eq!(shipment.released_amount, total_amount);
    assert_eq!(token_client.balance(&supplier), total_amount);
    assert_eq!(client.get_escrow_balance(&id), 0);
}

#[test]
fn test_raise_and_resolve_dispute_approve() {
    let (env, contract_id, token_id, buyer, supplier, logistics, arbiter) = setup();
    let client = ChainSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let total_amount: i128 = 1_000_000_000;
    create(&client, &env, "SHIP-DISPUTE", &buyer, &supplier, &logistics, &arbiter, &token_id, total_amount, false, 0);

    let id = String::from_str(&env, "SHIP-DISPUTE");
    client.submit_proof(&supplier, &id, &0, &String::from_str(&env, "ipfs://proof"));
    client.raise_dispute(&buyer, &id, &0);
    assert_eq!(client.get_milestone(&id, &0).status, MilestoneStatus::Disputed);

    client.resolve_dispute(&arbiter, &id, &0, &true);
    assert_eq!(token_client.balance(&supplier), total_amount * 25 / 100);
}

#[test]
fn test_raise_and_resolve_dispute_reject() {
    let (env, contract_id, token_id, buyer, supplier, logistics, arbiter) = setup();
    let client = ChainSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let total_amount: i128 = 1_000_000_000;
    create(&client, &env, "SHIP-REJECT", &buyer, &supplier, &logistics, &arbiter, &token_id, total_amount, false, 0);

    let id = String::from_str(&env, "SHIP-REJECT");
    client.submit_proof(&supplier, &id, &0, &String::from_str(&env, "ipfs://bad-proof"));
    client.raise_dispute(&buyer, &id, &0);
    client.resolve_dispute(&arbiter, &id, &0, &false);

    assert_eq!(client.get_milestone(&id, &0).status, MilestoneStatus::Pending);
    assert_eq!(token_client.balance(&supplier), 0);
}

#[test]
fn test_cancel_shipment() {
    let (env, contract_id, token_id, buyer, supplier, logistics, arbiter) = setup();
    let client = ChainSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let total_amount: i128 = 1_000_000_000;
    let balance_before = token_client.balance(&buyer);
    create(&client, &env, "SHIP-CANCEL", &buyer, &supplier, &logistics, &arbiter, &token_id, total_amount, false, 0);

    let id = String::from_str(&env, "SHIP-CANCEL");
    client.cancel_shipment(&buyer, &id);

    assert_eq!(client.get_shipment(&id).status, ShipmentStatus::Cancelled);
    assert_eq!(token_client.balance(&buyer), balance_before);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_unauthorized_confirm_milestone() {
    let (env, contract_id, token_id, buyer, supplier, logistics, arbiter) = setup();
    let client = ChainSettleContractClient::new(&env, &contract_id);

    create(&client, &env, "SHIP-AUTH", &buyer, &supplier, &logistics, &arbiter, &token_id, 1_000_000_000, false, 0);

    let id = String::from_str(&env, "SHIP-AUTH");
    client.submit_proof(&supplier, &id, &0, &String::from_str(&env, "ipfs://proof"));
    client.confirm_milestone(&supplier, &id, &0); // should panic
}

// ============================================================
// ISSUE #1: SEQUENTIAL MILESTONE TESTS
// ============================================================

#[test]
fn test_sequential_happy_path() {
    let (env, contract_id, token_id, buyer, supplier, logistics, arbiter) = setup();
    let client = ChainSettleContractClient::new(&env, &contract_id);

    create(&client, &env, "SEQ-OK", &buyer, &supplier, &logistics, &arbiter, &token_id, 1_000_000_000, true, 0);
    let id = String::from_str(&env, "SEQ-OK");

    // Must go in order: 0 → 1 → 2
    client.submit_proof(&supplier, &id, &0, &String::from_str(&env, "ipfs://0"));
    client.confirm_milestone(&buyer, &id, &0);

    client.submit_proof(&logistics, &id, &1, &String::from_str(&env, "ipfs://1"));
    client.confirm_milestone(&buyer, &id, &1);

    client.submit_proof(&supplier, &id, &2, &String::from_str(&env, "ipfs://2"));
    client.confirm_milestone(&buyer, &id, &2);

    assert_eq!(client.get_shipment(&id).status, ShipmentStatus::Completed);
}

#[test]
#[should_panic(expected = "milestone is not in pending status")]
fn test_sequential_out_of_order_rejected() {
    let (env, contract_id, token_id, buyer, supplier, logistics, arbiter) = setup();
    let client = ChainSettleContractClient::new(&env, &contract_id);

    create(&client, &env, "SEQ-BAD", &buyer, &supplier, &logistics, &arbiter, &token_id, 1_000_000_000, true, 0);
    let id = String::from_str(&env, "SEQ-BAD");

    // Skip milestone 0 — should panic
    client.submit_proof(&logistics, &id, &1, &String::from_str(&env, "ipfs://1"));
}

#[test]
fn test_non_sequential_baseline() {
    let (env, contract_id, token_id, buyer, supplier, logistics, arbiter) = setup();
    let client = ChainSettleContractClient::new(&env, &contract_id);

    create(&client, &env, "NONSEQ", &buyer, &supplier, &logistics, &arbiter, &token_id, 1_000_000_000, false, 0);
    let id = String::from_str(&env, "NONSEQ");

    // Submit milestone 2 before 0 — allowed when sequential=false
    client.submit_proof(&supplier, &id, &2, &String::from_str(&env, "ipfs://2"));
    assert_eq!(client.get_milestone(&id, &2).status, MilestoneStatus::ProofSubmitted);
}

// ============================================================
// ISSUE #2: MULTI-TOKEN / WHITELIST TESTS
// ============================================================

#[test]
fn test_usdc_shipment_no_whitelist() {
    // Empty whitelist → any token accepted
    let (env, contract_id, token_id, buyer, supplier, logistics, arbiter) = setup();
    let client = ChainSettleContractClient::new(&env, &contract_id);
    create(&client, &env, "USDC-SHIP", &buyer, &supplier, &logistics, &arbiter, &token_id, 1_000_000_000, false, 0);
    assert_eq!(client.get_shipment(&String::from_str(&env, "USDC-SHIP")).status, ShipmentStatus::Active);
}

#[test]
fn test_xlm_shipment_whitelisted() {
    let (env, contract_id, token_id, buyer, supplier, logistics, arbiter) = setup();
    let client = ChainSettleContractClient::new(&env, &contract_id);

    // Register a second "XLM" token
    let xlm_admin = Address::generate(&env);
    let xlm_id = env.register_stellar_asset_contract_v2(xlm_admin.clone()).address();
    token::StellarAssetClient::new(&env, &xlm_id).mint(&buyer, &10_000_000_000);

    // Whitelist both tokens
    client.add_allowed_token(&token_id);
    client.add_allowed_token(&xlm_id);

    create(&client, &env, "XLM-SHIP", &buyer, &supplier, &logistics, &arbiter, &xlm_id, 1_000_000_000, false, 0);
    assert_eq!(client.get_shipment(&String::from_str(&env, "XLM-SHIP")).status, ShipmentStatus::Active);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_non_whitelisted_token_rejected() {
    let (env, contract_id, token_id, buyer, supplier, logistics, arbiter) = setup();
    let client = ChainSettleContractClient::new(&env, &contract_id);

    // Whitelist only token_id; try to use a different token
    client.add_allowed_token(&token_id);

    let other_admin = Address::generate(&env);
    let other_token = env.register_stellar_asset_contract_v2(other_admin.clone()).address();
    token::StellarAssetClient::new(&env, &other_token).mint(&buyer, &10_000_000_000);

    create(&client, &env, "BAD-TOKEN", &buyer, &supplier, &logistics, &arbiter, &other_token, 1_000_000_000, false, 0);
}

#[test]
fn test_whitelist_toggle() {
    let (env, contract_id, token_id, buyer, supplier, logistics, arbiter) = setup();
    let client = ChainSettleContractClient::new(&env, &contract_id);

    // Add then remove token_id — list becomes empty → permissionless again
    client.add_allowed_token(&token_id);
    client.remove_allowed_token(&token_id);

    // Should succeed because list is now empty
    create(&client, &env, "TOGGLE-SHIP", &buyer, &supplier, &logistics, &arbiter, &token_id, 1_000_000_000, false, 0);
    assert_eq!(client.get_shipment(&String::from_str(&env, "TOGGLE-SHIP")).status, ShipmentStatus::Active);
}

// ============================================================
// ISSUE #3: PARTIAL CANCELLATION TESTS
// ============================================================

#[test]
fn test_cancel_zero_confirmed() {
    let (env, contract_id, token_id, buyer, supplier, logistics, arbiter) = setup();
    let client = ChainSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let total: i128 = 1_000_000_000;
    let before = token_client.balance(&buyer);
    create(&client, &env, "CANCEL-ZERO", &buyer, &supplier, &logistics, &arbiter, &token_id, total, false, 0);

    client.cancel_shipment(&buyer, &String::from_str(&env, "CANCEL-ZERO"));
    assert_eq!(token_client.balance(&buyer), before); // full refund
}

#[test]
fn test_cancel_partial_confirmed() {
    let (env, contract_id, token_id, buyer, supplier, logistics, arbiter) = setup();
    let client = ChainSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let total: i128 = 1_000_000_000;
    create(&client, &env, "CANCEL-PART", &buyer, &supplier, &logistics, &arbiter, &token_id, total, false, 0);

    let id = String::from_str(&env, "CANCEL-PART");
    // Confirm milestone 0 (25%)
    client.submit_proof(&supplier, &id, &0, &String::from_str(&env, "ipfs://0"));
    client.confirm_milestone(&buyer, &id, &0);

    let released = total * 25 / 100;
    assert_eq!(token_client.balance(&supplier), released); // supplier keeps 25%

    client.cancel_shipment(&buyer, &id);

    let shipment = client.get_shipment(&id);
    assert_eq!(shipment.status, ShipmentStatus::Cancelled);
    // Buyer gets back the remaining 75%
    assert_eq!(token_client.balance(&buyer), 10_000_000_000 - total + (total - released));
    // Supplier still has the 25% already released
    assert_eq!(token_client.balance(&supplier), released);
}

#[test]
#[should_panic(expected = "cannot cancel: dispute must be resolved first")]
fn test_cancel_blocked_by_dispute() {
    let (env, contract_id, token_id, buyer, supplier, logistics, arbiter) = setup();
    let client = ChainSettleContractClient::new(&env, &contract_id);

    create(&client, &env, "CANCEL-DISP", &buyer, &supplier, &logistics, &arbiter, &token_id, 1_000_000_000, false, 0);

    let id = String::from_str(&env, "CANCEL-DISP");
    client.submit_proof(&supplier, &id, &0, &String::from_str(&env, "ipfs://proof"));
    client.raise_dispute(&buyer, &id, &0);

    client.cancel_shipment(&buyer, &id); // should panic
}

// ============================================================
// ISSUE #4: HOLDBACK TESTS
// ============================================================

#[test]
fn test_holdback_happy_path() {
    let (env, contract_id, token_id, buyer, supplier, logistics, arbiter) = setup();
    let client = ChainSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let total: i128 = 1_000_000_000;
    create(&client, &env, "HOLD-OK", &buyer, &supplier, &logistics, &arbiter, &token_id, total, false, 10);

    let id = String::from_str(&env, "HOLD-OK");
    client.submit_proof(&supplier, &id, &0, &String::from_str(&env, "ipfs://0"));
    client.confirm_milestone(&buyer, &id, &0);

    // Payment NOT yet transferred — still held
    assert_eq!(token_client.balance(&supplier), 0);
    assert_eq!(client.get_milestone(&id, &0).status, MilestoneStatus::ConfirmedHeld);

    // Advance ledger past holdback window
    env.ledger().set(soroban_sdk::testutils::LedgerInfo {
        timestamp: 0,
        protocol_version: 22,
        sequence_number: env.ledger().sequence() + 11,
        network_id: Default::default(),
        base_reserve: 10,
        min_temp_entry_ttl: 10,
        min_persistent_entry_ttl: 10,
        max_entry_ttl: 6_300_000,
    });

    client.release_held_payment(&id, &0);
    assert_eq!(token_client.balance(&supplier), total * 25 / 100);
    assert_eq!(client.get_milestone(&id, &0).status, MilestoneStatus::Confirmed);
}

#[test]
fn test_holdback_early_dispute_cancels_hold() {
    let (env, contract_id, token_id, buyer, supplier, logistics, arbiter) = setup();
    let client = ChainSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let total: i128 = 1_000_000_000;
    create(&client, &env, "HOLD-DISP", &buyer, &supplier, &logistics, &arbiter, &token_id, total, false, 100);

    let id = String::from_str(&env, "HOLD-DISP");
    client.submit_proof(&supplier, &id, &0, &String::from_str(&env, "ipfs://0"));
    client.confirm_milestone(&buyer, &id, &0);

    // Dispute within holdback window
    client.raise_dispute(&buyer, &id, &0);
    assert_eq!(client.get_milestone(&id, &0).status, MilestoneStatus::Disputed);
    assert_eq!(client.get_milestone(&id, &0).release_after_ledger, 0); // hold cancelled
    assert_eq!(token_client.balance(&supplier), 0); // no payment yet
}

#[test]
#[should_panic(expected = "holdback period not yet expired")]
fn test_release_before_expiry_panics() {
    let (env, contract_id, token_id, buyer, supplier, logistics, arbiter) = setup();
    let client = ChainSettleContractClient::new(&env, &contract_id);

    create(&client, &env, "HOLD-EARLY", &buyer, &supplier, &logistics, &arbiter, &token_id, 1_000_000_000, false, 100);

    let id = String::from_str(&env, "HOLD-EARLY");
    client.submit_proof(&supplier, &id, &0, &String::from_str(&env, "ipfs://0"));
    client.confirm_milestone(&buyer, &id, &0);

    // Try to release immediately — should panic
    client.release_held_payment(&id, &0);
}

#[test]
fn test_no_holdback_immediate_transfer() {
    let (env, contract_id, token_id, buyer, supplier, logistics, arbiter) = setup();
    let client = ChainSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let total: i128 = 1_000_000_000;
    create(&client, &env, "NO-HOLD", &buyer, &supplier, &logistics, &arbiter, &token_id, total, false, 0);

    let id = String::from_str(&env, "NO-HOLD");
    client.submit_proof(&supplier, &id, &0, &String::from_str(&env, "ipfs://0"));
    client.confirm_milestone(&buyer, &id, &0);

    // Immediate transfer — no holdback
    assert_eq!(token_client.balance(&supplier), total * 25 / 100);
    assert_eq!(client.get_milestone(&id, &0).status, MilestoneStatus::Confirmed);
}
