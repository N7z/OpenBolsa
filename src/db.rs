//! Armazenamento local das cotações em SQLite.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};

use crate::cotahist::Quote;

/// Um pregão de um ativo.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // open/high/low ficam para o gráfico de candle
pub struct Bar {
    pub date: u32,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

#[derive(Debug, Clone, Default)]
pub struct AssetInfo {
    pub name: String,
    pub spec: String,
}

/// `$XDG_DATA_HOME/openbolsa/openbolsa.db` (ou `~/.local/share/...`).
pub fn default_path() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("openbolsa").join("openbolsa.db")
}

pub fn open(path: &Path) -> anyhow::Result<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         CREATE TABLE IF NOT EXISTS quotes (
             ticker TEXT NOT NULL,
             date   INTEGER NOT NULL,
             open   REAL NOT NULL,
             high   REAL NOT NULL,
             low    REAL NOT NULL,
             close  REAL NOT NULL,
             trades INTEGER NOT NULL,
             quantity INTEGER NOT NULL,
             volume REAL NOT NULL,
             PRIMARY KEY (ticker, date)
         ) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS assets (
             ticker TEXT PRIMARY KEY,
             name   TEXT NOT NULL,
             spec   TEXT NOT NULL,
             isin   TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS loaded_files (
             name TEXT PRIMARY KEY,
             loaded_at TEXT NOT NULL
         );",
    )?;
    Ok(conn)
}

pub fn insert_quotes(conn: &mut Connection, quotes: &[Quote]) -> anyhow::Result<()> {
    let tx = conn.transaction()?;
    {
        let mut q = tx.prepare(
            "INSERT OR REPLACE INTO quotes
             (ticker, date, open, high, low, close, trades, quantity, volume)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;
        let mut a = tx.prepare("INSERT OR REPLACE INTO assets (ticker, name, spec, isin) VALUES (?1, ?2, ?3, ?4)")?;
        // Nome e especificação do pregão mais recente (a especificação muda, ex.: `ON EJ NM` no ex-juros).
        let mut latest: HashMap<&str, &Quote> = HashMap::new();
        for x in quotes {
            q.execute(params![x.ticker, x.date, x.open, x.high, x.low, x.close, x.trades, x.quantity, x.volume])?;
            let e = latest.entry(&x.ticker).or_insert(x);
            if x.date >= e.date {
                *e = x;
            }
        }
        for x in latest.values() {
            a.execute(params![x.ticker, x.name, x.spec, x.isin])?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub fn is_loaded(conn: &Connection, name: &str) -> anyhow::Result<bool> {
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM loaded_files WHERE name = ?1", [name], |r| r.get(0))?;
    Ok(n > 0)
}

pub fn mark_loaded(conn: &Connection, name: &str) -> anyhow::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO loaded_files (name, loaded_at) VALUES (?1, datetime('now'))",
        [name],
    )?;
    Ok(())
}

/// Data do último pregão gravado (`AAAAMMDD`).
pub fn last_date(conn: &Connection) -> anyhow::Result<Option<u32>> {
    Ok(conn.query_row("SELECT MAX(date) FROM quotes", [], |r| r.get(0))?)
}

/// Último pregão gravado dentro do ano.
pub fn last_date_in(conn: &Connection, year: i32) -> anyhow::Result<Option<u32>> {
    let (from, to) = (year as u32 * 10000, year as u32 * 10000 + 9999);
    Ok(conn.query_row("SELECT MAX(date) FROM quotes WHERE date BETWEEN ?1 AND ?2", [from, to], |r| r.get(0))?)
}

/// Todo o histórico gravado de um ativo, em ordem de data.
pub fn load_ticker(conn: &Connection, ticker: &str) -> anyhow::Result<Vec<Bar>> {
    let mut stmt = conn.prepare(
        "SELECT date, open, high, low, close, volume FROM quotes WHERE ticker = ?1 ORDER BY date",
    )?;
    let rows = stmt.query_map([ticker], |r| {
        Ok(Bar { date: r.get(0)?, open: r.get(1)?, high: r.get(2)?, low: r.get(3)?, close: r.get(4)?, volume: r.get(5)? })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Carrega todo o histórico a partir de `since` (`AAAAMMDD`), agrupado por ticker e em ordem de data.
pub fn load_bars(conn: &Connection, since: u32) -> anyhow::Result<HashMap<String, Vec<Bar>>> {
    let mut stmt = conn.prepare(
        "SELECT ticker, date, open, high, low, close, volume FROM quotes
         WHERE date >= ?1 ORDER BY ticker, date",
    )?;
    let mut out: HashMap<String, Vec<Bar>> = HashMap::new();
    let rows = stmt.query_map([since], |r| {
        Ok((
            r.get::<_, String>(0)?,
            Bar {
                date: r.get(1)?,
                open: r.get(2)?,
                high: r.get(3)?,
                low: r.get(4)?,
                close: r.get(5)?,
                volume: r.get(6)?,
            },
        ))
    })?;
    for row in rows {
        let (ticker, bar) = row?;
        out.entry(ticker).or_default().push(bar);
    }
    Ok(out)
}

pub fn load_assets(conn: &Connection) -> anyhow::Result<HashMap<String, AssetInfo>> {
    let mut stmt = conn.prepare("SELECT ticker, name, spec FROM assets")?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, AssetInfo { name: r.get(1)?, spec: r.get(2)? }))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}
