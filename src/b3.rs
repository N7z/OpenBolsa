//! Acesso aos dados públicos da B3: arquivos COTAHIST e cotação ao vivo.

use std::time::Duration;

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, Utc, Weekday};
use rusqlite::Connection;

use crate::cotahist::Quote;
use crate::{cotahist, db, mirror};

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:144.0) Gecko/20100101 Firefox/144.0";
const HIST_URL: &str = "https://bvmf.bmfbovespa.com.br/InstDados/SerHist";
const LIVE_URL: &str = "https://cotacao.b3.com.br/mds/api/v1/instrumentQuotation";

/// Primeiro ano com COTAHIST publicado pela B3.
pub const FIRST_YEAR: i32 = 1986;
/// Se o último pregão gravado for mais antigo que isso, baixa de novo o anual em vez de dia a dia.
const MAX_DAILY_GAP_DAYS: i64 = 30;

pub fn client() -> anyhow::Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(600))
        .build()?)
}

/// Baixa um arquivo; `None` se a B3 responder 404 (ainda não publicado ou dia sem pregão).
fn download(client: &reqwest::blocking::Client, url: &str) -> anyhow::Result<Option<Vec<u8>>> {
    let resp = client.get(url).send()?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let resp = resp.error_for_status()?;
    Ok(Some(resp.bytes()?.to_vec()))
}

/// Baixa e lê um arquivo do COTAHIST (`A2026`, `D23102026`); `None` se ainda não existir na B3.
pub fn fetch_cotahist(
    client: &reqwest::blocking::Client,
    name: &str,
    progress: &dyn Fn(String),
) -> anyhow::Result<Option<Vec<Quote>>> {
    let url = format!("{HIST_URL}/COTAHIST_{name}.ZIP");
    progress(format!("Baixando COTAHIST_{name}.ZIP…"));
    let Some(bytes) = download(client, &url)? else {
        return Ok(None);
    };
    progress(format!("Processando COTAHIST_{name} ({:.0} MB)…", bytes.len() as f64 / 1e6));
    Ok(Some(cotahist::parse_zip(&bytes)?))
}

/// Baixa e grava um arquivo; `false` se ainda não existir na B3.
fn load_file(
    client: &reqwest::blocking::Client,
    conn: &mut Connection,
    name: &str,
    progress: &dyn Fn(String),
) -> anyhow::Result<bool> {
    let Some(quotes) = fetch_cotahist(client, name, progress)? else {
        return Ok(false);
    };
    db::insert_quotes(conn, &quotes)?;
    db::mark_loaded(conn, name)?;
    Ok(true)
}

pub fn to_date(d: u32) -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt((d / 10000) as i32, (d / 100) % 100, d % 100)
}

/// Garante os anuais e completa com os arquivos diários que faltam. Devolve quantos arquivos foram gravados.
///
/// Do mirror vem o histórico inteiro, desde 1986. Se ele falhar, a B3 direto só dá os últimos 3 anos
/// (2 de média + 1 de folga para o retorno de 12 meses): são ~90 MB por ano.
pub fn sync(conn: &mut Connection, progress: &dyn Fn(String)) -> anyhow::Result<usize> {
    let client = client()?;
    let today = brt_now().date_naive();
    let year = today.year();
    let mut loaded = 0;

    match mirror::sync(&client, conn, FIRST_YEAR, progress) {
        Ok(n) => loaded += n,
        Err(e) => {
            progress(format!("Mirror indisponível ({e:#}), baixando da B3…"));
            for y in year - 2..=year {
                let name = format!("A{y}");
                if !db::is_loaded(conn, &name)?
                    && db::last_date_in(conn, y)?.is_none()
                    && load_file(&client, conn, &name, progress)?
                {
                    loaded += 1;
                }
            }
        }
    }

    let Some(last) = db::last_date(conn)?.and_then(to_date) else {
        return Ok(loaded);
    };

    // Muito tempo sem abrir: o anual do ano corrente sai mais barato que dezenas de diários.
    if (today - last).num_days() > MAX_DAILY_GAP_DAYS {
        for y in last.year()..=year {
            if load_file(&client, conn, &format!("A{y}"), progress)? {
                loaded += 1;
            }
        }
    }

    let mut day = db::last_date(conn)?.and_then(to_date).unwrap_or(last).succ_opt().unwrap();
    while day <= today {
        if !matches!(day.weekday(), Weekday::Sat | Weekday::Sun) {
            let name = format!("D{}", day.format("%d%m%Y"));
            if !db::is_loaded(conn, &name)? {
                if load_file(&client, conn, &name, progress)? {
                    loaded += 1;
                } else {
                    progress(format!("Pregão de {} ainda não publicado", day.format("%d/%m/%Y")));
                }
            }
        }
        day = day.succ_opt().unwrap();
    }
    Ok(loaded)
}

/// Horário de Brasília (sem horário de verão desde 2019).
pub fn brt_now() -> DateTime<FixedOffset> {
    Utc::now().with_timezone(&FixedOffset::west_opt(3 * 3600).unwrap())
}

#[derive(Debug, Clone, PartialEq)]
pub struct LiveQuote {
    pub price: f64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    /// Variação do dia em %.
    pub change_pct: f64,
    /// Quantidade negociada no dia (`ttlQty`). Só cresce ao longo do pregão, então serve
    /// para descartar respostas atrasadas: a B3 alterna entre servidores dessincronizados.
    pub quantity: i64,
    /// Horário da resposta da B3, ex.: `2026-10-26 16:37:06`. É a hora do servidor,
    /// não a do último negócio: em dia sem pregão vem a data de hoje com os preços do pregão anterior.
    pub time: String,
}

impl LiveQuote {
    /// Mesmos preços, ignorando o horário.
    pub fn same_prices(&self, other: &LiveQuote) -> bool {
        self.price == other.price && self.open == other.open && self.high == other.high && self.low == other.low
    }
}

/// Erro HTTP que pede para esperar mais antes de tentar de novo (bloqueio ou limite).
#[derive(Debug)]
pub struct Throttled(pub reqwest::StatusCode);

impl std::fmt::Display for Throttled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "B3 respondeu {}", self.0)
    }
}

impl std::error::Error for Throttled {}

/// Cotação durante o pregão (endpoint não documentado do site da B3).
pub fn live_quote(client: &reqwest::blocking::Client, ticker: &str) -> anyhow::Result<LiveQuote> {
    let resp = client.get(format!("{LIVE_URL}/{ticker}")).send()?;
    let status = resp.status();
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status == reqwest::StatusCode::FORBIDDEN {
        return Err(Throttled(status).into());
    }
    parse_live(&resp.error_for_status()?.json()?).ok_or_else(|| anyhow::anyhow!("{ticker}: cotação indisponível"))
}

fn parse_live(v: &serde_json::Value) -> Option<LiveQuote> {
    if v["BizSts"]["cd"].as_str() != Some("OK") {
        return None;
    }
    let qtn = &v["Trad"][0]["scty"]["SctyQtn"];
    let price = qtn["curPrc"].as_f64()?;
    let or_price = |k: &str| qtn[k].as_f64().unwrap_or(price);
    Some(LiveQuote {
        price,
        open: or_price("opngPric"),
        high: or_price("maxPric"),
        low: or_price("minPric"),
        change_pct: qtn["prcFlcn"].as_f64().unwrap_or(0.0),
        quantity: v["Trad"][0]["ttlQty"].as_i64().unwrap_or(0),
        time: v["Msg"]["dtTm"].as_str()?.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_live_quote() {
        let v = serde_json::json!({
            "BizSts": {"cd": "OK"},
            "Msg": {"dtTm": "2026-10-26 16:37:06"},
            "Trad": [{"scty": {"SctyQtn": {
                "opngPric": 16.48, "minPric": 16.48, "maxPric": 18.12,
                "avrgPric": 17.327, "curPrc": 17.8, "prcFlcn": -27.9935275
            }, "symb": "BBAS3"}, "ttlQty": 280617}]
        });
        let q = parse_live(&v).unwrap();
        assert_eq!(q.price, 17.8);
        assert_eq!(q.high, 18.12);
        assert_eq!(q.time, "2026-10-26 16:37:06");
        assert_eq!(q.quantity, 280617);
    }

    #[test]
    fn rejects_unavailable_quote() {
        let v = serde_json::json!({"BizSts": {"cd": "NOK", "desc": "Quotation not available."}, "Msg": {"dtTm": "2026-10-26 17:17:27"}});
        assert!(parse_live(&v).is_none());
    }
}
