//! Poll contínuo da cotação ao vivo durante o pregão.
//!
//! O endpoint da B3 responde um ticker por chamada, então um único worker gira pelo universo
//! sem parar, intercalando o ativo aberto no gráfico e o IBOV para que esses fiquem quase em
//! tempo real. Cada cotação é enviada assim que chega.

use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, NaiveTime, Timelike, Weekday};

use crate::b3::{self, LiveQuote, Throttled};

pub const IBOV: &str = "IBOV";
/// Pausa entre chamadas, para não sobrecarregar o endpoint (o SPEC indica ≈0,15 s).
const DELAY: Duration = Duration::from_millis(150);
/// Uma em cada N chamadas vai para o ativo aberto no gráfico.
const FOCUS_EVERY: u64 = 4;
/// Uma em cada N chamadas vai para o IBOV.
const IBOV_EVERY: u64 = 15;
/// Erros seguidos antes de dar uma pausa.
const MAX_ERRORS: u32 = 5;
const ERROR_BACKOFF: Duration = Duration::from_secs(30);
/// Pausa quando a B3 responde 403/429.
const THROTTLE_BACKOFF: Duration = Duration::from_secs(120);
/// Depois desse horário, uma volta inteira sem nenhuma mudança indica que não há pregão.
const NO_TRADES_AFTER: (u32, u32) = (10, 30);

/// O que a interface pede ao worker; lido a cada chamada.
#[derive(Debug, Default, Clone)]
pub struct Control {
    pub enabled: bool,
    pub tickers: Vec<String>,
    pub focus: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Off,
    /// Rodando; `cycle` é quanto levou a última volta completa pelo universo.
    Running { cycle: Option<Duration> },
    /// Parado até o próximo pregão.
    Idle(String),
    /// Pausado depois de erro.
    Backoff { reason: String, until: Instant },
}

pub enum Event {
    Quote(String, LiveQuote),
    State(State),
}

/// Janela em que vale a pena consultar: dias úteis, da pré-abertura ao fim do after-market.
pub fn market_window(now: DateTime<FixedOffset>) -> bool {
    let weekday = !matches!(now.weekday(), Weekday::Sat | Weekday::Sun);
    let t = now.time();
    weekday && t >= NaiveTime::from_hms_opt(9, 45, 0).unwrap() && t <= NaiveTime::from_hms_opt(18, 30, 0).unwrap()
}

/// Resposta atrasada: do mesmo dia, mas com menos negócios que uma já recebida.
///
/// A B3 alterna entre servidores dessincronizados; sem isso o preço fica pulando entre o
/// valor atual e um antigo.
fn is_behind(new: &LiveQuote, old: &LiveQuote) -> bool {
    new.time.get(..10) == old.time.get(..10) && new.quantity < old.quantity
}

/// Escolhe o próximo ticker: ativo aberto e IBOV intercalados, o resto em rodízio.
#[derive(Default)]
struct Scheduler {
    slot: u64,
    pos: usize,
}

impl Scheduler {
    /// Devolve o ticker e se ele começa uma nova volta pelo universo.
    fn next(&mut self, tickers: &[String], focus: Option<&str>) -> (String, bool) {
        self.slot += 1;
        if self.slot.is_multiple_of(IBOV_EVERY) {
            return (IBOV.to_string(), false);
        }
        if let Some(f) = focus
            && self.slot.is_multiple_of(FOCUS_EVERY)
        {
            return (f.to_string(), false);
        }
        let wrapped = self.pos >= tickers.len();
        if wrapped {
            self.pos = 0;
        }
        self.pos += 1;
        (tickers[self.pos - 1].clone(), wrapped)
    }
}

pub fn spawn(control: Arc<Mutex<Control>>, tx: Sender<Event>, wake: impl Fn() + Send + 'static) {
    std::thread::spawn(move || {
        let send = |e: Event| {
            let _ = tx.send(e);
            wake();
        };
        let mut last_state: Option<State> = None;
        // Running com duração é reenviado a cada volta; os outros estados só quando mudam.
        let set_state = |last_state: &mut Option<State>, s: State| {
            if matches!(s, State::Running { cycle: Some(_) }) || last_state.as_ref() != Some(&s) {
                *last_state = Some(s.clone());
                send(Event::State(s));
            }
        };

        let client = loop {
            match b3::client() {
                Ok(c) => break c,
                Err(e) => {
                    set_state(&mut last_state, State::Backoff { reason: e.to_string(), until: Instant::now() + ERROR_BACKOFF });
                    std::thread::sleep(ERROR_BACKOFF);
                }
            }
        };

        let mut sched = Scheduler::default();
        let mut last: HashMap<String, LiveQuote> = HashMap::new();
        let mut errors = 0;
        let mut cycle_start = Instant::now();
        let mut cycle_changed = false;
        // A primeira volta só estabelece a base de comparação para detectar "sem negócios".
        let mut full_cycles = 0u32;
        // Fora da janela de pregão faz uma única volta (pega o fechamento do dia) e para.
        let mut swept_outside = false;
        let mut idle_until: Option<NaiveDate> = None;

        loop {
            let Control { enabled, tickers, focus } = control.lock().unwrap().clone();
            if !enabled || tickers.is_empty() {
                set_state(&mut last_state, State::Off);
                // Ao religar fora do pregão, faz de novo a volta única.
                swept_outside = false;
                std::thread::sleep(Duration::from_millis(300));
                continue;
            }

            let now = b3::brt_now();
            if idle_until.is_some_and(|d| d >= now.date_naive()) {
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
            idle_until = None;
            let open = market_window(now);
            if open {
                swept_outside = false;
            } else if swept_outside {
                set_state(&mut last_state, State::Idle("Pregão fechado".into()));
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }

            let (ticker, wrapped) = sched.next(&tickers, focus.as_deref());
            if wrapped {
                let after_open = (now.hour(), now.minute()) >= NO_TRADES_AFTER;
                full_cycles += 1;
                if !open {
                    swept_outside = true;
                    continue;
                } else if !cycle_changed && after_open && full_cycles >= 2 {
                    idle_until = Some(now.date_naive());
                    set_state(&mut last_state, State::Idle("Sem negócios novos: pregão encerrado ou feriado".into()));
                    full_cycles = 0;
                    continue;
                }
                set_state(&mut last_state, State::Running { cycle: Some(cycle_start.elapsed()) });
                cycle_start = Instant::now();
                cycle_changed = false;
            } else if !matches!(last_state, Some(State::Running { .. })) {
                set_state(&mut last_state, State::Running { cycle: None });
            }

            match b3::live_quote(&client, &ticker) {
                Ok(q) if last.get(&ticker).is_some_and(|old| is_behind(&q, old)) => {
                    errors = 0;
                }
                Ok(q) => {
                    errors = 0;
                    if last.get(&ticker).is_none_or(|old| !old.same_prices(&q) || old.quantity != q.quantity) {
                        if ticker != IBOV {
                            cycle_changed |= last.contains_key(&ticker);
                        }
                        last.insert(ticker.clone(), q.clone());
                    }
                    send(Event::Quote(ticker, q));
                }
                Err(e) if e.is::<Throttled>() => {
                    set_state(&mut last_state, State::Backoff { reason: e.to_string(), until: Instant::now() + THROTTLE_BACKOFF });
                    std::thread::sleep(THROTTLE_BACKOFF);
                }
                Err(e) => {
                    errors += 1;
                    if errors >= MAX_ERRORS {
                        errors = 0;
                        set_state(&mut last_state, State::Backoff { reason: format!("{e:#}"), until: Instant::now() + ERROR_BACKOFF });
                        std::thread::sleep(ERROR_BACKOFF);
                    }
                }
            }
            std::thread::sleep(DELAY);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brt(s: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_str(&format!("{s} -0300"), "%Y-%m-%d %H:%M %z").unwrap()
    }

    fn quote(time: &str, price: f64, quantity: i64) -> LiveQuote {
        LiveQuote { price, open: price, high: price, low: price, change_pct: 0.0, quantity, time: time.into() }
    }

    #[test]
    fn discards_lagging_server() {
        // Respostas reais da LJQQ3 alternando entre dois servidores.
        let ahead = quote("2026-10-26 18:23:16", 1.09, 13894);
        let behind = quote("2026-10-26 18:23:17", 1.15, 7957);
        assert!(is_behind(&behind, &ahead));
        assert!(!is_behind(&ahead, &behind));
        // Dia seguinte começa do zero.
        assert!(!is_behind(&quote("2026-10-27 10:00:01", 1.10, 10), &ahead));
    }

    #[test]
    fn market_window_hours() {
        assert!(market_window(brt("2026-10-26 10:00"))); // segunda
        assert!(market_window(brt("2026-10-26 18:30")));
        assert!(!market_window(brt("2026-10-26 09:30")));
        assert!(!market_window(brt("2026-10-26 19:00")));
        assert!(!market_window(brt("2026-10-24 12:00"))); // sábado
    }

    #[test]
    fn scheduler_interleaves_focus_and_ibov() {
        let tickers: Vec<String> = ["A", "B", "C"].iter().map(|s| s.to_string()).collect();
        let mut s = Scheduler::default();
        let picks: Vec<(String, bool)> = (0..60).map(|_| s.next(&tickers, Some("F"))).collect();
        let count = |t: &str| picks.iter().filter(|(p, _)| p == t).count();
        assert_eq!(count(IBOV), 4);
        assert_eq!(count("F"), 14); // múltiplos de 4 até 60, menos o 60 (que é do IBOV)
        assert_eq!(count("A") + count("B") + count("C"), 42);
        // A primeira volta não conta como "nova volta"; as seguintes começam sempre em A.
        let wraps: Vec<&String> = picks.iter().filter(|(_, w)| *w).map(|(t, _)| t).collect();
        assert!(!picks[0].1);
        assert!(wraps.iter().all(|t| *t == "A"));
    }
}
