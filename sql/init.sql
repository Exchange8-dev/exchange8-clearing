-- SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
-- SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
-- SPDX-FileCopyrightText: Copyright Contributors to the Exchange8:Clearing
-- SPDX-License-Identifier: Apache-2.0

-- Ex8:Clearing — event-sourced trading ledger + drop copy.
--
-- Design: design/sentin8-exchange8-data-model-technical-design.md §3.2 (CL) + §3.4.
-- No migration code (new platform): `sentin8-cli vertical init` applies this.

CREATE SCHEMA IF NOT EXISTS clearing;

-- Order day log (legacy FIX_ORDER).
CREATE TABLE IF NOT EXISTS clearing.fix_order (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id          UUID NOT NULL,
    order_id        BIGINT NOT NULL,     -- leased in-process (batch lease; design doc §2a)
    connection_id   TEXT NOT NULL,
    cl_ord_id       TEXT NOT NULL,
    security_id     UUID,
    symbol          TEXT NOT NULL,
    side            CHAR(1) NOT NULL CHECK (side IN ('B','S')),
    order_type      CHAR(1) NOT NULL CHECK (order_type IN ('L','F','I','P','B')),
    tif             TEXT NOT NULL DEFAULT '0',
    qty             NUMERIC(18,8) NOT NULL,
    price           NUMERIC(18,8),
    visible_qty     NUMERIC(18,8),
    capacity        TEXT,
    lot_type        CHAR(1) NOT NULL DEFAULT '2' CHECK (lot_type IN ('1','2')),
    stp_action      TEXT NOT NULL DEFAULT 'D',
    account         TEXT,
    trader_id       TEXT,
    ord_status      TEXT NOT NULL DEFAULT '0',
    cum_qty         NUMERIC(18,8) NOT NULL DEFAULT 0,
    ts              TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (env_id, order_id)
);
CREATE INDEX IF NOT EXISTS idx_exchange8_fixorder_env_conn_ts ON clearing.fix_order (env_id, connection_id, ts);
CREATE INDEX IF NOT EXISTS idx_exchange8_fixorder_env_symbol ON clearing.fix_order (env_id, symbol);

-- Lifecycle journal (legacy ORDER_UPDATE) — append-only.
CREATE TABLE IF NOT EXISTS clearing.order_update (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id      UUID NOT NULL,
    order_id    BIGINT NOT NULL,
    type_code   CHAR(1) NOT NULL CHECK (type_code IN
                ('A','B','P','S','C','X','T','R','M','I')),
    volume      NUMERIC(18,8),
    price       NUMERIC(18,8),
    update_time TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_exchange8_ordupd_order ON clearing.order_update (env_id, order_id);

-- Replace mapping (legacy ORDER_REPLACE).
CREATE TABLE IF NOT EXISTS clearing.order_replace (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id          UUID NOT NULL,
    connection_id   TEXT NOT NULL,
    orig_cl_ord_id  TEXT NOT NULL,
    new_cl_ord_id   TEXT NOT NULL,
    price           NUMERIC(18,8),
    volume          NUMERIC(18,8)
);

-- Cross extension (legacy ORDER_CROSS).
CREATE TABLE IF NOT EXISTS clearing.order_cross (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id      UUID NOT NULL,
    order_id    BIGINT NOT NULL,
    cross_type  TEXT NOT NULL,
    account     TEXT,
    contra_account TEXT,
    sell_price  NUMERIC(18,8),
    sell_qty    NUMERIC(18,8)
);

-- Executions (legacy TRADE) — one row per side, shared trade_id.
CREATE TABLE IF NOT EXISTS clearing.trade (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id          UUID NOT NULL,
    trade_id        BIGINT NOT NULL,
    trade_type      CHAR(1) NOT NULL DEFAULT 'R' CHECK (trade_type IN ('R','C','A')),
    order_id        BIGINT NOT NULL,
    sub_order_id    INTEGER NOT NULL DEFAULT 0,
    liquidity       CHAR(1) NOT NULL CHECK (liquidity IN ('A','R')),
    volume          NUMERIC(18,8) NOT NULL,
    price           NUMERIC(18,8) NOT NULL,
    charge          NUMERIC(18,8) NOT NULL DEFAULT 0,
    bust_state      TEXT CHECK (bust_state IN ('active','busted')),
    amend_id        UUID,
    trade_ts        TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_exchange8_trade_env_ts ON clearing.trade (env_id, trade_ts);
CREATE INDEX IF NOT EXISTS idx_exchange8_trade_env_tradeid ON clearing.trade (env_id, trade_id);

-- Trade charges (legacy static/dynamic charge tables).
CREATE TABLE IF NOT EXISTS clearing.trade_charge_static (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id      UUID NOT NULL,
    category    TEXT NOT NULL,
    charge_type  TEXT NOT NULL,
    value       NUMERIC(18,8) NOT NULL
);
CREATE TABLE IF NOT EXISTS clearing.trade_charge_dynamic (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id      UUID NOT NULL,
    bucket_low  NUMERIC(18,8) NOT NULL,
    bucket_high NUMERIC(18,8) NOT NULL,
    charge_bps  NUMERIC(10,4) NOT NULL
);
CREATE TABLE IF NOT EXISTS clearing.trade_category (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id      UUID NOT NULL,
    code        TEXT NOT NULL UNIQUE,
    description TEXT
);

-- Bust/amend records.
CREATE TABLE IF NOT EXISTS clearing.bust_trade (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id      UUID NOT NULL,
    trade_id    BIGINT NOT NULL,
    reason      TEXT NOT NULL CHECK (reason IN ('erroneous','obvious_error','regulatory')),
    busted_by   TEXT NOT NULL,
    busted_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (env_id, trade_id)
);
CREATE TABLE IF NOT EXISTS clearing.amend_trade (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id      UUID NOT NULL,
    trade_id    BIGINT NOT NULL,
    new_qty     NUMERIC(18,8),
    new_price   NUMERIC(18,8),
    wash        BOOLEAN NOT NULL DEFAULT FALSE,
    principal   BOOLEAN NOT NULL DEFAULT FALSE,
    amended_by  TEXT NOT NULL,
    amended_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Drop-copy sessions (legacy FIXDROP_CONNECTION).
CREATE TABLE IF NOT EXISTS clearing.drop_connection (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id      UUID NOT NULL,
    comp_id     TEXT NOT NULL UNIQUE,
    mode        TEXT NOT NULL DEFAULT 'ALL' CHECK (mode IN ('ALL','FILLS')),
    status      TEXT NOT NULL DEFAULT 'E' CHECK (status IN ('E','D','H')),
    book_tokens TEXT[] NOT NULL DEFAULT '{}'
);

-- Drop entitlement junctions (legacy CUSTOMER_TO_DROP / TRADER_TO_DROP /
-- CONNECTION_TO_DROP parity).
CREATE TABLE IF NOT EXISTS clearing.drop_entitlement_customer (
    env_id              UUID NOT NULL,
    drop_connection_id UUID NOT NULL REFERENCES clearing.drop_connection(id),
    customer_id         UUID NOT NULL,
    PRIMARY KEY (drop_connection_id, customer_id)
);
CREATE TABLE IF NOT EXISTS clearing.drop_entitlement_trader (
    env_id              UUID NOT NULL,
    drop_connection_id UUID NOT NULL REFERENCES clearing.drop_connection(id),
    trader_id          UUID NOT NULL,
    PRIMARY KEY (drop_connection_id, trader_id)
);
CREATE TABLE IF NOT EXISTS clearing.drop_entitlement_connection (
    env_id              UUID NOT NULL,
    drop_connection_id UUID NOT NULL REFERENCES clearing.drop_connection(id),
    connection_id      TEXT NOT NULL,
    PRIMARY KEY (drop_connection_id, connection_id)
);

-- Drop resend store (legacy FIXDROP_MESSAGE) — retention-purged.
CREATE TABLE IF NOT EXISTS clearing.drop_message (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id      UUID NOT NULL,
    connection_id UUID NOT NULL REFERENCES clearing.drop_connection(id),
    seq_no      BIGINT NOT NULL,
    msg_type    CHAR(1) NOT NULL,
    msg_body    BYTEA NOT NULL,
    ts          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (connection_id, seq_no)
);

-- ---------------------------------------------------------------------------
-- RLS (architecture-principles.md §4.3): enable + force on all tenant tables.
-- Policies use the platform helpers sentin8_context_initialized() and
-- sentin8_current_env_id(); services call set_tenant_context on the same
-- connection as the data query.
-- ---------------------------------------------------------------------------
DO $$
DECLARE tbl TEXT;
BEGIN
    FOREACH tbl IN ARRAY ARRAY['clearing.fix_order', 'clearing.order_update', 'clearing.order_replace', 'clearing.order_cross', 'clearing.trade', 'clearing.trade_charge_static', 'clearing.trade_charge_dynamic', 'clearing.trade_category', 'clearing.bust_trade', 'clearing.amend_trade', 'clearing.drop_connection', 'clearing.drop_entitlement_customer', 'clearing.drop_entitlement_trader', 'clearing.drop_entitlement_connection', 'clearing.drop_message'] LOOP
        BEGIN
            EXECUTE format('ALTER TABLE %s ENABLE ROW LEVEL SECURITY', tbl);
            EXECUTE format('ALTER TABLE %s FORCE ROW LEVEL SECURITY', tbl);
            EXECUTE format('DROP POLICY IF EXISTS %1$I_select ON %2$s;
CREATE POLICY %1$I_select ON %2$s FOR SELECT USING (sentin8_context_initialized() AND env_id = sentin8_current_env_id())', split_part(tbl, '.', 2), tbl);
            EXECUTE format('DROP POLICY IF EXISTS %1$I_insert ON %2$s;
CREATE POLICY %1$I_insert ON %2$s FOR INSERT WITH CHECK (sentin8_context_initialized() AND env_id = sentin8_current_env_id())', split_part(tbl, '.', 2), tbl);
            EXECUTE format('DROP POLICY IF EXISTS %1$I_update ON %2$s;
CREATE POLICY %1$I_update ON %2$s FOR UPDATE USING (sentin8_context_initialized() AND env_id = sentin8_current_env_id())', split_part(tbl, '.', 2), tbl);
            EXECUTE format('DROP POLICY IF EXISTS %1$I_delete ON %2$s;
CREATE POLICY %1$I_delete ON %2$s FOR DELETE USING (sentin8_context_initialized() AND env_id = sentin8_current_env_id())', split_part(tbl, '.', 2), tbl);
        EXCEPTION WHEN OTHERS THEN
            RAISE NOTICE 'RLS setup failed for %: %', tbl, SQLERRM;
        END;
    END LOOP;
END $$;

-- ---------------------------------------------------------------------------
-- Per-service role grants (schema-per-service: this service owns schema
-- `clearing`; grants live in db `exchange8` only — design doc §1).
-- ---------------------------------------------------------------------------
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'exchange8_clearing_service') THEN
        ALTER ROLE exchange8_clearing_service SET search_path = clearing, public;
        GRANT USAGE ON SCHEMA clearing TO exchange8_clearing_service;
        GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA clearing TO exchange8_clearing_service;
        GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA clearing TO exchange8_clearing_service;
    END IF;
END $$;

-- ---------------------------------------------------------------------------
-- Local outbox (DBX §6.7 — generated from `sentin8_outbox::outbox_table_sql(
-- "clearing")`): rows written in the same DB transaction as the domain change;
-- the shell's relay (boot step §8b) pushes them to Bus `POST /v1/produce`.
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS clearing.outbox (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id          UUID NOT NULL,
    event_type      TEXT NOT NULL,
    stream_family   TEXT NOT NULL,
    payload         JSONB NOT NULL,
    publisher_id_token  TEXT,
    publisher_subject   TEXT,
    relayed         BOOLEAN NOT NULL DEFAULT false,
    relayed_at      TIMESTAMPTZ,
    relay_attempts  INT NOT NULL DEFAULT 0,
    next_retry_at   TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_clearing_outbox_unrelayed
    ON clearing.outbox (created_at) WHERE relayed = false;
DO $$ BEGIN
    EXECUTE 'ALTER TABLE clearing.outbox ENABLE ROW LEVEL SECURITY';
    EXECUTE 'ALTER TABLE clearing.outbox FORCE ROW LEVEL SECURITY';
    EXECUTE 'DROP POLICY IF EXISTS outbox_select ON clearing.outbox';
    EXECUTE 'CREATE POLICY outbox_select ON clearing.outbox FOR SELECT
        USING (sentin8_context_initialized()
               AND env_id = sentin8_current_env_id())';
    EXECUTE 'DROP POLICY IF EXISTS outbox_insert ON clearing.outbox';
    EXECUTE 'CREATE POLICY outbox_insert ON clearing.outbox FOR INSERT
        WITH CHECK (sentin8_context_initialized()
               AND env_id = sentin8_current_env_id())';
    EXECUTE 'DROP POLICY IF EXISTS outbox_update ON clearing.outbox';
    EXECUTE 'CREATE POLICY outbox_update ON clearing.outbox FOR UPDATE
        USING (sentin8_context_initialized()
               AND env_id = sentin8_current_env_id())';
EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'RLS setup failed for clearing.outbox: %', SQLERRM;
END $$;

CREATE OR REPLACE FUNCTION clearing.clearing_notify_outbox() RETURNS TRIGGER
LANGUAGE plpgsql AS $fn$
BEGIN
    PERFORM pg_notify('clearing_outbox_changed', '');
    RETURN NEW;
END;
$fn$;
DROP TRIGGER IF EXISTS outbox_notify_trigger ON clearing.outbox;
CREATE TRIGGER outbox_notify_trigger
    AFTER INSERT ON clearing.outbox
    FOR EACH ROW
    EXECUTE FUNCTION clearing.clearing_notify_outbox();

-- ---------------------------------------------------------------------------
-- Local audit journal (DBX §6.8 — generated from
-- `sentin8_audit::audit_journal_sql("clearing")`): rows written in the same
-- transaction as the domain change; the shell's pusher (boot step §8c) pushes
-- them to Sys `POST /v1/audit` (`sentin8:sys:audit:write`). A Sys outage
-- never drops audit.
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS clearing.audit_journal (
    id              UUID PRIMARY KEY,
    env_id          UUID NOT NULL,
    action          TEXT NOT NULL,
    target_resource_type TEXT,
    target_resource_id    TEXT,
    actor_user_id   TEXT,
    actor_subject_type TEXT,
    severity        TEXT NOT NULL DEFAULT 'info',
    correlation_id  TEXT,
    before_state    JSONB,
    after_state     JSONB,
    details         JSONB,
    pushed          BOOLEAN NOT NULL DEFAULT false,
    pushed_at       TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_clearing_audit_journal_unpushed
    ON clearing.audit_journal (created_at) WHERE pushed = false;
DO $$ BEGIN
    EXECUTE 'ALTER TABLE clearing.audit_journal ENABLE ROW LEVEL SECURITY';
    EXECUTE 'ALTER TABLE clearing.audit_journal FORCE ROW LEVEL SECURITY';
    EXECUTE 'DROP POLICY IF EXISTS audit_journal_dml ON clearing.audit_journal';
    EXECUTE 'CREATE POLICY audit_journal_dml ON clearing.audit_journal
        USING (sentin8_context_initialized()
               AND env_id = sentin8_current_env_id())
        WITH CHECK (sentin8_context_initialized()
               AND env_id = sentin8_current_env_id())';
EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'RLS setup failed for clearing.audit_journal: %', SQLERRM;
END $$;

-- Outbox + journal role grants (same pattern as the table grants above).
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'exchange8_clearing_service') THEN
        GRANT SELECT, INSERT, UPDATE ON ALL TABLES IN SCHEMA clearing TO exchange8_clearing_service;
    END IF;
END $$;
