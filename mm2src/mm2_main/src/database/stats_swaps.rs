use crate::mm2::lp_swap::{MakerSavedSwap, SavedSwap, SavedSwapIo, TakerSavedSwap};
use common::log::{debug, error};
use db_common::{owned_named_params,
                sqlite::{rusqlite::{Connection, OptionalExtension},
                         AsSqlNamedParams, OwnedSqlNamedParams}};
use mm2_core::mm_ctx::MmArc;
use std::collections::HashSet;

const CREATE_STATS_SWAPS_TABLE: &str = "CREATE TABLE IF NOT EXISTS stats_swaps (
    id INTEGER NOT NULL PRIMARY KEY,
    maker_coin VARCHAR(255) NOT NULL,
    taker_coin VARCHAR(255) NOT NULL,
    uuid VARCHAR(255) NOT NULL UNIQUE,
    started_at INTEGER NOT NULL,
    finished_at INTEGER NOT NULL,
    maker_amount DECIMAL NOT NULL,
    taker_amount DECIMAL NOT NULL,
    is_success INTEGER NOT NULL
);";

const INSERT_STATS_SWAP_ON_INIT: &str = "INSERT INTO stats_swaps (
    maker_coin,
    taker_coin,
    uuid,
    started_at,
    finished_at,
    maker_amount,
    taker_amount,
    is_success
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)";

const INSERT_STATS_SWAP: &str = "INSERT INTO stats_swaps (
    maker_coin,
    maker_coin_ticker,
    maker_coin_platform,
    taker_coin,
    taker_coin_ticker,
    taker_coin_platform,
    uuid,
    started_at,
    finished_at,
    maker_amount,
    taker_amount,
    is_success,
    maker_coin_usd_price,
    taker_coin_usd_price,
    maker_pubkey,
    taker_pubkey
) VALUES (:maker_coin, :maker_coin_ticker, :maker_coin_platform, :taker_coin, :taker_coin_ticker, 
:taker_coin_platform, :uuid, :started_at, :finished_at, :maker_amount, :taker_amount, :is_success, 
:maker_coin_usd_price, :taker_coin_usd_price, :maker_pubkey, :taker_pubkey)";

pub const ADD_COINS_PRICE_INFOMATION: &[&str] = &[
    "ALTER TABLE stats_swaps ADD COLUMN maker_coin_usd_price DECIMAL;",
    "ALTER TABLE stats_swaps ADD COLUMN taker_coin_usd_price DECIMAL;",
];

pub const ADD_MAKER_TAKER_PUBKEYS: &[&str] = &[
    "ALTER TABLE stats_swaps ADD COLUMN maker_pubkey VARCHAR(255);",
    "ALTER TABLE stats_swaps ADD COLUMN taker_pubkey VARCHAR(255);",
];

pub const ADD_SPLIT_TICKERS: &[&str] = &[
    "ALTER TABLE stats_swaps ADD COLUMN maker_coin_ticker VARCHAR(255) NOT NULL DEFAULT '';",
    "ALTER TABLE stats_swaps ADD COLUMN maker_coin_platform VARCHAR(255) NOT NULL DEFAULT '';",
    "ALTER TABLE stats_swaps ADD COLUMN taker_coin_ticker VARCHAR(255) NOT NULL DEFAULT '';",
    "ALTER TABLE stats_swaps ADD COLUMN taker_coin_platform VARCHAR(255) NOT NULL DEFAULT '';",
    "UPDATE stats_swaps SET maker_coin_ticker = CASE instr(maker_coin, '-') \
        WHEN 0 THEN maker_coin \
        ELSE substr(maker_coin, 0, instr(maker_coin, '-')) \
        END;",
    "UPDATE stats_swaps SET maker_coin_platform = CASE instr(maker_coin, '-') \
        WHEN 0 THEN '' \
        ELSE substr(maker_coin, instr(maker_coin, '-') + 1) \
        END;",
    "UPDATE stats_swaps SET taker_coin_ticker = CASE instr(taker_coin, '-') \
        WHEN 0 THEN taker_coin \
        ELSE substr(taker_coin, 0, instr(taker_coin, '-')) \
        END;",
    "UPDATE stats_swaps SET taker_coin_platform = CASE instr(taker_coin, '-') \
        WHEN 0 THEN '' \
        ELSE substr(taker_coin, instr(taker_coin, '-') + 1) \
        END;",
];

pub const ADD_STARTED_AT_INDEX: &str = "CREATE INDEX timestamp_index ON stats_swaps (started_at);";

pub const SELECT_ID_BY_UUID: &str = "SELECT id FROM stats_swaps WHERE uuid = ?1";

/// Returns SQL statements to initially fill stats_swaps table using existing DB with JSON files
pub async fn create_and_fill_stats_swaps_from_json_statements(ctx: &MmArc) -> Vec<(&'static str, Vec<String>)> {
    let maker_swaps = SavedSwap::load_all_from_maker_stats_db(ctx).await.unwrap_or_default();
    let taker_swaps = SavedSwap::load_all_from_taker_stats_db(ctx).await.unwrap_or_default();

    let mut result = vec![(CREATE_STATS_SWAPS_TABLE, vec![])];
    let mut inserted_maker_uuids = HashSet::with_capacity(maker_swaps.len());

    for maker_swap in maker_swaps {
        if let Some(sql_with_params) = insert_stats_maker_swap_sql_init(&maker_swap) {
            inserted_maker_uuids.insert(maker_swap.uuid);
            result.push(sql_with_params);
        }
    }
    for taker_swap in taker_swaps {
        if inserted_maker_uuids.contains(&taker_swap.uuid) {
            continue;
        }
        if let Some(sql_with_params) = insert_stats_taker_swap_sql_init(&taker_swap) {
            result.push(sql_with_params);
        }
    }
    result
}

fn split_coin(coin: &str) -> (String, String) {
    let mut split = coin.split('-');
    let ticker = split.next().expect("split returns empty string at least").into();
    let platform = split.next().map_or("".into(), |platform| platform.into());
    (ticker, platform)
}

fn insert_stats_maker_swap_sql(swap: &MakerSavedSwap) -> Option<(&'static str, OwnedSqlNamedParams)> {
    let swap_data = match swap.swap_data() {
        Ok(d) => d,
        Err(e) => {
            error!("Error {} on getting swap {} data", e, swap.uuid);
            return None;
        },
    };

    let finished_at = match swap.finished_at() {
        Ok(t) => t.to_string(),
        Err(e) => {
            error!("Error {} on getting swap {} finished_at", e, swap.uuid);
            return None;
        },
    };

    let pubkeys = match swap.swap_pubkeys() {
        Ok(p) => p,
        Err(e) => {
            error!("Error {} on getting swap {} pubkeys", e, swap.uuid);
            return None;
        },
    };

    let is_success = swap
        .is_success()
        .expect("is_success can return error only when swap is not finished");

    let (maker_coin_ticker, maker_coin_platform) = split_coin(&swap_data.maker_coin);
    let (taker_coin_ticker, taker_coin_platform) = split_coin(&swap_data.taker_coin);

    let params = owned_named_params! {
        ":maker_coin": swap_data.maker_coin.clone(),
        ":maker_coin_ticker": maker_coin_ticker,
        ":maker_coin_platform": maker_coin_platform,
        ":taker_coin": swap_data.taker_coin.clone(),
        ":taker_coin_ticker": taker_coin_ticker,
        ":taker_coin_platform": taker_coin_platform,
        ":uuid": swap.uuid.to_string(),
        ":started_at": swap_data.started_at.to_string(),
        ":finished_at": finished_at,
        ":maker_amount": swap_data.maker_amount.to_string(),
        ":taker_amount": swap_data.taker_amount.to_string(),
        ":is_success": (is_success as u32).to_string(),
        ":maker_coin_usd_price": swap.maker_coin_usd_price.as_ref().map(|p| p.to_string()),
        ":taker_coin_usd_price": swap.taker_coin_usd_price.as_ref().map(|p| p.to_string()),
        ":maker_pubkey": pubkeys.maker,
        ":taker_pubkey": pubkeys.taker,
    };

    Some((INSERT_STATS_SWAP, params))
}

fn insert_stats_maker_swap_sql_init(swap: &MakerSavedSwap) -> Option<(&'static str, Vec<String>)> {
    let swap_data = match swap.swap_data() {
        Ok(d) => d,
        Err(e) => {
            error!("Error {} on getting swap {} data", e, swap.uuid);
            return None;
        },
    };
    let finished_at = match swap.finished_at() {
        Ok(t) => t.to_string(),
        Err(e) => {
            error!("Error {} on getting swap {} finished_at", e, swap.uuid);
            return None;
        },
    };
    let is_success = swap
        .is_success()
        .expect("is_success can return error only when swap is not finished");

    let params = vec![
        swap_data.maker_coin.clone(),
        swap_data.taker_coin.clone(),
        swap.uuid.to_string(),
        swap_data.started_at.to_string(),
        finished_at,
        swap_data.maker_amount.to_string(),
        swap_data.taker_amount.to_string(),
        (is_success as u32).to_string(),
    ];
    Some((INSERT_STATS_SWAP_ON_INIT, params))
}

fn insert_stats_taker_swap_sql(swap: &TakerSavedSwap) -> Option<(&'static str, OwnedSqlNamedParams)> {
    let swap_data = match swap.swap_data() {
        Ok(d) => d,
        Err(e) => {
            error!("Error {} on getting swap {} data", e, swap.uuid);
            return None;
        },
    };
    let finished_at = match swap.finished_at() {
        Ok(t) => t.to_string(),
        Err(e) => {
            error!("Error {} on getting swap {} finished_at", e, swap.uuid);
            return None;
        },
    };

    let pubkeys = match swap.swap_pubkeys() {
        Ok(p) => p,
        Err(e) => {
            error!("Error {} on getting swap {} pubkeys", e, swap.uuid);
            return None;
        },
    };

    let is_success = swap
        .is_success()
        .expect("is_success can return error only when swap is not finished");

    let (maker_coin_ticker, maker_coin_platform) = split_coin(&swap_data.maker_coin);
    let (taker_coin_ticker, taker_coin_platform) = split_coin(&swap_data.taker_coin);

    let params = owned_named_params! {
        ":maker_coin": swap_data.maker_coin.clone(),
        ":maker_coin_ticker": maker_coin_ticker,
        ":maker_coin_platform": maker_coin_platform,
        ":taker_coin": swap_data.taker_coin.clone(),
        ":taker_coin_ticker": taker_coin_ticker,
        ":taker_coin_platform": taker_coin_platform,
        ":uuid": swap.uuid.to_string(),
        ":started_at": swap_data.started_at.to_string(),
        ":finished_at": finished_at,
        ":maker_amount": swap_data.maker_amount.to_string(),
        ":taker_amount": swap_data.taker_amount.to_string(),
        ":is_success": (is_success as u32).to_string(),
        ":maker_coin_usd_price": swap.maker_coin_usd_price.as_ref().map(|p| p.to_string()),
        ":taker_coin_usd_price": swap.taker_coin_usd_price.as_ref().map(|p| p.to_string()),
        ":maker_pubkey": pubkeys.maker,
        ":taker_pubkey": pubkeys.taker,
    };
    Some((INSERT_STATS_SWAP, params))
}

fn insert_stats_taker_swap_sql_init(swap: &TakerSavedSwap) -> Option<(&'static str, Vec<String>)> {
    let swap_data = match swap.swap_data() {
        Ok(d) => d,
        Err(e) => {
            error!("Error {} on getting swap {} data", e, swap.uuid);
            return None;
        },
    };
    let finished_at = match swap.finished_at() {
        Ok(t) => t.to_string(),
        Err(e) => {
            error!("Error {} on getting swap {} finished_at", e, swap.uuid);
            return None;
        },
    };
    let is_success = swap
        .is_success()
        .expect("is_success can return error only when swap is not finished");

    let params = vec![
        swap_data.maker_coin.clone(),
        swap_data.taker_coin.clone(),
        swap.uuid.to_string(),
        swap_data.started_at.to_string(),
        finished_at,
        swap_data.maker_amount.to_string(),
        swap_data.taker_amount.to_string(),
        (is_success as u32).to_string(),
    ];
    Some((INSERT_STATS_SWAP_ON_INIT, params))
}

pub fn add_swap_to_index(conn: &Connection, swap: &SavedSwap) {
    let params = vec![swap.uuid().to_string()];
    let query_row = conn.query_row(SELECT_ID_BY_UUID, &params, |row| row.get::<_, i64>(0));
    match query_row.optional() {
        // swap is not indexed yet, go ahead
        Ok(None) => (),
        // swap is already indexed
        Ok(Some(_)) => return,
        Err(e) => {
            error!("Error {} on query {} with params {:?}", e, SELECT_ID_BY_UUID, params);
            return;
        },
    };

    let sql_with_params = match swap {
        SavedSwap::Maker(maker) => insert_stats_maker_swap_sql(maker),
        SavedSwap::Taker(taker) => insert_stats_taker_swap_sql(taker),
    };

    let (sql, params) = match sql_with_params {
        Some(tuple) => tuple,
        None => return,
    };

    debug!("Executing query {} with params {:?}", sql, params);
    if let Err(e) = conn.execute_named(sql, &params.as_sql_named_params()) {
        error!("Error {} on query {} with params {:?}", e, sql, params);
    };
}

#[test]
fn test_split_coin() {
    let input = "";
    let expected = ("".into(), "".into());
    let actual = split_coin(input);
    assert_eq!(expected, actual);

    let input = "RICK";
    let expected = ("RICK".into(), "".into());
    let actual = split_coin(input);
    assert_eq!(expected, actual);

    let input = "RICK-BEP20";
    let expected = ("RICK".into(), "BEP20".into());
    let actual = split_coin(input);
    assert_eq!(expected, actual);

    let input = "RICK-";
    let expected = ("RICK".into(), "".into());
    let actual = split_coin(input);
    assert_eq!(expected, actual);
}
