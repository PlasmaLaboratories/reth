//! Walks a [`CallTraceArena`] and emits one [`Transfer`] row per native-value
//! movement, plus an optional synthetic fee row per transaction.

use crate::payload::{HexQuantity, Transfer, TransferName};
use alloy_primitives::{Address, B256, U256};
use revm_inspectors::tracing::{types::CallKind, CallTraceArena};

/// Per-block synthetic counter used as the `logIndex` for emitted rows. Native
/// value movements have no real log index, so we keep a deterministic order
/// across the entire block.
#[derive(Debug, Default)]
pub struct LogIndexCounter(u64);

impl LogIndexCounter {
    pub const fn next(&mut self) -> u64 {
        let v = self.0;
        self.0 += 1;
        v
    }
}

/// Inputs needed to build the constant fields of every emitted row for a
/// transaction.
pub struct TxRowContext {
    pub native_address: Address,
    pub block_hash: B256,
    pub block_number: u64,
    pub transaction_hash: B256,
    pub tx_signer: Address,
}

/// Push rows for every value-moving frame in `arena`. Skips delegate/static
/// frames and zero-value frames.
pub fn extract_value_transfers(
    arena: &CallTraceArena,
    ctx: &TxRowContext,
    counter: &mut LogIndexCounter,
    out: &mut Vec<Transfer>,
) {
    for node in arena.nodes() {
        let trace = &node.trace;
        if trace.value.is_zero() {
            continue;
        }
        // CALLCODE/DELEGATECALL/STATICCALL/AUTHCALL do not move native value
        // (DELEGATECALL inherits its parent's value field without transferring).
        let (from, to) = match trace.kind {
            CallKind::Call | CallKind::Create | CallKind::Create2 => (trace.caller, trace.address),
            _ => continue,
        };
        out.push(Transfer {
            address: ctx.native_address,
            block_hash: ctx.block_hash,
            block_number: HexQuantity(ctx.block_number),
            from,
            log_index: HexQuantity(counter.next()),
            name: TransferName::Xpl,
            to,
            transaction_hash: ctx.transaction_hash,
            value: trace.value.into(),
        });
    }
}

/// Append a synthetic gas-fee row using `gas_used * effective_gas_price` if
/// non-zero.
pub fn push_fee_row(
    ctx: &TxRowContext,
    gas_used: u64,
    effective_gas_price: u128,
    counter: &mut LogIndexCounter,
    out: &mut Vec<Transfer>,
) {
    let fee = U256::from(gas_used).saturating_mul(U256::from(effective_gas_price));
    if fee.is_zero() {
        return;
    }
    out.push(Transfer {
        address: ctx.native_address,
        block_hash: ctx.block_hash,
        block_number: HexQuantity(ctx.block_number),
        from: ctx.tx_signer,
        log_index: HexQuantity(counter.next()),
        name: TransferName::Fee,
        to: ctx.native_address,
        transaction_hash: ctx.transaction_hash,
        value: fee.into(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, b256};
    use revm_inspectors::tracing::types::{CallTrace, CallTraceNode};

    fn node(kind: CallKind, caller: Address, address: Address, value: U256) -> CallTraceNode {
        CallTraceNode {
            trace: CallTrace { kind, caller, address, value, ..Default::default() },
            ..Default::default()
        }
    }

    fn ctx() -> TxRowContext {
        TxRowContext {
            native_address: Address::ZERO,
            block_hash: b256!("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            block_number: 100,
            transaction_hash: b256!(
                "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            ),
            tx_signer: address!("0x1111111111111111111111111111111111111111"),
        }
    }

    fn arena_with(nodes: Vec<CallTraceNode>) -> CallTraceArena {
        let mut arena = CallTraceArena::default();
        let slot = arena.nodes_mut();
        *slot = nodes;
        arena
    }

    #[test]
    fn direct_call_with_value_emits_row() {
        let n = node(
            CallKind::Call,
            address!("0x2222222222222222222222222222222222222222"),
            address!("0x3333333333333333333333333333333333333333"),
            U256::from(42),
        );
        let arena = arena_with(vec![n]);
        let mut out = Vec::new();
        let mut counter = LogIndexCounter::default();
        extract_value_transfers(&arena, &ctx(), &mut counter, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].value.0, U256::from(42));
        assert_eq!(out[0].from, address!("0x2222222222222222222222222222222222222222"));
        assert_eq!(out[0].to, address!("0x3333333333333333333333333333333333333333"));
        assert_eq!(out[0].name, TransferName::Xpl);
        assert_eq!(out[0].log_index, HexQuantity(0));
    }

    #[test]
    fn zero_value_call_emits_nothing() {
        let n = node(CallKind::Call, Address::repeat_byte(1), Address::repeat_byte(2), U256::ZERO);
        let arena = arena_with(vec![n]);
        let mut out = Vec::new();
        let mut counter = LogIndexCounter::default();
        extract_value_transfers(&arena, &ctx(), &mut counter, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn delegate_static_callcode_authcall_skipped() {
        let arena = arena_with(vec![
            node(
                CallKind::DelegateCall,
                Address::repeat_byte(1),
                Address::repeat_byte(2),
                U256::from(1),
            ),
            node(
                CallKind::StaticCall,
                Address::repeat_byte(1),
                Address::repeat_byte(2),
                U256::from(1),
            ),
            node(
                CallKind::CallCode,
                Address::repeat_byte(1),
                Address::repeat_byte(2),
                U256::from(1),
            ),
            node(
                CallKind::AuthCall,
                Address::repeat_byte(1),
                Address::repeat_byte(2),
                U256::from(1),
            ),
        ]);
        let mut out = Vec::new();
        let mut counter = LogIndexCounter::default();
        extract_value_transfers(&arena, &ctx(), &mut counter, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn create_with_endowment_emits_row_to_created_address() {
        let n = node(
            CallKind::Create,
            address!("0x4444444444444444444444444444444444444444"),
            address!("0x5555555555555555555555555555555555555555"),
            U256::from(99),
        );
        let arena = arena_with(vec![n]);
        let mut out = Vec::new();
        let mut counter = LogIndexCounter::default();
        extract_value_transfers(&arena, &ctx(), &mut counter, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].to, address!("0x5555555555555555555555555555555555555555"));
    }

    #[test]
    fn aa_style_internal_call_emits_wallet_as_from() {
        // Bundler → EntryPoint → SmartAccount → recipient (the inner CALL has value).
        let bundler = address!("0x9999999999999999999999999999999999999999");
        let entry = address!("0x1111111111111111111111111111111111111111");
        let wallet = address!("0x2222222222222222222222222222222222222222");
        let recipient = address!("0x3333333333333333333333333333333333333333");

        let arena = arena_with(vec![
            node(CallKind::Call, bundler, entry, U256::ZERO),
            node(CallKind::Call, entry, wallet, U256::ZERO),
            node(CallKind::Call, wallet, recipient, U256::from(10_u64.pow(18))),
        ]);

        let mut out = Vec::new();
        let mut counter = LogIndexCounter::default();
        extract_value_transfers(&arena, &ctx(), &mut counter, &mut out);

        assert_eq!(out.len(), 1, "only the value-moving frame emits a row");
        assert_eq!(out[0].from, wallet, "the wallet is the from, not the bundler");
        assert_eq!(out[0].to, recipient);
    }

    #[test]
    fn fee_row_uses_gas_used_times_effective_price() {
        let mut out = Vec::new();
        let mut counter = LogIndexCounter::default();
        push_fee_row(&ctx(), 21_000, 2_000_000_000, &mut counter, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, TransferName::Fee);
        assert_eq!(out[0].value.0, U256::from(42_000_000_000_000_u128));
        assert_eq!(out[0].from, address!("0x1111111111111111111111111111111111111111"));
        assert_eq!(out[0].to, Address::ZERO);
    }

    #[test]
    fn zero_fee_emits_no_row() {
        let mut out = Vec::new();
        let mut counter = LogIndexCounter::default();
        push_fee_row(&ctx(), 0, 1_000, &mut counter, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn log_index_is_monotonic_across_calls_and_fee() {
        let arena = arena_with(vec![
            node(CallKind::Call, Address::repeat_byte(1), Address::repeat_byte(2), U256::from(1)),
            node(CallKind::Call, Address::repeat_byte(3), Address::repeat_byte(4), U256::from(2)),
        ]);
        let mut out = Vec::new();
        let mut counter = LogIndexCounter::default();
        extract_value_transfers(&arena, &ctx(), &mut counter, &mut out);
        push_fee_row(&ctx(), 21_000, 1_000_000_000, &mut counter, &mut out);
        assert_eq!(out[0].log_index, HexQuantity(0));
        assert_eq!(out[1].log_index, HexQuantity(1));
        assert_eq!(out[2].log_index, HexQuantity(2));
    }
}
