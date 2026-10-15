# BUSINESS.md: Exchange8:Clearing — Business Requirements & Functionality

> Audience: product owners, business analysts, and anyone evaluating what
> Exchange8:Clearing **does for the business**. For architecture read `README.md`;
> for wire-level detail read `DESIGN.md`.

## 1. Business Purpose

Exchange8:Clearing is the exchange's book of record for every order and trade that
happened on the venue. It turns the venue's raw event stream into an
authoritative, regulator-grade order journal and execution ledger; it lets
clearing firms watch their firms' activity in real time through drop copies;
it lets operations correct errors on the record through controlled, audited
trade busts; and it enforces the short-sale locate discipline the regulator
requires before any short sale executes. The ledger it produces is also the
raw material every other capability downstream consumes — market data trade
series, regulatory reports, settlement netting, and surveillance analytics.

## 2. Business Scope (What the Service Provides)

| Capability | What it delivers |
|---|---|
| Order day log | A complete journal of every accepted order with client order id, side, type, quantity, price, and status progression. |
| Order lifecycle journal | A one-letter-coded, append-only history of every order state transition, including replaces and rejections. |
| Trade ledger | Every execution as one-row-per-side with a shared trade id, liquidity attribution (maker/taker), price, quantity, and charges. |
| Trade bust | Step-up-authorized cancellation of an erroneous/obviously-error/regulatorily-directed trade, idempotent and audited, with downstream fan-out. |
| Trade amend (storage) | Persistent amend records correcting quantity/price/wash/principal flags with the amender's identity. |
| Drop copy | Entitled, filtered real-time copies of order/trade events to clearing clients — fills-only or full mode, per-instrument and per-customer/trader filters, with reconnect resends. |
| Regulatory stream projection | Deduplicated, per-day-sequenced reportable order/trade events for the regulatory reporting chain. |
| Short-sale locate (core) | Reg SHO locate ledger: borrow-inventory-backed approval, consumption at execution, and expiry of instrument-scoped locate authorizations. |
| Drop-buffer purge (policy) | Retention-based deletion of the drop resend store (2555-day regulatory keep) executed via the scheduler proxy-back. |
| Ledger query API | Env-scoped trade and order queries for admins and authorized clients. |
| Admin surfaces | Shield policy administration plus declared admin pages for Order Journal, Trades, and Drop Copy. |

### What the service does NOT own (business boundary)

| Concern | Owner |
|---|---|
| Order matching and execution | Exchange8:ME |
| Pre-trade risk checks, shortable flags, margin | Exchange8:Risk, Exchange8:Margin |
| Regulatory report formatting/transport | Exchange8:Regulatory |
| CCP novation files, custody, collateral movement, give-up, block trades, EOD settlement exports | Exchange8:Settlement |
| Money movement | Bank8:Ledger |
| Scheduling of SOD/EOD/purge jobs | Sentin8:Cron |
| Identity/tokens | Sentin8:Auth |
| Policy decisions | Shield (sentin8-policy) |
| Reportable event transport sessions | Exchange8:Regulatory |

## 3. Business Capabilities

### Trade ledger

| Capability | Business rule |
|---|---|
| Every execution is captured | Each execution produces exactly one side row per participant with the same shared trade id; the pair is completed only when both sides arrive with matching price/quantity. |
| Maker/taker attribution | Each side row carries its liquidity (Added = maker, Removed = taker) so fees and attributions follow venue rules. |
| Charges at trade time | Commission tables (static and bps-tiered dynamic) are applied at trade time and stored on the trade row, feeding end-of-day records and netting. |
| Cross orders | Cross extensions (cross type, contra account, price/quantity) are journaled for negotiated crosses. |
| Replace tracking | Order replaces keep the original ↔ new client order id linkage so the history of an amended working order is unbroken. |
| Anomaly preservation | If the engine misbehaves (duplicate side, same-liquidity pair, price/quantity mismatch), the data is still recorded and the anomaly flagged for repair — the record is never silently "fixed". |

### Positions & lifecycle journal

| Capability | Business rule |
|---|---|
| Order state history | Every order transition (new, booked, accepted, posted, cancel-pending, cancelled, traded, replace-pending, replace-done, repriced) is journaled with its one-letter code. |
| Replaces keep lineage | A replace writes both the pending marker (original order) and the done marker (new order id) — the lineage is two entries, not an overwrite. |
| Rejections are journaled | A rejected order enters the journal as new-pending then cancelled, so attempts are as visible as fills. |
| Bust state on the trade | A busted trade keeps its original rows; the bust state flips to `busted` and a bust record preserves who busted, when, and why. |

### Locates

| Capability | Business rule |
|---|---|
| Locate approval | A locate is approved only when the instrument has borrow inventory and the requested quantity fits availability not already reserved by outstanding unconsumed, unexpired locates. |
| Locate consumption | Executed short sales consume approved locates oldest-first; a short sale whose locator chain cannot cover the quantity is not authorized. |
| Locate validity | A locate is instrument-scoped, quantity-capped, and time-boxed (24-hour validity); expired locates are unusable and are swept, releasing their reservations. |
| Inventory shrink safety | A lending-feed update showing less borrow never revokes already-issued locates (they were valid when issued); only new requests are capped. |
| Fee visibility | Borrow inventory carries the annualized borrow fee so locate economics are visible to the desk. |

### Drop copy

| Capability | Business rule |
|---|---|
| Entitlement filtering | A drop session sees exactly what it is entitled to: customer, trader, and originating-connection junctions; book-token allow-lists; and instrument allow-lists — an empty filter means all. |
| Fills-only mode | A fills-only session receives executions (and busts) but no working-order events. |
| Own sequences + resend | Each drop session has its own sequence per message and a persisted resend store, so a reconnecting client can recover a window of missed messages. |
| Session states | Drop sessions are managed (enable/disable/halt) through their status and book tokens. |
| Regulatory keep | Drop-buffer messages are retained for 2555 days (regulatory keep) and purged only by the retention policy. |

## 4. Business Events (Outbound + Inbound)

### Outbound

| Event (family/naming) | When it fires |
|---|---|
| `exchange8:clearing:order:recorded` | Order lifecycle journaled (order recorded into the ledger). |
| `exchange8:clearing:trade:busted` | A trade bust is applied — market data emits a broken-trade marker, gateway re-reports with cancel exec reports, drop buffers receive bust messages. |
| `exchange8:clearing:trade:amended` | A trade amendment is applied — same fan-out pattern with corrected values. |
| `exchange8:clearing:drop:sent` | Drop-copy messages are emitted to entitled sessions. |
| `exchange8:clearing:trade:reported` / `exchange8:clearing:order:reported` | Reportable lifecycle events enter the regulatory stream projection. |
| `exchange8:clearing:trade:novated` | Real-time CCP novation applied per trade (clearing-depth capability). |
| `exchange8:clearing:collateral:updated` | Collateral balances changed (clearing-depth capability). |

All outbound events are delivered through the service's local outbox and
relayed to Sentin8:Bus with the outbox row id as the idempotency key.

### Inbound (consumed)

| Stream | Used for |
|---|---|
| `exchange8:gateway:order:submitted` | Seeding the order day log. |
| `exchange8:me:order:booked / killed / modified / rejected / repriced` | Order lifecycle journal entries. |
| `exchange8:me:trade:executed` | Trade pairing and side rows; drop fan-out; regulatory stream. |
| `exchange8:sor:leg:filled (family)` | Routed fills into the ledger. |
| Lending-desk borrow inventory feed | Replacing locate inventory per instrument (locate core input). |

## 5. Business Rules & Invariants

1. **The ledger is append-only.** Busts and amends never rewrite or delete an
   original trade value; they add bust/amend records and status markers.
2. **Two sides, one trade.** Every executed trade is recorded exactly once
   per side with the same trade id; an unmatched side is a gap alert, not a
   silently created single-sided trade.
3. **Busts are once-and-once.** A trade is busted at most once per
   environment; replaying the same bust command is a no-op and is identified
   by a declared already-busted error.
4. **Busts need step-up authority.** Only holders of the trade-bust scope
   (with fresh user interaction enforced by the BFF) may bust, and every bust
   carries the buster's identity and a reason from the approved set
   (erroneous, obvious error, regulatory).
5. **Locates are prerequisites.** A covered short sale requires a valid,
   unconsumed, unexpired locate chain; locate capacity is bounded by borrow
   inventory net of outstanding reservations.
6. **Issued locates are stable.** Reducing borrow inventory never cancels
   issued locates; it only constrains new issuance.
7. **Drop copies see only entitlements.** A drop session never receives an
   event its junctions and filters deny, and each session's sequence is
   independent and resendable.
8. **Tenants see only their own books.** Every row is environment-scoped and
   protected by row-level security both in policy and in every query.
9. **Retention keeps what regulators ask for.** The drop resend store is kept
   2555 days; purge never touches another service's message stores.
10. **Replay is safe everywhere.** Event re-delivery, projector recovery, and
    bust/drop replays dedupe on declared idempotency keys — reprocessing
    never double-counts a trade, a bust, or a drop message.
11. **Audit never lags the trade.** Mutations carry audit records pushed to
    the platform audit service, and an audit-service outage only delays, never
    loses, entries.

## 6. Target-User Roles & Use Cases

| Persona | What they do with Exchange8:Clearing |
|---|---|
| Clearing operations clerk | Queries the trade and order blotters, verifies execution detail and charges on the books. |
| Trade support / error desk | Busts a clearly erroneous trade with the required reason and step-up authority, watching the fan-out downstream. |
| Regulatory reporting officer | Relies on the reportable stream projection (deduplicated, day-sequenced) that feeds the regulatory reporting chain. |
| Clearing firm / drop-copy subscriber | Consumes their entitled, filtered execution stream over their own FIX drop session with reliable resends. |
| Securities-lending desk | Maintains borrow inventory; approves and monitors locates their clients consume for short sales. |
| Compliance auditor | Reads the bust/amend history with who, when, and why; verifies retention of drop messages. |
| Platform admin | Manages Shield policy for the service and admin surfaces through declared admin scopes. |
| Exchange accountant | Uses the stored per-trade charges to reconcile commissions and feed netting. |

## 7. KPIs / Success Criteria

- **Completeness:** every executed trade yields exactly one pair of side rows
  with a shared trade id; zero unmatched sides at end-of-day outside alert.
- **Determinism:** replaying the event stream reproduces the ledger
  byte-identically.
- **Bust integrity:** 100% of busts carry reason, identity, and step-up scope;
  zero duplicate bust effects.
- **Drop-copy quality:** zero non-entitled deliveries; zero gaps in session
  sequences; resend window recovers missed messages after reconnect.
- **Locate coverage:** 100% of executed short sales traceable to a consumed,
  unexpired locate; zero over-consumption beyond inventory.
- **Latency:** projected ledger rows and drop fan-out within the venue's
  real-time clearing target under the design load (1M orders/day, 2M trade
  rows/day per environment).
- **Isolation:** zero cross-environment data visibility in the RLS matrix
  checks.
- **Audit durability:** zero audit loss during downstream audit-service
  outages.

## 8. Dependencies & Reliance on Other Services

- **Exchange8:ME / Exchange8:Gateway / Exchange8:SOR / Exchange8:OMS** — authoritative order and
  execution event producers; without their events there is no ledger.
- **Sentin8:Bus** — event transport and consumer groups; outbox relay target.
- **Sentin8:Auth** — token verification, M2M client registration (DCR),
  scopes.
- **Shield (sentin8-policy) via the shell** — every authorization decision.
- **Sentin8:Sys** — audit ingest and the hostname resolution used by the
  database-free TLS path.
- **Sentin8:Cron** — executes the drop-buffer purge policy on schedule.
- **Sentin8:DB (PostgreSQL 18, OAuth)** — persistence; the service's own
  role in its own database, no BYPASSRLS anywhere.
- **Securities-lending feed / prime-broker API (external)** — borrow
  inventory that backs the locate ledger.
- **Downstream consumers** — Exchange8:MD (broken trades), Exchange8:Gateway (FIX
  re-reports), drop sessions; their correctness depends on Clearing's events
  being emitted exactly once.

## 9. Acceptance Criteria

1. Order submittals, lifecycle changes, rejects, and reprices all appear in
   the order journal with the correct lifecycle codes, and replaces preserve
   original → new lineage.
2. Every execution appears as two side rows with one shared trade id,
   liquidity attribution, and the applied charge.
3. A trade bust: requires the bust scope, accepts only the approved reasons,
   is idempotent per trade, marks the trade busted, records who/why, and
   produces exactly one fan-out event downstream.
4. An unmatched or malformed trade side is surfaced as an anomaly (never
   silently repaired) while retaining the data for repair.
5. A drop session configured fills-only receives executions and busts but no
   working-order events; instrument- and entitlement-restricted sessions
   receive exactly their entitled events.
6. A reconnecting drop session resends its missed window from the persisted
   store with its own sequence numbers.
7. A locate is approved only within free borrow inventory; consumption is
   oldest-first; an expired locate cannot be consumed and is swept,
   releasing its reservation.
8. Reducing borrow inventory never cancels issued locates but does cap new
   issuance.
9. Every ledger table enforces row-level security by environment; a query
   without tenant context sees zero rows and an insert without context is
   rejected (probe-verified).
10. A failed event relay retries and, past its budget, lands in the dead
    letter queue with manual replay — no silent event loss.
11. An audit-service outage leaves journal rows unpushed but durable, and
    they drain once the service recovers.
12. Drop-buffer rows older than the retention window are removed by the
    scheduled purge and nothing else is.

## 10. Open Business Questions

- **Routed-fill ledger shape.** The design anticipates dedicated routed-trade
  records for SOR-executed orders; the current schema has no such table, so
  how routed fills are surfaced to clearing participants (identical to venue
  trades, or a distinct record class) is undecided.
- **Drop-copy FIX transport sessions.** The entitlement engine and resend
  store exist, but the outbound FIX session management (session ops, trace
  toggles, forced logout) is not yet operated by this service — who operates
  the session layer and via which endpoint is open.
- **Locate persistence and the borrowing desk feed.** The locate logic is
  proven in core, but no locate table or feed adapter is live; which storage
  and which feed contract (securities-lending desk vs prime-broker API) the
  business will use is an open commercial/technical question.
- **Real-time CCP novation and collateral.** The business capability of
  novating every trade to a CCP in real time (and tracking collateral) is
  designed but not provided by this service; whether it lands here or in
  Exchange8:Settlement is an open scoping decision.
- **Bust scope naming.** The platform-wide registered bust scope is
  `exchange8:trade:bust`; design materials also reference a
  `bust:execute` variant — the business-facing permission name should be
  settled once and reflected everywhere.
- **Regulatory stream persistence boundary.** The deduplicated reportable
  projection exists here, while regulatory transport belongs to
  Exchange8:Regulatory; the persistence boundary for reportables between the two
  is undecided.

---

**Design attribution:** exchange8:clearing and its underlying concepts were designed by Hamid Jahanzad <Hamid@Jahanzad.com>.
