//! Métricas por ativo em relação à média de 2 anos.

use std::collections::HashMap;

use chrono::{Months, NaiveDate};

use crate::b3::LiveQuote;
use crate::db::{AssetInfo, Bar};

/// Variação entre dois pregões seguidos acima disso indica provável desdobramento/grupamento.
pub const EVENT_THRESHOLD: f64 = 0.40;
/// Histórico mínimo para entrar no universo.
pub const MIN_SESSIONS: usize = 300;
/// Pregões usados na média de volume.
pub const VOLUME_SESSIONS: usize = 60;
/// Ativo sem negócio há mais que isso (dias corridos) é ignorado.
const STALE_DAYS: i64 = 15;

#[derive(Debug, Clone)]
pub struct Metrics {
    pub ticker: String,
    pub name: String,
    pub spec: String,
    /// Preço ao vivo, ou último fechamento.
    pub price: f64,
    pub live: bool,
    /// Variação do dia em %.
    pub change_pct: f64,
    pub prev_close: f64,
    pub avg2y: f64,
    pub dist_now: f64,
    pub dist_prev: f64,
    pub ret12m: Option<f64>,
    pub ret3m: Option<f64>,
    pub min2y: f64,
    pub max2y: f64,
    pub avg_volume: f64,
    pub sessions: usize,
    /// Datas (`AAAAMMDD`) com variação acima de ±40% na janela de 2 anos.
    pub events: Vec<u32>,
}

impl Metrics {
    pub fn crossed_below(&self) -> bool {
        self.dist_prev > 0.0 && self.dist_now < 0.0
    }

    pub fn in_universe(&self, min_volume: f64) -> bool {
        self.sessions >= MIN_SESSIONS && self.avg_volume > min_volume
    }
}

pub fn to_date(d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt((d / 10000) as i32, (d / 100) % 100, d % 100).unwrap_or_default()
}

pub fn from_date(d: NaiveDate) -> u32 {
    d.format("%Y%m%d").to_string().parse().unwrap_or(0)
}

/// Data da resposta da cotação ao vivo (`AAAAMMDD`).
fn live_date(q: &LiveQuote) -> Option<u32> {
    q.time.get(..10)?.replace('-', "").parse().ok()
}

/// A cotação ao vivo é de um pregão novo, posterior ao último gravado?
///
/// O endpoint devolve a data de hoje mesmo sem pregão (fim de semana, feriado, pré-abertura),
/// repetindo os preços do pregão anterior; nesse caso o OHLC é igual ao do último pregão gravado.
pub fn is_new_session(q: &LiveQuote, last: &Bar) -> bool {
    let same = |a: f64, b: f64| (a - b).abs() < 0.005;
    let repeats_last = same(q.price, last.close) && same(q.open, last.open) && same(q.high, last.high) && same(q.low, last.low);
    live_date(q).is_some_and(|d| d > last.date) && !repeats_last
}

/// Data do último pregão gravado entre todos os ativos.
pub fn ref_date(bars: &HashMap<String, Vec<Bar>>) -> u32 {
    bars.values().filter_map(|b| b.last()).map(|b| b.date).max().unwrap_or(0)
}

/// Início da janela de 2 anos que termina em `ref_date`.
pub fn window_start(ref_date: u32) -> u32 {
    from_date(to_date(ref_date) - Months::new(24))
}

pub fn compute(
    ticker: &str,
    info: Option<&AssetInfo>,
    bars: &[Bar],
    live: Option<&LiveQuote>,
    ref_date: u32,
) -> Option<Metrics> {
    let last = bars.last()?;
    if (to_date(ref_date) - to_date(last.date)).num_days() > STALE_DAYS || bars.len() < 2 {
        return None;
    }

    // "Fechamento anterior" é o último pregão já fechado antes do preço atual.
    let live = live.filter(|q| is_new_session(q, last));
    let (price, prev_idx, change_pct) = match live {
        Some(q) => (q.price, bars.len() - 1, q.change_pct),
        None => {
            let prev = bars[bars.len() - 2].close;
            (last.close, bars.len() - 2, (last.close / prev - 1.0) * 100.0)
        }
    };
    let prev_close = bars[prev_idx].close;

    let start = window_start(ref_date);
    let first = bars.partition_point(|b| b.date <= start);
    let window = &bars[first..];
    if window.is_empty() {
        return None;
    }
    let avg2y = window.iter().map(|b| b.close).sum::<f64>() / window.len() as f64;
    let min2y = window.iter().map(|b| b.close).fold(f64::INFINITY, f64::min);
    let max2y = window.iter().map(|b| b.close).fold(f64::NEG_INFINITY, f64::max);

    let year_ago = from_date(to_date(bars[prev_idx].date) - Months::new(12));
    let ret12m = bars
        .iter()
        .rev()
        .find(|b| b.date <= year_ago)
        .filter(|b| b.date > from_date(to_date(year_ago) - Months::new(1)))
        .map(|b| prev_close / b.close - 1.0);
    let ret3m = prev_idx.checked_sub(63).map(|i| prev_close / bars[i].close - 1.0);

    let vol_bars = &bars[bars.len().saturating_sub(VOLUME_SESSIONS)..];
    let avg_volume = vol_bars.iter().map(|b| b.volume).sum::<f64>() / vol_bars.len() as f64;

    let events = window
        .windows(2)
        .filter(|w| (w[1].close / w[0].close - 1.0).abs() > EVENT_THRESHOLD)
        .map(|w| w[1].date)
        .collect();

    Some(Metrics {
        ticker: ticker.to_string(),
        name: info.map(|i| i.name.clone()).unwrap_or_default(),
        spec: info.map(|i| i.spec.clone()).unwrap_or_default(),
        price,
        live: live.is_some(),
        change_pct,
        prev_close,
        avg2y,
        dist_now: price / avg2y - 1.0,
        dist_prev: prev_close / avg2y - 1.0,
        ret12m,
        ret3m,
        min2y,
        max2y,
        avg_volume,
        sessions: bars.len(),
        events,
    })
}

pub fn compute_all(
    bars: &HashMap<String, Vec<Bar>>,
    assets: &HashMap<String, AssetInfo>,
    live: &HashMap<String, LiveQuote>,
) -> Vec<Metrics> {
    let ref_date = ref_date(bars);
    let mut out: Vec<Metrics> = bars
        .iter()
        .filter_map(|(t, b)| compute(t, assets.get(t), b, live.get(t), ref_date))
        .collect();
    out.sort_by(|a, b| a.ticker.cmp(&b.ticker));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(date: NaiveDate, close: f64) -> Bar {
        Bar { date: from_date(date), open: close, high: close, low: close, close, volume: 20e6 }
    }

    #[test]
    fn detects_cross_below_average() {
        let start = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
        let mut bars: Vec<Bar> = (0..700).map(|i| bar(start + chrono::Days::new(i), 10.0)).collect();
        let n = bars.len();
        bars[n - 2].close = 11.0; // acima da média ontem
        bars[n - 1].close = 9.0; // abaixo hoje
        let ref_date = bars[n - 1].date;
        let m = compute("TEST3", None, &bars, None, ref_date).unwrap();
        assert!(m.crossed_below());
        assert!(m.in_universe(15e6));
        assert!(m.events.is_empty());
    }

    #[test]
    fn ignores_live_quote_repeating_last_session() {
        let start = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
        let bars: Vec<Bar> = (0..700).map(|i| bar(start + chrono::Days::new(i), 10.0)).collect();
        let last = *bars.last().unwrap();
        let next_day = to_date(last.date) + chrono::Days::new(1);
        let time = format!("{} 12:00:00", next_day.format("%Y-%m-%d"));
        let stale = LiveQuote { price: 10.0, open: 10.0, high: 10.0, low: 10.0, change_pct: 0.0, quantity: 0, time: time.clone() };
        let fresh = LiveQuote { price: 9.5, ..stale.clone() };
        assert!(!is_new_session(&stale, &last));
        assert!(is_new_session(&fresh, &last));
        let m = compute("TEST3", None, &bars, Some(&fresh), last.date).unwrap();
        assert!(m.live);
        assert_eq!(m.prev_close, 10.0);
    }

    #[test]
    fn flags_split() {
        let start = NaiveDate::from_ymd_opt(2025, 1, 1).unwrap();
        let mut bars: Vec<Bar> = (0..400).map(|i| bar(start + chrono::Days::new(i), 30.0)).collect();
        for b in &mut bars[200..] {
            b.close = 10.0;
        }
        let ref_date = bars.last().unwrap().date;
        let m = compute("SPLT3", None, &bars, None, ref_date).unwrap();
        assert_eq!(m.events, vec![bars[200].date]);
    }
}
