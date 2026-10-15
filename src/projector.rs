// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Exchange8:Clearing
// SPDX-License-Identifier: Apache-2.0

// Ex8:Clearing event projectors — the deterministic order/trade ledger.
//
// Event-sourced: ME/SOR lifecycle events in → ledger rows out. The projectors
// are pure functions of the event stream; the DB write layer and drop-copy
// fan-out live in the service shell.
//
// Design: `design/sentin8-exchange8-clearing-technical-design.md` §4.1–4.2.
// Truth: `design/behavior-catalog/messages.json` (update type codes).

use exchange8::contracts::enums::{Liquidity, Side};
use exchange8::contracts::events::{OrderBooked, OrderExecuted, OrderKilled, OrderModified};
use exchange8::fixed::{Price, Qty};

// ---------------------------------------------------------------------------
// Ledger rows (pure data; the shell binds these to `exchange8_*` tables)
// ---------------------------------------------------------------------------

/// One row per order lifecycle event (the platform spec codes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateType {
    /// `A` — new, pending.
    NewPending,
    /// `B` — booked.
    Booked,
    /// `P` — accepted.
    Accepted,
    /// `S` — posted.
    Posted,
    /// `C` — cancel pending.
    CancelPending,
    /// `X` — cancelled.
    Cancelled,
    /// `T` — traded.
    Traded,
    /// `R` — replace pending.
    ReplacePending,
    /// `M` — replace done.
    ReplaceDone,
    /// `I` — repriced.
    Repriced,
}

impl UpdateType {
    /// one-letter type code (order-update journal  wire parity).
    pub fn code(self) -> char {
        match self {
            UpdateType::NewPending => 'A',
            UpdateType::Booked => 'B',
            UpdateType::Accepted => 'P',
            UpdateType::Posted => 'S',
            UpdateType::CancelPending => 'C',
            UpdateType::Cancelled => 'X',
            UpdateType::Traded => 'T',
            UpdateType::ReplacePending => 'R',
            UpdateType::ReplaceDone => 'M',
            UpdateType::Repriced => 'I',
        }
    }
}

/// `exchange8_order_update` row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderUpdateRow {
    pub order_id: u64,
    pub instrument_id: u32,
    pub connection_id: u16,
    pub update_type: UpdateType,
    pub qty: Option<Qty>,
    pub price: Option<Price>,
    pub ts_us: u64,
}

/// Per-side trade record — the one-row-per-side `TRADE` shape is
/// preserved for clearing compatibility (both side-events share `trade_id`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TradeSideRow {
    pub trade_id: u64,
    pub order_id: u64,
    pub instrument_id: u32,
    pub connection_id: u16,
    pub side: Side,
    pub liquidity: Liquidity,
    pub price: Price,
    pub qty: Qty,
    pub counterparty_order_id: u64,
    /// Passive customer id when the counterparty is not anonymous.
    pub counterparty_id: Option<u16>,
    pub ts_us: u64,
}

/// A busted trade marker (`exchange8_bust_trade` projection).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BustRow {
    pub trade_id: u64,
    pub reason: BustReason,
}

/// bust reasons (`BUST_TRADE.Reason`). `ObviousError` and
/// `Regulatory` are constructed by the shell's admin bust command handler
/// (`POST /v1/admin/trades/bust`) — the projector test covers the projection
/// path only, so they carry an explicit allow here.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BustReason {
    /// Erroneous trade.
    Erroneous,
    /// Obvious error (clearly erroneous execution rule).
    ObviousError,
    /// Regulatory direction.
    Regulatory,
}

// ---------------------------------------------------------------------------
// Projectors
// ---------------------------------------------------------------------------

/// Order projector: lifecycle events → order-update rows.
///
/// The `OrderRejected` input maps to the the platform spec-row  terminal `X` pair
/// when the order is rejected at add time; kills map to `X`; booked to `B`;
/// modified to the `R`+`M` pair; repriced (booked-with-new-price) to `I`.
pub struct OrderProjector {
    updates: Vec<OrderUpdateRow>,
}

impl OrderProjector {
    pub fn new() -> Self {
        OrderProjector {
            updates: Vec::new(),
        }
    }

    fn push(
        &mut self,
        order_id: u64,
        instrument_id: u32,
        connection_id: u16,
        t: UpdateType,
        qty: Option<Qty>,
        price: Option<Price>,
        ts_us: u64,
    ) {
        self.updates.push(OrderUpdateRow {
            order_id,
            instrument_id,
            connection_id,
            update_type: t,
            qty,
            price,
            ts_us,
        });
    }

    /// Order accepted into the book (`ME_MSG_ORDER_BOOKED`).
    pub fn on_booked(&mut self, e: &OrderBooked, ts_us: u64) {
        let qty = if e.volume.raw > 0 {
            Some(e.volume)
        } else {
            None
        };
        // price 0 = booked at the original price (no reprice).
        let price = if e.price.raw > 0 { Some(e.price) } else { None };
        let t = if e.price.raw > 0 {
            UpdateType::Repriced
        } else {
            UpdateType::Booked
        };
        self.push(
            e.order_id,
            e.instrument_id,
            e.connection_id,
            t,
            qty,
            price,
            ts_us,
        );
    }

    /// Order cancelled/killed (`ME_MSG_ORDER_KILLED`).
    pub fn on_killed(&mut self, e: &OrderKilled, ts_us: u64) {
        self.push(
            e.order_id,
            e.instrument_id,
            e.connection_id,
            UpdateType::Cancelled,
            None,
            None,
            ts_us,
        );
    }

    /// Order replace (`ME_MSG_ORDER_MODIFIED`) → the platform spec then `M` rows.
    pub fn on_modified(&mut self, e: &OrderModified, ts_us: u64) {
        // Replace-pending marker on the old order id, then replace-done on
        // the new id (stores both in the journal).
        self.push(
            e.order_id,
            e.instrument_id,
            e.connection_id,
            UpdateType::ReplacePending,
            Some(e.new_volume),
            Some(e.new_price),
            ts_us,
        );
        self.push(
            e.new_order_id,
            e.instrument_id,
            e.connection_id,
            UpdateType::ReplaceDone,
            Some(e.new_volume),
            Some(e.new_price),
            ts_us,
        );
    }

    /// Order rejected at entry (`ME_MSG_ORDER_REJECTED`) → `A` then `X`.
    pub fn on_rejected(
        &mut self,
        order_id: u64,
        instrument_id: u32,
        connection_id: u16,
        ts_us: u64,
    ) {
        self.push(
            order_id,
            instrument_id,
            connection_id,
            UpdateType::NewPending,
            None,
            None,
            ts_us,
        );
        self.push(
            order_id,
            instrument_id,
            connection_id,
            UpdateType::Cancelled,
            None,
            None,
            ts_us,
        );
    }

    /// Journal of all projected rows (in event order).
    pub fn rows(&self) -> &[OrderUpdateRow] {
        &self.updates
    }
}

impl Default for OrderProjector {
    fn default() -> Self {
        Self::new()
    }
}

/// Trade projector: ME `OrderExecuted` side-events → per-side trade rows.
///
/// **Invariant (design §4.1)**: every ME execution emits TWO side-events
/// sharing one `trade_id` — `Liquidity::Removed` (taker) and
/// `Liquidity::Added` (maker). The projector must see exactly one of each
/// per trade id; a duplicate or a missing side is a projection gap alert.
pub struct TradeProjector {
    sides: Vec<TradeSideRow>,
    /// trade_ids already complete (both sides projected).
    complete: std::collections::HashSet<u64>,
    /// Orphan side-events: seen once, counterpart never arrived yet.
    pending: std::collections::HashMap<u64, TradeSideRow>,
    /// trade_ids with duplicate sides (projection bug / ME duplicate emit).
    pub anomalies: Vec<(u64, &'static str)>,
}

impl TradeProjector {
    pub fn new() -> Self {
        TradeProjector {
            sides: Vec::new(),
            complete: std::collections::HashSet::new(),
            pending: std::collections::HashMap::new(),
            anomalies: Vec::new(),
        }
    }

    /// Project one side of an execution. When both sides have arrived the
    /// logical trade is complete (rows are kept in arrival order).
    pub fn on_executed(&mut self, e: &OrderExecuted) {
        if self.complete.contains(&e.trade_id) {
            self.anomalies.push((e.trade_id, "side after completion"));
            return;
        }
        let row = TradeSideRow {
            trade_id: e.trade_id,
            order_id: e.order_id,
            instrument_id: e.instrument_id,
            connection_id: e.connection_id,
            side: e.side,
            liquidity: e.liquidity,
            price: e.price,
            qty: e.qty,
            counterparty_order_id: e.cp_order_id,
            counterparty_id: e.counterparty_id,
            ts_us: e.ts_us,
        };
        match self.pending.remove(&e.trade_id) {
            Some(first) => {
                // The counterpart arrived: validate the pair.
                if first.liquidity == e.liquidity {
                    self.anomalies
                        .push((e.trade_id, "both sides same liquidity"));
                }
                if first.price.raw != e.price.raw || first.qty.raw != e.qty.raw {
                    self.anomalies.push((e.trade_id, "side price/qty mismatch"));
                }
                self.sides.push(first);
                self.sides.push(row);
                self.complete.insert(e.trade_id);
            }
            None => {
                self.pending.insert(e.trade_id, row);
            }
        }
    }

    /// All projected side rows in pair-completion order.
    pub fn rows(&self) -> &[TradeSideRow] {
        &self.sides
    }

    /// Side rows still awaiting their counterpart (gap alert source).
    pub fn pending_rows(&self) -> impl Iterator<Item = &TradeSideRow> {
        self.pending.values()
    }

    /// Bust a completed trade: marks the trade id busted for the shell.
    pub fn bust(&mut self, trade_id: u64, reason: BustReason) -> BustRow {
        // Idempotent: busting an already-busted trade returns the marker
        // again (the shell dedupes on idempotency key = trade_id).
        BustRow { trade_id, reason }
    }
}

impl Default for TradeProjector {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Drop-copy fan-out filter (§4.2)
// ---------------------------------------------------------------------------

/// Drop-session entitlement configuration (one per `exchange8_drop_connection`).
#[derive(Clone, Debug, Default)]
pub struct DropConfig {
    /// Identifies the session row in the shell's registry (not used by the
    /// pure filter itself).
    #[allow(dead_code)]
    pub connection_id: u16,
    /// `all` or `fills` mode.
    pub fills_only: bool,
    /// Customer entitlement junction (empty = all customers).
    pub customers: std::collections::HashSet<u16>,
    /// Trader entitlement junction.
    pub traders: std::collections::HashSet<u16>,
    /// Book-token allow-list (empty = all books).
    pub book_tokens: std::collections::HashSet<u16>,
}

/// Whether a drop connection should receive this execution, given the
/// per-side trade row and the order's owning customer/trader.
///
/// `e` is the trade side row (kept for the shell's routing-connection dedup
/// skip; the pure filter itself keys off the junction sets).
pub fn drop_matches(
    cfg: &DropConfig,
    e: &TradeSideRow,
    customer: Option<u16>,
    trader: Option<u16>,
    book_id: u16,
) -> bool {
    let _ = e;
    // Entitlement junctions: a non-empty set filters; empty allows all.
    if !cfg.customers.is_empty() && customer.is_none_or(|c| !cfg.customers.contains(&c)) {
        return false;
    }
    if !cfg.traders.is_empty() && trader.is_none_or(|t| !cfg.traders.contains(&t)) {
        return false;
    }
    if !cfg.book_tokens.is_empty() && !cfg.book_tokens.contains(&book_id) {
        return false;
    }
    if cfg.fills_only {
        // fills-only drops receive exactly the execution events (every trade
        // side row is an execution) — no order-state events.
        return true;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use exchange8::contracts::enums::LotType;

    fn booked(order_id: u64, price_raw: i128, volume: u64) -> OrderBooked {
        OrderBooked {
            price: if price_raw > 0 {
                Price::equity(price_raw)
            } else {
                Price::equity(0)
            },
            order_id,
            sub_order_id: 0,
            instrument_id: 1,
            volume: Qty::units(volume),
            connection_id: 7,
        }
    }

    #[test]
    fn update_type_codes_round_trip() {
        assert_eq!(UpdateType::NewPending.code(), 'A');
        assert_eq!(UpdateType::Booked.code(), 'B');
        assert_eq!(UpdateType::Accepted.code(), 'P');
        assert_eq!(UpdateType::Posted.code(), 'S');
        assert_eq!(UpdateType::CancelPending.code(), 'C');
        assert_eq!(UpdateType::Cancelled.code(), 'X');
        assert_eq!(UpdateType::Traded.code(), 'T');
        assert_eq!(UpdateType::ReplacePending.code(), 'R');
        assert_eq!(UpdateType::ReplaceDone.code(), 'M');
        assert_eq!(UpdateType::Repriced.code(), 'I');
    }

    #[test]
    fn order_lifecycle_projects_journal() {
        let mut p = OrderProjector::new();
        p.on_booked(&booked(10, 0, 100), 1_000);
        p.on_killed(
            &OrderKilled {
                order_id: 10,
                sub_order_id: 0,
                instrument_id: 1,
                connection_id: 7,
                reason: exchange8::contracts::enums::KillReason::ClientRequest,
            },
            2_000,
        );
        let rows = p.rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].update_type.code(), 'B');
        assert_eq!(rows[0].qty, Some(Qty::units(100)));
        assert_eq!(rows[0].price, None);
        assert_eq!(rows[1].update_type.code(), 'X');
        assert_eq!(rows[1].order_id, 10);
    }

    #[test]
    fn booked_with_price_is_repriced() {
        let mut p = OrderProjector::new();
        p.on_booked(&booked(11, 100_050, 100), 1_000);
        let rows = p.rows();
        assert_eq!(rows[0].update_type.code(), 'I');
        assert_eq!(rows[0].price, Some(Price::equity(100_050)));
    }

    #[test]
    fn modified_projects_r_then_m() {
        let mut p = OrderProjector::new();
        p.on_modified(
            &OrderModified {
                new_price: Price::equity(100_100),
                order_id: 20,
                sub_order_id: 0,
                instrument_id: 1,
                new_volume: Qty::units(200),
                new_order_id: 21,
                connection_id: 7,
                side: Side::Bid,
                new_max_floor: Qty::units(0),
            },
            3_000,
        );
        let rows = p.rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].update_type.code(), 'R');
        assert_eq!(rows[0].order_id, 20);
        assert_eq!(rows[1].update_type.code(), 'M');
        assert_eq!(rows[1].order_id, 21);
    }

    #[test]
    fn rejected_projects_a_then_x() {
        let mut p = OrderProjector::new();
        p.on_rejected(30, 1, 7, 4_000);
        let rows = p.rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].update_type.code(), 'A');
        assert_eq!(rows[1].update_type.code(), 'X');
    }

    fn executed(trade_id: u64, order_id: u64, liq: Liquidity, side: Side) -> OrderExecuted {
        OrderExecuted {
            trade_id,
            ts_us: 9_000,
            price: Price::equity(100_100),
            order_id,
            sub_order_id: 0,
            cp_order_id: if side == Side::Bid { 99 } else { order_id },
            cp_sub_order_id: 0,
            instrument_id: 1,
            qty: Qty::units(100),
            remaining_volume: Qty::units(0),
            connection_id: 7,
            counterparty_id: Some(42),
            book_id: 1,
            liquidity: liq,
            side,
            lot_type: LotType::Board,
            last_mkt: *b"XTSX",
            principal_trade: false,
            wash_trade: false,
            midpoint_charge: false,
        }
    }

    #[test]
    fn executed_pairs_into_trade() {
        let mut p = TradeProjector::new();
        // Taker (Removed, buy) arrives first.
        p.on_executed(&executed(500, 10, Liquidity::Removed, Side::Bid));
        assert_eq!(p.rows().len(), 0);
        assert_eq!(p.pending_rows().count(), 1);
        // Maker (Added, sell) completes the pair.
        p.on_executed(&executed(500, 20, Liquidity::Added, Side::Ask));
        assert_eq!(p.rows().len(), 2);
        assert_eq!(p.pending_rows().count(), 0);
        let rows = p.rows();
        assert_eq!(rows[0].liquidity, Liquidity::Removed);
        assert_eq!(rows[1].liquidity, Liquidity::Added);
        assert_eq!(rows[0].trade_id, 500);
        assert_eq!(rows[1].trade_id, 500);
    }

    #[test]
    fn duplicate_side_is_anomaly() {
        let mut p = TradeProjector::new();
        p.on_executed(&executed(501, 10, Liquidity::Removed, Side::Bid));
        p.on_executed(&executed(501, 20, Liquidity::Added, Side::Ask));
        // A third side-event for the completed trade is an anomaly.
        p.on_executed(&executed(501, 30, Liquidity::Removed, Side::Bid));
        assert_eq!(p.anomalies.len(), 1);
        assert_eq!(p.anomalies[0].0, 501);
    }

    #[test]
    fn both_sides_same_liquidity_is_anomaly() {
        let mut p = TradeProjector::new();
        p.on_executed(&executed(502, 10, Liquidity::Removed, Side::Bid));
        // Malformed ME output: both sides claim taker.
        p.on_executed(&executed(502, 20, Liquidity::Removed, Side::Ask));
        assert!(p.anomalies.iter().any(|a| a.0 == 502));
        // The pair still projects (data preserved for repair).
        assert_eq!(p.rows().len(), 2);
    }

    #[test]
    fn mismatched_pair_is_anomaly() {
        let mut p = TradeProjector::new();
        let mut a = executed(503, 10, Liquidity::Removed, Side::Bid);
        a.qty = Qty::units(100);
        let mut b = executed(503, 20, Liquidity::Added, Side::Ask);
        b.qty = Qty::units(300); // ME bug: sides disagree
        p.on_executed(&a);
        p.on_executed(&b);
        assert!(p.anomalies.iter().any(|(id, _)| *id == 503));
    }

    #[test]
    fn bust_is_idempotent_data() {
        let mut p = TradeProjector::new();
        p.on_executed(&executed(600, 10, Liquidity::Removed, Side::Bid));
        p.on_executed(&executed(600, 20, Liquidity::Added, Side::Ask));
        let bust = p.bust(600, BustReason::Erroneous);
        assert_eq!(bust.trade_id, 600);
        let again = p.bust(600, BustReason::Erroneous);
        assert_eq!(again.trade_id, 600);
    }

    #[test]
    fn drop_fan_out_filters() {
        let row = TradeSideRow {
            trade_id: 1,
            order_id: 10,
            instrument_id: 1,
            connection_id: 7,
            side: Side::Bid,
            liquidity: Liquidity::Removed,
            price: Price::equity(100_100),
            qty: Qty::units(100),
            counterparty_order_id: 20,
            counterparty_id: None,
            ts_us: 9_000,
        };
        // No entitlements → everything passes.
        let all = DropConfig::default();
        assert!(drop_matches(&all, &row, Some(5), Some(6), 1));
        // Customer filter allows the entitled customer.
        let mut by_customer = DropConfig::default();
        by_customer.customers.insert(5);
        assert!(drop_matches(&by_customer, &row, Some(5), Some(6), 1));
        // A different customer is suppressed.
        assert!(!drop_matches(&by_customer, &row, Some(9), Some(6), 1));
        // Unknown customer (anonymous) suppressed when a filter exists.
        assert!(!drop_matches(&by_customer, &row, None, Some(6), 1));
        // Trader filter.
        let mut by_trader = DropConfig::default();
        by_trader.traders.insert(6);
        assert!(drop_matches(&by_trader, &row, Some(5), Some(6), 1));
        assert!(!drop_matches(&by_trader, &row, Some(5), Some(9), 1));
        // Book-token allow-list.
        let mut by_book = DropConfig::default();
        by_book.book_tokens.insert(1);
        assert!(drop_matches(&by_book, &row, Some(5), Some(6), 1));
        assert!(!drop_matches(&by_book, &row, Some(5), Some(6), 2));
        // fills-only mode passes every trade side row.
        let mut fills = DropConfig::default();
        fills.fills_only = true;
        assert!(drop_matches(&fills, &row, Some(5), Some(6), 1));
    }
}
