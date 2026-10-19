// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Exchange8:Clearing
// SPDX-License-Identifier: Apache-2.0

//! Exchange8:Clearing drop-copy + regulatory stream + bust events + purge.
//!
//! - **FIX drop-copy engine**: per-client entitlement-filtered copy of
//!   order/trade lifecycle; fills-only vs all modes; resend window from the
//!   persisted drop buffer.
//! - **Regulatory stream**: the Clearing→Regulatory projection (order
//!   and trade reportable events, dedup by (type, id), per-day sequence).
//! - **Bust/amend as idempotent events**: bust dedup on (env, trade),
//!   step-up auth decision shape, and the compensation projection to
//!   positions/charges.
//! - **Purge jobs**: retention window for the drop buffer
//!   (`clearing.drop_message`) - the pure policy the Cron proxy-back runs.
//!   The gateway/md message stores' purge is the owning service's policy.
//!
//! Design: `design/sentin8-exchange8-clearing-technical-design.md` §4,
//! `design/sentin8-exchange8-clearing-depth-technical-design.md` (bust flow).

use std::collections::HashMap;

use exchange8::fixed::{Price, Qty};

// ---------------------------------------------------------------------------
// Drop copy
// ---------------------------------------------------------------------------

/// Drop-copy entitlement per client.
#[derive(Clone, Debug, PartialEq)]
pub struct DropEntitlement {
    pub client_comp_id: String,
    /// Fills-only (no order events) - the default entitlement mode.
    pub fills_only: bool,
    /// Instruments the client is entitled to see (empty = all).
    pub instruments: Vec<u32>,
}

/// One lifecycle event entering the drop engine.
#[derive(Clone, Debug, PartialEq)]
pub enum DropEvent {
    OrderNew {
        order_id: u64,
        instrument_id: u32,
        ts_us: u64,
    },
    OrderKilled {
        order_id: u64,
        instrument_id: u32,
        ts_us: u64,
    },
    Trade {
        trade_id: u64,
        instrument_id: u32,
        qty: Qty,
        price: Price,
        ts_us: u64,
    },
    Bust {
        trade_id: u64,
        instrument_id: u32,
        ts_us: u64,
    },
}

impl DropEvent {
    fn is_fill_class(&self) -> bool {
        matches!(self, DropEvent::Trade { .. } | DropEvent::Bust { .. })
    }

    fn instrument(&self) -> u32 {
        match self {
            DropEvent::OrderNew { instrument_id, .. }
            | DropEvent::OrderKilled { instrument_id, .. }
            | DropEvent::Trade { instrument_id, .. }
            | DropEvent::Bust { instrument_id, .. } => *instrument_id,
        }
    }
}

/// The drop-copy engine: filters per client + maintains the resend buffer.
pub struct DropEngine {
    /// Buffered events per client (the persisted `exchange8_drop_message`
    /// projection; here the in-memory window).
    buffers: HashMap<String, Vec<DropEvent>>,
    /// Global sequence of ingested events (resend cursor).
    seq: u64,
}

impl DropEngine {
    pub fn new() -> Self {
        DropEngine {
            buffers: HashMap::new(),
            seq: 0,
        }
    }

    /// Ingest one event: fan out to every entitled client's buffer.
    pub fn ingest(&mut self, e: DropEvent, entitled: &[DropEntitlement]) {
        self.seq += 1;
        for ent in entitled {
            if ent.fills_only && !e.is_fill_class() {
                continue;
            }
            if !ent.instruments.is_empty() && !ent.instruments.contains(&e.instrument()) {
                continue;
            }
            self.buffers
                .entry(ent.client_comp_id.clone())
                .or_default()
                .push(e.clone());
        }
    }

    /// Drain one client's copy (send path).
    pub fn drain(&mut self, client: &str) -> Vec<DropEvent> {
        self.buffers
            .get_mut(client)
            .map(std::mem::take)
            .unwrap_or_default()
    }

    /// Resend window: events with seq > `from_seq` for one client (session
    /// reconnect). Pure-core form: the buffer retains order; the shell
    /// persists (seq, event) pairs.
    pub fn resend_after(&self, client: &str, from_seq: u64) -> Vec<&DropEvent> {
        self.buffers
            .get(client)
            .map(|b| b.iter().skip(from_seq as usize).collect())
            .unwrap_or_default()
    }

    pub fn seq(&self) -> u64 {
        self.seq
    }
}

impl Default for DropEngine {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Regulatory stream projection
// ---------------------------------------------------------------------------

/// A reportable event projected to Regulatory.
#[derive(Clone, Debug, PartialEq)]
pub struct Reportable {
    /// `exchange8:clearing:order:reported` / `...:trade:reported`.
    pub kind: &'static str,
    /// Domain id (order_id / trade_id).
    pub id: u64,
    pub instrument_id: u32,
    pub ts_us: u64,
}

/// The projection: lifecycle events → reportables, deduped on (kind, id),
/// sequenced per day.
pub struct RegulatoryStream {
    emitted: std::collections::HashSet<(&'static str, u64)>,
    out: Vec<Reportable>,
    day_seq: u64,
    /// Day boundary (µs) - rollover resets the sequence.
    day_boundary_us: u64,
}

impl RegulatoryStream {
    pub fn new(day_boundary_us: u64) -> Self {
        RegulatoryStream {
            emitted: std::collections::HashSet::new(),
            out: Vec::new(),
            day_seq: 0,
            day_boundary_us,
        }
    }

    /// Project one lifecycle event; dedup on (kind, id) - at-least-once
    /// input, exactly-once output.
    pub fn project(&mut self, kind: &'static str, id: u64, instrument_id: u32, ts_us: u64) {
        if ts_us >= self.day_boundary_us {
            // New day: reset sequence (per-day wire seq).
            self.day_seq = 0;
            self.day_boundary_us += 24 * 3600 * 1_000_000;
        }
        if self.emitted.insert((kind, id)) {
            self.day_seq += 1;
            self.out.push(Reportable {
                kind,
                id,
                instrument_id,
                ts_us,
            });
        }
    }

    /// Drain the reportables (Regulatory consumes).
    pub fn drain(&mut self) -> Vec<Reportable> {
        std::mem::take(&mut self.out)
    }

    pub fn day_seq(&self) -> u64 {
        self.day_seq
    }
}

// ---------------------------------------------------------------------------
// Bust/amend idempotent events
// ---------------------------------------------------------------------------

/// Bust authorization decision (step-up auth shape).
#[derive(Clone, Debug, PartialEq)]
pub struct BustRequest {
    pub trade_id: u64,
    /// Requesting account (must be a party to the trade or ops).
    pub requester: String,
    pub reason: String,
}

/// Bust outcome.
#[derive(Clone, Debug, PartialEq)]
pub enum BustOutcome {
    /// Applied; the compensation projection.
    Applied {
        trade_id: u64,
        buyer_credit: i128,
        seller_debit: i128,
    },
    /// Already busted (idempotent) - the bust stands as originally applied.
    AlreadyBusted { trade_id: u64 },
    /// Unknown trade.
    Unknown,
    /// Not a party to the trade (step-up auth denied).
    NotAuthorized,
}

/// The bust ledger: idempotent per (trade_id), applied once.
pub struct BustLedger {
    busted: HashMap<u64, (i128, i128)>, // trade_id → (buyer_credit, seller_debit)
    /// Known trades (trade_id → (buyer, seller, notional)).
    trades: HashMap<u64, (String, String, i128)>,
}

impl BustLedger {
    pub fn new() -> Self {
        BustLedger {
            busted: HashMap::new(),
            trades: HashMap::new(),
        }
    }

    /// Register an executed trade (projector input).
    pub fn on_trade(&mut self, trade_id: u64, buyer: &str, seller: &str, notional: i128) {
        self.trades
            .insert(trade_id, (buyer.into(), seller.into(), notional));
    }

    /// Apply a bust (idempotent): reverses the notional to both parties.
    pub fn bust(&mut self, req: &BustRequest) -> BustOutcome {
        let Some((buyer, seller, notional)) = self.trades.get(&req.trade_id).cloned() else {
            return BustOutcome::Unknown;
        };
        // Step-up: requester must be one of the parties.
        if req.requester != buyer && req.requester != seller {
            return BustOutcome::NotAuthorized;
        }
        if self.busted.contains_key(&req.trade_id) {
            return BustOutcome::AlreadyBusted {
                trade_id: req.trade_id,
            };
        }
        let effect = (notional, -notional);
        self.busted.insert(req.trade_id, effect);
        BustOutcome::Applied {
            trade_id: req.trade_id,
            buyer_credit: effect.0,
            seller_debit: effect.1,
        }
    }

    pub fn is_busted(&self, trade_id: u64) -> bool {
        self.busted.contains_key(&trade_id)
    }
}

impl Default for BustLedger {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Purge policy
// ---------------------------------------------------------------------------

/// Retention window for one message family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retention {
    /// Family table (schema-qualified: `clearing.drop_message`).
    pub table: &'static str,
    /// Days to retain.
    pub days: u32,
}

/// Default retention policy for the drop buffer (7y regulatory keep).
/// The gateway (`gateway.fix_message`) and md (`md.md_message`) stores
/// declare their own retention under the owning service - never a
/// cross-schema DELETE (design doc §1).
pub const DEFAULT_RETENTION: &[Retention] = &[Retention {
    table: "clearing.drop_message",
    days: 2555,
}];

/// Compute the purge cutoff per family: rows older than cutoff are deleted.
/// Returns (table, cutoff_epoch_us) pairs for the Cron proxy-back DELETE.
pub fn purge_cutoffs(policy: &[Retention], now_us: u64) -> Vec<(&'static str, u64)> {
    policy
        .iter()
        .map(|r| {
            let cutoff = now_us.saturating_sub(r.days as u64 * 86_400_000_000);
            (r.table, cutoff)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ent(client: &str, fills_only: bool, instruments: Vec<u32>) -> DropEntitlement {
        DropEntitlement {
            client_comp_id: client.into(),
            fills_only,
            instruments,
        }
    }

    fn trade(id: u64, inst: u32) -> DropEvent {
        DropEvent::Trade {
            trade_id: id,
            instrument_id: inst,
            qty: Qty::units(100),
            price: Price::equity(10_000),
            ts_us: 1,
        }
    }

    #[test]
    fn drop_fills_only_filters_order_events() {
        let mut eng = DropEngine::new();
        let entitled = vec![ent("FILLSONLY", true, vec![]), ent("ALL", false, vec![])];
        eng.ingest(
            DropEvent::OrderNew {
                order_id: 1,
                instrument_id: 3,
                ts_us: 1,
            },
            &entitled,
        );
        eng.ingest(trade(9, 3), &entitled);
        // Fills-only client sees just the trade.
        let fills = eng.drain("FILLSONLY");
        assert_eq!(fills.len(), 1);
        assert!(matches!(fills[0], DropEvent::Trade { .. }));
        // All-mode client sees both.
        let all = eng.drain("ALL");
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn drop_instrument_entitlement_filters() {
        let mut eng = DropEngine::new();
        let entitled = vec![ent("EQ", false, vec![1, 2])];
        eng.ingest(trade(1, 1), &entitled);
        eng.ingest(trade(2, 9), &entitled); // not entitled instrument
        let out = eng.drain("EQ");
        assert_eq!(out.len(), 1);
        assert!(matches!(&out[0], DropEvent::Trade { trade_id, .. } if *trade_id == 1));
    }

    #[test]
    fn regulatory_stream_dedups_and_sequences() {
        let mut s = RegulatoryStream::new(86_400_000_000);
        s.project("exchange8:clearing:order:reported", 1, 3, 100);
        s.project("exchange8:clearing:order:reported", 1, 3, 200); // dedup
        s.project("exchange8:clearing:trade:reported", 9, 3, 300);
        let out = s.drain();
        assert_eq!(out.len(), 2);
        assert_eq!(s.day_seq(), 2);
        assert!(s.drain().is_empty());
    }

    #[test]
    fn bust_idempotent_and_authorized() {
        let mut led = BustLedger::new();
        led.on_trade(42, "BUYER", "SELLER", 1_000_000);
        // Party busts.
        let req = BustRequest {
            trade_id: 42,
            requester: "BUYER".into(),
            reason: "erroneous".into(),
        };
        match led.bust(&req) {
            BustOutcome::Applied {
                trade_id,
                buyer_credit,
                seller_debit,
            } => {
                assert_eq!(trade_id, 42);
                assert_eq!(buyer_credit, 1_000_000);
                assert_eq!(seller_debit, -1_000_000);
            }
            other => panic!("expected applied, got {other:?}"),
        }
        // Replay: idempotent (already busted).
        assert!(matches!(led.bust(&req), BustOutcome::AlreadyBusted { .. }));
        // Non-party denied.
        let outsider = BustRequest {
            trade_id: 42,
            requester: "OTHER".into(),
            reason: "x".into(),
        };
        assert!(matches!(led.bust(&outsider), BustOutcome::NotAuthorized));
        // Unknown trade.
        let unk = BustRequest {
            trade_id: 99,
            requester: "BUYER".into(),
            reason: "x".into(),
        };
        assert!(matches!(led.bust(&unk), BustOutcome::Unknown));
        assert!(led.is_busted(42));
    }

    #[test]
    fn purge_cutoffs_compute() {
        // Arbitrary reference epoch µs.
        let now = 1_767_000_000u64 * 1_000_000;
        let cuts = purge_cutoffs(DEFAULT_RETENTION, now);
        let drop = cuts
            .iter()
            .find(|(t, _)| *t == "clearing.drop_message")
            .unwrap();
        // 7y (2555 days) before `now` - the drop buffer's regulatory keep.
        assert_eq!(drop.1, now - 2555u64 * 86_400_000_000);
        // Saturating: early epoch → 0.
        let early = purge_cutoffs(DEFAULT_RETENTION, 1000);
        assert!(early.iter().all(|(_, c)| *c == 0));
    }
}
