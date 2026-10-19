// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Exchange8:Clearing
// SPDX-License-Identifier: Apache-2.0

//! Exchange8:Clearing securities lending - short-sale locate/borrow ledger.
//!
//! Implements clearing-depth design §3: before a short sale, the broker must
//! locate borrowable shares. This module owns the locate ledger - locate
//! requests, approvals, consumption at execution, and expiry - plus the
//! borrow inventory that backs it (securities-lending feed / prime-broker
//! API projection).
//!
//! Reg SHO semantics: a locate is instrument-scoped, qty-capped, and
//! time-boxed; consuming one authorizes the corresponding short-sale qty.
//! The Risk §5.1 shortable flag is the static gate; the locate is the
//! operational prerequisite the flag implies.
//!
//! Pure core; the shell wires persistence (`refdata.locate` table) and the
//! lending-feed adapter.
//!
//! Design: `design/sentin8-exchange8-clearing-depth-technical-design.md` §3.

use std::collections::HashMap;

/// Locate request outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocateDecision {
    /// Approved: (locate_id, approved qty).
    Approved(u64, u64),
    /// Rejected: reason key.
    Rejected(&'static str),
}

/// A locate ledger entry (one approved locate).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Locate {
    pub locate_id: u64,
    pub account_id: u64,
    pub instrument_id: u32,
    /// Approved qty.
    pub qty: u64,
    /// Qty already consumed by executed short sales.
    pub consumed: u64,
    /// Expiry time (µs epoch); locates are time-boxed (Reg SHO).
    pub expires_us: u64,
}

/// Borrowable inventory for one instrument from the lending desk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BorrowInventory {
    pub instrument_id: u32,
    /// Shares available to borrow.
    pub available: u64,
    /// Borrow fee in bps (annualized).
    pub fee_bps: u32,
}

/// The locate ledger: tracks inventory, issues locates, consumes them at
/// short-sale execution, expires stale entries.
pub struct LocateLedger {
    /// Borrow inventory per instrument (lending-feed projection).
    inventory: HashMap<u32, BorrowInventory>,
    /// Issued locates by id.
    locates: HashMap<u64, Locate>,
    /// Per (instrument, account) outstanding locate ids.
    by_account: HashMap<(u32, u64), Vec<u64>>,
    next_id: u64,
    /// Clock for expiry (µs epoch); injected for determinism.
    now_us: u64,
}

impl LocateLedger {
    pub fn new(now_us: u64) -> Self {
        LocateLedger {
            inventory: HashMap::new(),
            locates: HashMap::new(),
            by_account: HashMap::new(),
            next_id: 1,
            now_us,
        }
    }

    /// Update borrow inventory from the lending feed (full replace per
    /// instrument).
    pub fn set_inventory(&mut self, inv: BorrowInventory) {
        self.inventory.insert(inv.instrument_id, inv);
    }

    /// Request a locate. Approved when: instrument has borrow inventory,
    /// the requested qty fits the *unreserved* availability (inventory minus
    /// outstanding unconsumed locates), and qty > 0.
    pub fn request(&mut self, account_id: u64, instrument_id: u32, qty: u64) -> LocateDecision {
        if qty == 0 {
            return LocateDecision::Rejected("zero_qty");
        }
        let Some(inv) = self.inventory.get(&instrument_id) else {
            return LocateDecision::Rejected("no_inventory");
        };
        // Reserved = outstanding unconsumed, unexpired locates.
        let reserved: u64 = self
            .locates
            .values()
            .filter(|l| l.instrument_id == instrument_id)
            .filter(|l| l.consumed < l.qty && l.expires_us > self.now_us)
            .map(|l| l.qty - l.consumed)
            .sum();
        if inv.available.saturating_sub(reserved) < qty {
            return LocateDecision::Rejected("insufficient_inventory");
        }
        let id = self.next_id;
        self.next_id += 1;
        // Reg SHO locate validity window (default 3 business days is the
        // convention; here 24h in µs).
        let expires_us = self.now_us + 24 * 3600 * 1_000_000;
        let locate = Locate {
            locate_id: id,
            account_id,
            instrument_id,
            qty,
            consumed: 0,
            expires_us,
        };
        self.locates.insert(id, locate);
        self.by_account
            .entry((instrument_id, account_id))
            .or_default()
            .push(id);
        LocateDecision::Approved(id, qty)
    }

    /// Consume `qty` from an account's outstanding locates for an instrument
    /// at short-sale execution (oldest-first). Returns true if a valid
    /// (unexpired, unconsumed) locate chain covered the qty.
    pub fn consume(&mut self, account_id: u64, instrument_id: u32, qty: u64) -> bool {
        let ids = match self.by_account.get(&(instrument_id, account_id)) {
            Some(v) if !v.is_empty() => v.clone(),
            _ => return false,
        };
        let mut need = qty;
        for id in ids {
            if need == 0 {
                break;
            }
            let Some(l) = self.locates.get_mut(&id) else {
                continue;
            };
            if l.expires_us <= self.now_us {
                continue; // expired - unusable
            }
            let avail = l.qty - l.consumed;
            if avail == 0 {
                continue;
            }
            let take = avail.min(need);
            l.consumed += take;
            need -= take;
        }
        need == 0
    }

    /// Drop expired locates (cron sweep); returns the ids expired.
    pub fn expire_stale(&mut self) -> Vec<u64> {
        let now = self.now_us;
        let expired: Vec<u64> = self
            .locates
            .iter()
            .filter(|(_, l)| l.expires_us <= now)
            .map(|(id, _)| *id)
            .collect();
        for id in &expired {
            self.locates.remove(id);
        }
        self.by_account.retain(|_, v| {
            v.retain(|id| !expired.contains(id));
            !v.is_empty()
        });
        expired
    }

    /// Advance the clock (expiry determinism).
    pub fn advance_clock(&mut self, to_us: u64) {
        self.now_us = self.now_us.max(to_us);
    }

    /// Outstanding (unconsumed, unexpired) qty for an account+instrument.
    pub fn outstanding(&self, account_id: u64, instrument_id: u32) -> u64 {
        self.by_account
            .get(&(instrument_id, account_id))
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.locates.get(id))
                    .filter(|l| l.expires_us > self.now_us)
                    .map(|l| l.qty - l.consumed)
                    .sum()
            })
            .unwrap_or(0)
    }

    /// Locate by id (diagnostics).
    pub fn locate(&self, id: u64) -> Option<&Locate> {
        self.locates.get(&id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger_with_inventory(available: u64) -> LocateLedger {
        let mut l = LocateLedger::new(1_000_000);
        l.set_inventory(BorrowInventory {
            instrument_id: 7,
            available,
            fee_bps: 350,
        });
        l
    }

    #[test]
    fn request_approves_when_inventory_fits() {
        let mut l = ledger_with_inventory(1000);
        match l.request(42, 7, 500) {
            LocateDecision::Approved(id, qty) => {
                assert_eq!(id, 1);
                assert_eq!(qty, 500);
                assert_eq!(l.outstanding(42, 7), 500);
            }
            _ => panic!("expected approval"),
        }
    }

    #[test]
    fn request_rejects_no_inventory() {
        let mut l = LocateLedger::new(1_000_000);
        assert_eq!(
            l.request(1, 99, 100),
            LocateDecision::Rejected("no_inventory")
        );
    }

    #[test]
    fn request_rejects_zero() {
        let mut l = ledger_with_inventory(1000);
        assert_eq!(l.request(1, 7, 0), LocateDecision::Rejected("zero_qty"));
    }

    #[test]
    fn reservations_cap_concurrent_locates() {
        let mut l = ledger_with_inventory(1000);
        assert!(matches!(
            l.request(1, 7, 600),
            LocateDecision::Approved(_, _)
        ));
        // 600 reserved of 1000 → 500 request rejected.
        assert_eq!(
            l.request(2, 7, 500),
            LocateDecision::Rejected("insufficient_inventory")
        );
        // 400 fits exactly.
        assert!(matches!(
            l.request(2, 7, 400),
            LocateDecision::Approved(_, _)
        ));
    }

    #[test]
    fn consume_uses_locates_oldest_first() {
        let mut l = ledger_with_inventory(1000);
        l.request(42, 7, 100); // id 1
        l.request(42, 7, 100); // id 2
                               // Consume 150 → 100 from id 1, 50 from id 2.
        assert!(l.consume(42, 7, 150));
        assert_eq!(l.locate(1).unwrap().consumed, 100);
        assert_eq!(l.locate(2).unwrap().consumed, 50);
        assert_eq!(l.outstanding(42, 7), 50);
        // Remaining 50 covers exactly.
        assert!(l.consume(42, 7, 50));
        assert!(!l.consume(42, 7, 1)); // exhausted
    }

    #[test]
    fn consume_fails_without_locate() {
        let mut l = ledger_with_inventory(1000);
        assert!(!l.consume(42, 7, 10));
    }

    #[test]
    fn consumption_releases_reservation_for_new_locates() {
        let mut l = ledger_with_inventory(1000);
        l.request(1, 7, 800);
        assert!(l.consume(1, 7, 800)); // fully consumed → reservation released
        assert!(matches!(
            l.request(2, 7, 800),
            LocateDecision::Approved(_, _)
        ));
    }

    #[test]
    fn expired_locates_unusable_and_swept() {
        let mut l = ledger_with_inventory(1000);
        l.request(42, 7, 100);
        // Advance past the 24h validity window.
        l.advance_clock(1_000_000 + 24 * 3600 * 1_000_000 + 1);
        assert!(!l.consume(42, 7, 10)); // expired
        let expired = l.expire_stale();
        assert_eq!(expired, vec![1]);
        assert_eq!(l.outstanding(42, 7), 0);
        // Reservation released after sweep.
        assert!(matches!(
            l.request(43, 7, 1000),
            LocateDecision::Approved(_, _)
        ));
    }

    #[test]
    fn inventory_shrink_does_not_revoke_issued_locates() {
        // A feed update showing less borrow does not revoke outstanding
        // locates (they were valid when issued); new requests are capped.
        let mut l = ledger_with_inventory(1000);
        l.request(1, 7, 600);
        l.set_inventory(BorrowInventory {
            instrument_id: 7,
            available: 100,
            fee_bps: 900,
        });
        assert_eq!(l.outstanding(1, 7), 600); // still outstanding
        assert_eq!(
            l.request(2, 7, 50),
            LocateDecision::Rejected("insufficient_inventory") // 600 reserved > 100
        );
    }
}
