//! IPGraph stateful precompile.
//!
//! Ported from story-geth `core/vm/ipgraph.go`. Manages an on-chain IP asset graph
//! with parent/ancestor relationships and royalty calculations.
//!
//! Registered at address `0x0101` via [`ipgraph_precompile`].
//!
//! Uses [`DynPrecompile::new_stateful`] from `alloy-evm` (see
//! `alloy-evm/src/precompiles.rs`) because it reads/writes EVM state through
//! [`PrecompileInput::internals`] (`sload`/`sstore`).

use alloy_evm::precompiles::{DynPrecompile, PrecompileInput};
use alloy_primitives::{Address, B256, U256, address, b256, keccak256};
use alloy_sol_macro::sol;
use alloy_sol_types::SolCall;
use revm::precompile::{PrecompileError, PrecompileId, PrecompileOutput, PrecompileResult};
use std::collections::{BTreeSet, HashMap};

/// IPGraph precompile address (0x0101), matching story-geth `ipGraphAddress`.
pub const IPGRAPH_ADDRESS: Address = address!("0x0000000000000000000000000000000000000101");

/// ACL contract address, matching story-geth `aclAddress`.
const ACL_ADDRESS: Address = address!("0x1640A22a8A086747cD377b73954545e2Dfcc9Cad");

/// ACL storage slot, matching story-geth `aclSlot`.
const ACL_SLOT: B256 = b256!("af99b37fdaacca72ee7240cb1435cc9e498aee6ef4edc19c8cc0cd787f4e6800");

// Gas constants from story-geth `core/vm/ipgraph.go:14-21`.
const WRITE_GAS: u64 = 100;
const READ_GAS: u64 = 10;
const AVERAGE_ANCESTOR_COUNT: u64 = 30;
const AVERAGE_PARENT_COUNT: u64 = 4;
const INTRINSIC_GAS: u64 = 1000;
const EXTERNAL_READ_GAS: u64 = 2100;

/// Max uint32 value for royalty validation (ipgraph.go:48).
const MAX_UINT32: u64 = u32::MAX as u64;

// Selector definitions using `alloy_sol_macro::sol!` (same pattern as
// bera-reth `src/transaction/pol.rs`). Each function generates a struct
// with a `SELECTOR` constant ([u8; 4]) and `abi_decode`/`abi_encode` methods.
//
// The function signatures match story-geth `core/vm/ipgraph.go:30-48`.
sol! {
    interface IpGraph {
        function addParentIp(address ipId, address[] parentIpIds);
        function hasParentIp(address ipId, address parentIpId);
        function getParentIps(address ipId);
        function getParentIpsCount(address ipId);
        function getAncestorIps(address ipId);
        function getAncestorIpsCount(address ipId);
        function hasAncestorIp(address ipId, address ancestorIpId);
        function setRoyalty(address ipId, address parentIpId, uint256 royaltyPolicyKind, uint256 amount);
        function getRoyalty(address ipId, address ancestorIpId, uint256 royaltyPolicyKind);
        function getRoyaltyStack(address ipId, uint256 royaltyPolicyKind);

        // Ext variants — same logic, different gas costs (EXTERNAL_READ_GAS vs READ_GAS).
        function hasParentIpExt(address ipId, address parentIpId);
        function getParentIpsExt(address ipId);
        function getParentIpsCountExt(address ipId);
        function getAncestorIpsExt(address ipId);
        function getAncestorIpsCountExt(address ipId);
        function hasAncestorIpExt(address ipId, address ancestorIpId);
        function getRoyaltyExt(address ipId, address ancestorIpId, uint256 royaltyPolicyKind);
        function getRoyaltyStackExt(address ipId, uint256 royaltyPolicyKind);
    }
}

/// Computes required gas for the given selector and input.
///
/// Mirrors story-geth `RequiredGas` (ipgraph.go:53-134). The Ext variants
/// use `EXTERNAL_READ_GAS` instead of `READ_GAS` but otherwise identical logic.
fn required_gas(selector: [u8; 4], data: &[u8]) -> u64 {
    use IpGraph::*;

    match selector {
        // Write operations
        addParentIpCall::SELECTOR => {
            // Gas scales with parent count (ipgraph.go:62-68)
            if data.len() >= 96 {
                let parent_count = u64::from_be_bytes(data[88..96].try_into().unwrap_or([0; 8]));
                if parent_count > 1024 {
                    return u64::MAX;
                }
                INTRINSIC_GAS + WRITE_GAS * parent_count
            } else {
                INTRINSIC_GAS
            }
        }
        setRoyaltyCall::SELECTOR => WRITE_GAS,

        // Internal read operations (READ_GAS)
        hasParentIpCall::SELECTOR | getParentIpsCall::SELECTOR => READ_GAS * AVERAGE_PARENT_COUNT,
        getParentIpsCountCall::SELECTOR => READ_GAS,
        getAncestorIpsCall::SELECTOR | hasAncestorIpCall::SELECTOR => {
            READ_GAS * AVERAGE_ANCESTOR_COUNT * 2
        }
        getAncestorIpsCountCall::SELECTOR => READ_GAS * AVERAGE_PARENT_COUNT * 2,
        getRoyaltyCall::SELECTOR => {
            royalty_gas(data, READ_GAS, AVERAGE_ANCESTOR_COUNT, AVERAGE_PARENT_COUNT)
        }
        getRoyaltyStackCall::SELECTOR => {
            royalty_stack_gas(data, READ_GAS, AVERAGE_ANCESTOR_COUNT, AVERAGE_PARENT_COUNT)
        }

        // Ext read operations (EXTERNAL_READ_GAS) — same formulas, higher base cost
        hasParentIpExtCall::SELECTOR | getParentIpsExtCall::SELECTOR => {
            EXTERNAL_READ_GAS * AVERAGE_PARENT_COUNT
        }
        getParentIpsCountExtCall::SELECTOR => EXTERNAL_READ_GAS,
        getAncestorIpsExtCall::SELECTOR | hasAncestorIpExtCall::SELECTOR => {
            EXTERNAL_READ_GAS * AVERAGE_ANCESTOR_COUNT * 2
        }
        getAncestorIpsCountExtCall::SELECTOR => EXTERNAL_READ_GAS * AVERAGE_PARENT_COUNT * 2,
        getRoyaltyExtCall::SELECTOR => {
            royalty_gas(data, EXTERNAL_READ_GAS, AVERAGE_ANCESTOR_COUNT, AVERAGE_PARENT_COUNT)
        }
        getRoyaltyStackExtCall::SELECTOR => {
            royalty_stack_gas(
                data,
                EXTERNAL_READ_GAS,
                AVERAGE_ANCESTOR_COUNT,
                AVERAGE_PARENT_COUNT,
            )
        }

        _ => INTRINSIC_GAS,
    }
}

/// Gas for getRoyalty/getRoyaltyExt — depends on royalty policy kind (ipgraph.go:83-91).
fn royalty_gas(data: &[u8], base: u64, ancestor_count: u64, _parent_count: u64) -> u64 {
    // royaltyPolicyKind is the 3rd arg (offset 64 from args start)
    let kind = royalty_policy_kind(data, 64);
    match kind {
        0 => base * (ancestor_count * 3),     // LAP
        1 => base * (ancestor_count * 2 + 2), // LRP
        _ => INTRINSIC_GAS,
    }
}

/// Gas for getRoyaltyStack/getRoyaltyStackExt — depends on royalty policy kind (ipgraph.go:92-100).
fn royalty_stack_gas(data: &[u8], base: u64, ancestor_count: u64, parent_count: u64) -> u64 {
    // royaltyPolicyKind is the 2nd arg (offset 32 from args start)
    let kind = royalty_policy_kind(data, 32);
    match kind {
        0 => base * (parent_count + 1), // LAP
        1 => base * (ancestor_count * 2), // LRP
        _ => INTRINSIC_GAS,
    }
}

/// Reads royalty policy kind from calldata at the given offset (from start of args, not selector).
fn royalty_policy_kind(data: &[u8], offset: usize) -> u64 {
    // data includes selector (4 bytes), offset is from args start
    let start = 4 + offset + 24; // skip selector + offset + 24 leading zero bytes of uint256
    let end = start + 8;
    if data.len() >= end {
        u64::from_be_bytes(data[start..end].try_into().unwrap_or([0; 8]))
    } else {
        u64::MAX // unknown kind
    }
}

// --- Storage helpers ---
// These mirror story-geth's storage patterns using `PrecompileInput::internals`
// (`sload`/`sstore` from `alloy-evm/src/traits.rs`).
//
// StorageKey and StorageValue are both U256.

/// Reads a storage slot. Maps to story-geth `evm.StateDB.GetState(addr, slot)`.
fn sload(
    input: &mut PrecompileInput<'_>,
    address: Address,
    key: U256,
) -> Result<U256, PrecompileError> {
    input
        .internals_mut()
        .sload(address, key)
        .map(|r| r.data)
        .map_err(|e| PrecompileError::other(format!("sload failed: {e}")))
}

/// Writes a storage slot. Maps to story-geth `evm.StateDB.SetState(addr, slot, value)`.
fn sstore(
    input: &mut PrecompileInput<'_>,
    address: Address,
    key: U256,
    value: U256,
) -> Result<(), PrecompileError> {
    input
        .internals_mut()
        .sstore(address, key, value)
        .map(|_| ())
        .map_err(|e| PrecompileError::other(format!("sstore failed: {e}")))
}

/// ACL check — mirrors story-geth `isAllowed` (ipgraph.go:190-201).
///
/// Reads `keccak256(caller ++ aclSlot)` from `aclAddress` storage.
/// Returns true if the value is 1.
fn is_allowed(input: &mut PrecompileInput<'_>) -> Result<bool, PrecompileError> {
    // story-geth: slot = keccak256(evm.caller.Bytes(), aclSlot.Bytes())
    // caller is 20 bytes, aclSlot is 32 bytes → 52-byte preimage
    let mut buf = [0u8; 52];
    buf[..20].copy_from_slice(input.caller.as_slice());
    buf[20..].copy_from_slice(ACL_SLOT.as_slice());
    let slot = U256::from_be_bytes(keccak256(buf).0);

    let value = sload(input, ACL_ADDRESS, slot)?;
    Ok(value == U256::from(1))
}

/// Checks that the call is not a DELEGATECALL/CALLCODE.
///
/// Uses `PrecompileInput::is_direct_call()` from `alloy-evm/src/precompiles.rs:633`.
/// Maps to story-geth's `evm.currentPrecompileCallType == DELEGATECALL` check.
fn require_direct_call(input: &PrecompileInput<'_>) -> Result<(), PrecompileError> {
    if !input.is_direct_call() {
        return Err(PrecompileError::other("cannot be called with DELEGATECALL"));
    }
    Ok(())
}

/// Reads parent count for an IP. Storage key = left-padded ipId address.
///
/// Maps to story-geth: `evm.StateDB.GetState(ipGraphAddress, BytesToHash(ipId.Bytes()))`.
fn get_parent_count(input: &mut PrecompileInput<'_>, ip_id: Address) -> Result<u64, PrecompileError> {
    let key = address_to_u256(ip_id);
    let value = sload(input, IPGRAPH_ADDRESS, key)?;
    // Safe: parent count fits in u64
    Ok(value.try_into().unwrap_or(u64::MAX))
}

/// Reads parent at index `i` for an IP.
///
/// Maps to story-geth: `slot = keccak256(ipId.Bytes()) + i`.
fn get_parent_at(
    input: &mut PrecompileInput<'_>,
    ip_id: Address,
    index: u64,
) -> Result<Address, PrecompileError> {
    let base = U256::from_be_bytes(keccak256(ip_id.as_slice()).0);
    let slot = base + U256::from(index);
    let value = sload(input, IPGRAPH_ADDRESS, slot)?;
    Ok(u256_to_address(value))
}

/// Converts an Address to a U256 (left-padded to 32 bytes, like story-geth `BytesToHash`).
fn address_to_u256(addr: Address) -> U256 {
    let mut bytes = [0u8; 32];
    bytes[12..].copy_from_slice(addr.as_slice());
    U256::from_be_bytes(bytes)
}

/// Converts a U256 back to an Address (take last 20 bytes).
fn u256_to_address(value: U256) -> Address {
    let bytes = value.to_be_bytes::<32>();
    Address::from_slice(&bytes[12..])
}

/// Encodes a bool as a 32-byte ABI value (0 or 1).
fn encode_bool(value: bool) -> alloy_primitives::Bytes {
    let mut out = [0u8; 32];
    if value {
        out[31] = 1;
    }
    alloy_primitives::Bytes::copy_from_slice(&out)
}

/// Encodes a U256 as 32 bytes.
fn encode_u256(value: U256) -> alloy_primitives::Bytes {
    alloy_primitives::Bytes::copy_from_slice(&value.to_be_bytes::<32>())
}

/// Encodes a dynamic address array as ABI output (offset + length + elements).
///
/// Matches story-geth pattern: `output = [offset=32][length][addr1][addr2]...`
fn encode_address_array(addrs: &[Address]) -> alloy_primitives::Bytes {
    let mut out = Vec::with_capacity(64 + addrs.len() * 32);
    // offset (always 32)
    out.extend_from_slice(&U256::from(32).to_be_bytes::<32>());
    // length
    out.extend_from_slice(&U256::from(addrs.len()).to_be_bytes::<32>());
    // elements (left-padded addresses)
    for addr in addrs {
        let mut padded = [0u8; 32];
        padded[12..].copy_from_slice(addr.as_slice());
        out.extend_from_slice(&padded);
    }
    alloy_primitives::Bytes::from(out)
}

/// Creates the IPGraph stateful precompile for registration via
/// [`PrecompilesMap::extend_precompiles`].
pub fn ipgraph_precompile() -> DynPrecompile {
    DynPrecompile::new_stateful(
        PrecompileId::Custom("IPGRAPH".into()),
        |mut input| {
            let data = input.data;

            if data.len() < 4 {
                return Err(PrecompileError::other("input too short"));
            }

            let selector: [u8; 4] = data[..4].try_into().unwrap();
            let gas_required = required_gas(selector, data);

            if input.gas < gas_required {
                return Err(PrecompileError::OutOfGas);
            }

            let result = dispatch(selector, &mut input)?;

            Ok(PrecompileOutput::new(gas_required, result))
        },
    )
}

/// Dispatches to the appropriate handler based on selector.
///
/// Mirrors story-geth `Run` (ipgraph.go:136-184). Ext variants call the same
/// underlying functions — they differ only in gas cost (handled above).
fn dispatch(
    selector: [u8; 4],
    input: &mut PrecompileInput<'_>,
) -> Result<alloy_primitives::Bytes, PrecompileError> {
    use IpGraph::*;

    match selector {
        addParentIpCall::SELECTOR => add_parent_ip(input),
        hasParentIpCall::SELECTOR | hasParentIpExtCall::SELECTOR => has_parent_ip(input),
        getParentIpsCall::SELECTOR | getParentIpsExtCall::SELECTOR => get_parent_ips(input),
        getParentIpsCountCall::SELECTOR | getParentIpsCountExtCall::SELECTOR => {
            get_parent_ips_count(input)
        }
        getAncestorIpsCall::SELECTOR | getAncestorIpsExtCall::SELECTOR => get_ancestor_ips(input),
        getAncestorIpsCountCall::SELECTOR | getAncestorIpsCountExtCall::SELECTOR => {
            get_ancestor_ips_count(input)
        }
        hasAncestorIpCall::SELECTOR | hasAncestorIpExtCall::SELECTOR => has_ancestor_ip(input),
        setRoyaltyCall::SELECTOR => set_royalty(input),
        getRoyaltyCall::SELECTOR | getRoyaltyExtCall::SELECTOR => get_royalty(input),
        getRoyaltyStackCall::SELECTOR | getRoyaltyStackExtCall::SELECTOR => {
            get_royalty_stack(input)
        }
        _ => Err(PrecompileError::other("unknown selector")),
    }
}

// --- Handler implementations ---
// Each mirrors the corresponding function in story-geth `core/vm/ipgraph.go`.
// Args are parsed from `input.data[4..]` (selector already stripped by dispatch).

/// ipgraph.go:203-238
fn add_parent_ip(
    input: &mut PrecompileInput<'_>,
) -> Result<alloy_primitives::Bytes, PrecompileError> {
    if !is_allowed(input)? {
        return Err(PrecompileError::other("caller not allowed to add parent IP"));
    }
    if !input.is_direct_call() {
        return Err(PrecompileError::other("addParentIp can only be called with CALL"));
    }

    let args = &input.data[4..];
    if args.len() < 96 {
        return Err(PrecompileError::other("input too short for addParentIp"));
    }

    let ip_id = Address::from_slice(&args[12..32]);
    let parent_count = U256::from_be_slice(&args[64..96]);
    let parent_count_u64: u64 = parent_count
        .try_into()
        .map_err(|_| PrecompileError::other("parent count overflow"))?;

    if args.len() != 96 + (parent_count_u64 as usize) * 32 {
        return Err(PrecompileError::other("input length does not match parent count"));
    }

    // Store each parent: slot = keccak256(ipId) + index
    for i in 0..parent_count_u64 {
        let offset = 96 + (i as usize) * 32;
        let parent_ip_id = Address::from_slice(&args[offset + 12..offset + 32]);

        let base = U256::from_be_bytes(keccak256(ip_id.as_slice()).0);
        let slot = base + U256::from(i);
        sstore(input, IPGRAPH_ADDRESS, slot, address_to_u256(parent_ip_id))?;
    }

    // Store parent count: key = ipId as U256
    sstore(input, IPGRAPH_ADDRESS, address_to_u256(ip_id), parent_count)?;

    Ok(alloy_primitives::Bytes::new())
}

/// ipgraph.go:241-272
fn has_parent_ip(
    input: &mut PrecompileInput<'_>,
) -> Result<alloy_primitives::Bytes, PrecompileError> {
    if !is_allowed(input)? {
        return Err(PrecompileError::other("caller not allowed to query hasParentIp"));
    }
    require_direct_call(input)?;

    let args = &input.data[4..];
    if args.len() != 64 {
        return Err(PrecompileError::other("input too short for hasParentIp"));
    }

    let ip_id = Address::from_slice(&args[12..32]);
    let parent_ip_id = Address::from_slice(&args[44..64]);

    let count = get_parent_count(input, ip_id)?;
    for i in 0..count {
        let stored = get_parent_at(input, ip_id, i)?;
        if stored == parent_ip_id {
            return Ok(encode_bool(true));
        }
    }
    Ok(encode_bool(false))
}

/// ipgraph.go:274-307
fn get_parent_ips(
    input: &mut PrecompileInput<'_>,
) -> Result<alloy_primitives::Bytes, PrecompileError> {
    if !is_allowed(input)? {
        return Err(PrecompileError::other("caller not allowed to query getParentIps"));
    }
    require_direct_call(input)?;

    let args = &input.data[4..];
    if args.len() != 32 {
        return Err(PrecompileError::other("input too short for getParentIps"));
    }

    let ip_id = Address::from_slice(&args[12..32]);
    let count = get_parent_count(input, ip_id)?;

    let mut parents = Vec::with_capacity(count as usize);
    for i in 0..count {
        parents.push(get_parent_at(input, ip_id, i)?);
    }
    Ok(encode_address_array(&parents))
}

/// ipgraph.go:309-332
fn get_parent_ips_count(
    input: &mut PrecompileInput<'_>,
) -> Result<alloy_primitives::Bytes, PrecompileError> {
    if !is_allowed(input)? {
        return Err(PrecompileError::other("caller not allowed to query parent Ips count"));
    }
    require_direct_call(input)?;

    let args = &input.data[4..];
    if args.len() != 32 {
        return Err(PrecompileError::other("input too short for getParentIpsCount"));
    }

    let ip_id = Address::from_slice(&args[12..32]);
    let count = get_parent_count(input, ip_id)?;
    Ok(encode_u256(U256::from(count)))
}

/// BFS to find all ancestors. Mirrors story-geth `findAncestors` (ipgraph.go:425-449).
///
/// Uses a stack (DFS in Go, but order doesn't matter — we collect into a set).
fn find_ancestors(
    input: &mut PrecompileInput<'_>,
    ip_id: Address,
) -> Result<BTreeSet<Address>, PrecompileError> {
    let mut ancestors = BTreeSet::new();
    let mut stack = vec![ip_id];

    while let Some(node) = stack.pop() {
        let count = get_parent_count(input, node)?;
        for i in 0..count {
            let parent = get_parent_at(input, node, i)?;
            if ancestors.insert(parent) {
                stack.push(parent);
            }
        }
    }
    Ok(ancestors)
}

/// ipgraph.go:334-372
fn get_ancestor_ips(
    input: &mut PrecompileInput<'_>,
) -> Result<alloy_primitives::Bytes, PrecompileError> {
    if !is_allowed(input)? {
        return Err(PrecompileError::other("caller not allowed to query getAncestorIps"));
    }
    require_direct_call(input)?;

    let args = &input.data[4..];
    if args.len() != 32 {
        return Err(PrecompileError::other("input too short for getAncestorIps"));
    }

    let ip_id = Address::from_slice(&args[12..32]);
    let ancestors = find_ancestors(input, ip_id)?;

    // BTreeSet is already sorted (matching story-geth's sort.Slice)
    let sorted: Vec<Address> = ancestors.into_iter().collect();
    Ok(encode_address_array(&sorted))
}

/// ipgraph.go:374-396
fn get_ancestor_ips_count(
    input: &mut PrecompileInput<'_>,
) -> Result<alloy_primitives::Bytes, PrecompileError> {
    if !is_allowed(input)? {
        return Err(PrecompileError::other("caller not allowed to query getAncestorIpsCount"));
    }
    require_direct_call(input)?;

    let args = &input.data[4..];
    if args.len() != 32 {
        return Err(PrecompileError::other("input too short for getAncestorIpsCount"));
    }

    let ip_id = Address::from_slice(&args[12..32]);
    let ancestors = find_ancestors(input, ip_id)?;
    Ok(encode_u256(U256::from(ancestors.len())))
}

/// ipgraph.go:398-423
fn has_ancestor_ip(
    input: &mut PrecompileInput<'_>,
) -> Result<alloy_primitives::Bytes, PrecompileError> {
    if !is_allowed(input)? {
        return Err(PrecompileError::other("caller not allowed to query hasAncestorIp"));
    }
    require_direct_call(input)?;

    let args = &input.data[4..];
    if args.len() != 64 {
        return Err(PrecompileError::other("input too short for hasAncestorIp"));
    }

    let ip_id = Address::from_slice(&args[12..32]);
    let ancestor_ip_id = Address::from_slice(&args[44..64]);
    let ancestors = find_ancestors(input, ip_id)?;
    Ok(encode_bool(ancestors.contains(&ancestor_ip_id)))
}

/// ipgraph.go:451-492
fn set_royalty(
    input: &mut PrecompileInput<'_>,
) -> Result<alloy_primitives::Bytes, PrecompileError> {
    if !is_allowed(input)? {
        return Err(PrecompileError::other("caller not allowed to set Royalty"));
    }
    if !input.is_direct_call() {
        return Err(PrecompileError::other("setRoyalty can only be called with CALL"));
    }

    let args = &input.data[4..];
    if args.len() != 128 {
        return Err(PrecompileError::other("input too short for setRoyalty"));
    }

    let ip_id = Address::from_slice(&args[12..32]);
    let parent_ip_id = Address::from_slice(&args[44..64]);
    let royalty_policy_kind = U256::from_be_slice(&args[64..96]);
    let royalty = U256::from_be_slice(&args[96..128]);

    if royalty > U256::from(MAX_UINT32) {
        return Err(PrecompileError::other("royalty value exceeds uint32 range"));
    }

    // Store royalty: slot = keccak256(ipId, parentIpId, royaltyPolicyKind)
    let slot = royalty_slot(ip_id, parent_ip_id, royalty_policy_kind);
    sstore(input, IPGRAPH_ADDRESS, slot, royalty)?;

    // For LAP policy (kind=0), update royalty stack
    if royalty_policy_kind == U256::ZERO {
        let parent_stack_slot = royalty_stack_slot(parent_ip_id, royalty_policy_kind);
        let parent_stack = sload(input, IPGRAPH_ADDRESS, parent_stack_slot)?;

        let ip_stack_slot = royalty_stack_slot(ip_id, royalty_policy_kind);
        let current_stack = sload(input, IPGRAPH_ADDRESS, ip_stack_slot)?;

        let new_stack = current_stack + parent_stack + royalty;
        sstore(input, IPGRAPH_ADDRESS, ip_stack_slot, new_stack)?;
    }

    Ok(alloy_primitives::Bytes::new())
}

/// Computes royalty storage slot: `keccak256(ipId ++ parentIpId ++ royaltyPolicyKind)`.
///
/// Maps to story-geth ipgraph.go:478.
fn royalty_slot(ip_id: Address, parent_ip_id: Address, kind: U256) -> U256 {
    let mut buf = Vec::with_capacity(84);
    buf.extend_from_slice(ip_id.as_slice());
    buf.extend_from_slice(parent_ip_id.as_slice());
    buf.extend_from_slice(&kind.to_be_bytes::<32>());
    U256::from_be_bytes(keccak256(&buf).0)
}

/// Computes royalty stack storage slot: `keccak256(ipId ++ royaltyPolicyKind ++ "royaltyStack")`.
///
/// Maps to story-geth ipgraph.go:482-484.
fn royalty_stack_slot(ip_id: Address, kind: U256) -> U256 {
    let mut buf = Vec::with_capacity(64);
    buf.extend_from_slice(ip_id.as_slice());
    buf.extend_from_slice(&kind.to_be_bytes::<32>());
    buf.extend_from_slice(b"royaltyStack");
    U256::from_be_bytes(keccak256(&buf).0)
}

/// ipgraph.go:494-569 (getRoyalty with LAP and LRP policies)
fn get_royalty(
    input: &mut PrecompileInput<'_>,
) -> Result<alloy_primitives::Bytes, PrecompileError> {
    if !is_allowed(input)? {
        return Err(PrecompileError::other("caller not allowed to query getRoyalty"));
    }
    require_direct_call(input)?;

    let args = &input.data[4..];
    if args.len() != 96 {
        return Err(PrecompileError::other("input too short for getRoyalty"));
    }

    let ip_id = Address::from_slice(&args[12..32]);
    let ancestor_ip_id = Address::from_slice(&args[44..64]);
    let royalty_policy_kind = U256::from_be_slice(&args[64..96]);

    if royalty_policy_kind == U256::ZERO {
        // LAP — ipgraph.go:516-543
        get_royalty_lap(input, ip_id, ancestor_ip_id)
    } else if royalty_policy_kind == U256::from(1) {
        // LRP — ipgraph.go:544-568
        get_royalty_lrp(input, ip_id, ancestor_ip_id)
    } else {
        Err(PrecompileError::other("unknown royalty policy kind"))
    }
}

/// ipgraph.go:670-714 (getRoyaltyStack with LAP and LRP policies)
fn get_royalty_stack(
    input: &mut PrecompileInput<'_>,
) -> Result<alloy_primitives::Bytes, PrecompileError> {
    if !is_allowed(input)? {
        return Err(PrecompileError::other("caller not allowed to query getRoyaltyStack"));
    }
    require_direct_call(input)?;

    let args = &input.data[4..];
    if args.len() != 64 {
        return Err(PrecompileError::other("input too short for getRoyaltyStack"));
    }

    let ip_id = Address::from_slice(&args[12..32]);
    let royalty_policy_kind = U256::from_be_slice(&args[32..64]);

    if royalty_policy_kind == U256::ZERO {
        // LAP — ipgraph.go:687-693
        get_royalty_stack_lap(input, ip_id)
    } else if royalty_policy_kind == U256::from(1) {
        // LRP — ipgraph.go:694-713
        get_royalty_stack_lrp(input, ip_id)
    } else {
        Err(PrecompileError::other("unknown royalty policy kind"))
    }
}

// --- Royalty calculation helpers ---

/// 100% in the integer format used by story-geth (ipgraph.go:26).
const HUNDRED_PERCENT: U256 = U256::from_limbs([100_000_000, 0, 0, 0]);

/// Topological sort — ipgraph.go:621-661.
///
/// Returns `(topo_order, all_parents)` where `topo_order` is a post-order DFS
/// traversal and `all_parents` maps each node to its parent list.
/// If `ancestor_ip_id` is not reachable, returns empty results.
fn topological_sort(
    input: &mut PrecompileInput<'_>,
    ip_id: Address,
    ancestor_ip_id: Address,
) -> Result<(Vec<Address>, HashMap<Address, Vec<Address>>), PrecompileError> {
    let mut all_parents: HashMap<Address, Vec<Address>> = HashMap::new();
    let mut visited: HashMap<Address, bool> = HashMap::new();
    let mut in_topo_order: HashMap<Address, bool> = HashMap::new();
    let mut topo_order: Vec<Address> = Vec::new();
    let mut stack = vec![ip_id];

    while let Some(current) = stack.pop() {
        if *visited.get(&current).unwrap_or(&false) {
            // Second visit — add to topo order
            if !*in_topo_order.get(&current).unwrap_or(&false) {
                topo_order.push(current);
                in_topo_order.insert(current, true);
            }
            continue;
        }
        visited.insert(current, true);
        // Push again for second visit (post-order)
        stack.push(current);

        let count = get_parent_count(input, current)?;
        for i in 0..count {
            let parent = get_parent_at(input, current, i)?;
            all_parents.entry(current).or_default().push(parent);

            if !*visited.get(&parent).unwrap_or(&false) {
                stack.push(parent);
            }
        }
    }

    if !*visited.get(&ancestor_ip_id).unwrap_or(&false) {
        return Ok((Vec::new(), HashMap::new()));
    }

    Ok((topo_order, all_parents))
}

/// LAP getRoyalty — ipgraph.go:530-575.
///
/// Topologically sorts ancestors, then accumulates royalties along the path.
/// Each node distributes its `pathCount` and weighted royalty to its parents.
fn get_royalty_lap(
    input: &mut PrecompileInput<'_>,
    ip_id: Address,
    ancestor_ip_id: Address,
) -> Result<alloy_primitives::Bytes, PrecompileError> {
    let mut royalty: HashMap<Address, U256> = HashMap::new();
    let mut path_count: HashMap<Address, U256> = HashMap::new();
    royalty.insert(ip_id, HUNDRED_PERCENT);
    path_count.insert(ip_id, U256::from(1));

    let (topo_order, all_parents) = topological_sort(input, ip_id, ancestor_ip_id)?;

    // Iterate in reverse topo order (ipgraph.go:542)
    for &node in topo_order.iter().rev() {
        if node == ancestor_ip_id {
            break;
        }

        let parents = match all_parents.get(&node) {
            Some(p) => p.clone(),
            None => continue,
        };

        let contribution = *path_count.get(&node).unwrap_or(&U256::ZERO);

        for &parent_ip_id in &parents {
            // Read royalty from storage: slot = keccak256(node, parent, LAP_KIND)
            let slot = royalty_slot(node, parent_ip_id, U256::ZERO);
            let parent_royalty = sload(input, IPGRAPH_ADDRESS, slot)?;

            // Update path count
            let existing = *path_count.get(&parent_ip_id).unwrap_or(&U256::ZERO);
            path_count.insert(parent_ip_id, existing + contribution);

            // Update royalty: existing + contribution * parentRoyalty
            let existing_royalty = *royalty.get(&parent_ip_id).unwrap_or(&U256::ZERO);
            royalty.insert(parent_ip_id, existing_royalty + contribution * parent_royalty);
        }
    }

    let result = *royalty.get(&ancestor_ip_id).unwrap_or(&U256::ZERO);
    if result > U256::from(MAX_UINT32) {
        return Err(PrecompileError::other("royalty value exceeds uint32 range"));
    }
    Ok(encode_u256(result))
}

/// LRP getRoyalty — ipgraph.go:577-619.
///
/// Like LAP but distributes proportionally: `contribution = currentRoyalty * parentRoyalty / 100%`.
fn get_royalty_lrp(
    input: &mut PrecompileInput<'_>,
    ip_id: Address,
    ancestor_ip_id: Address,
) -> Result<alloy_primitives::Bytes, PrecompileError> {
    let mut royalty: HashMap<Address, U256> = HashMap::new();
    royalty.insert(ip_id, HUNDRED_PERCENT);

    let (topo_order, all_parents) = topological_sort(input, ip_id, ancestor_ip_id)?;

    for &node in topo_order.iter().rev() {
        if node == ancestor_ip_id {
            break;
        }

        let current_royalty = *royalty.get(&node).unwrap_or(&U256::ZERO);
        if current_royalty.is_zero() {
            continue;
        }

        let parents = match all_parents.get(&node) {
            Some(p) => p.clone(),
            None => continue,
        };

        for &parent_ip_id in &parents {
            // Read royalty from storage: slot = keccak256(node, parent, LRP_KIND)
            let slot = royalty_slot(node, parent_ip_id, U256::from(1));
            let parent_royalty = sload(input, IPGRAPH_ADDRESS, slot)?;

            // contribution = currentRoyalty * parentRoyalty / hundredPercent
            let contribution = (current_royalty * parent_royalty) / HUNDRED_PERCENT;

            let existing = *royalty.get(&parent_ip_id).unwrap_or(&U256::ZERO);
            royalty.insert(parent_ip_id, existing + contribution);
        }
    }

    let result = *royalty.get(&ancestor_ip_id).unwrap_or(&U256::ZERO);
    if result > U256::from(MAX_UINT32) {
        return Err(PrecompileError::other("royalty value exceeds uint32 range"));
    }
    Ok(encode_u256(result))
}

/// LAP getRoyaltyStack — ipgraph.go:693-697.
///
/// Simply reads the stored royalty stack value.
fn get_royalty_stack_lap(
    input: &mut PrecompileInput<'_>,
    ip_id: Address,
) -> Result<alloy_primitives::Bytes, PrecompileError> {
    let slot = royalty_stack_slot(ip_id, U256::ZERO);
    let value = sload(input, IPGRAPH_ADDRESS, slot)?;
    Ok(encode_u256(value))
}

/// LRP getRoyaltyStack — ipgraph.go:699-713.
///
/// Sums royalties from all direct parents.
fn get_royalty_stack_lrp(
    input: &mut PrecompileInput<'_>,
    ip_id: Address,
) -> Result<alloy_primitives::Bytes, PrecompileError> {
    let count = get_parent_count(input, ip_id)?;
    let mut total = U256::ZERO;

    for i in 0..count {
        let parent = get_parent_at(input, ip_id, i)?;
        // slot = keccak256(ipId, parent, LRP_KIND)
        let slot = royalty_slot(ip_id, parent, U256::from(1));
        let royalty_value = sload(input, IPGRAPH_ADDRESS, slot)?;
        total += royalty_value;
    }

    Ok(encode_u256(total))
}
