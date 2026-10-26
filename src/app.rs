//! Interface: lista de ativos e gráfico por ativo.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{Datelike, Timelike, Weekday};
use eframe::egui::{self, Color32, RichText};
use egui_extras::{Column, TableBuilder};
use egui_plot::{BoxElem, BoxPlot, BoxSpread, HLine, Legend, Line, LineStyle, Plot, Points, VLine};
use serde::{Deserialize, Serialize};

use crate::analysis::{self, Metrics};
use crate::b3::{self, LiveQuote};
use crate::db::{self, AssetInfo, Bar};
use crate::live::{self, IBOV};
use crate::theme::{self, ACCENT, AMBER, GREEN, RED, TEXT, TEXT_STRONG, TEXT_WEAK};

/// Quanto tempo o preço fica destacado depois de mudar.
const FLASH_SECS: f64 = 1.5;
/// O arquivo diário da B3 sai à noite; a partir dessa hora tenta baixá-lo.
const DAILY_FILE_HOUR: u32 = 19;
const SYNC_RETRY: Duration = Duration::from_secs(30 * 60);

enum Msg {
    Status(String),
    Loaded(HashMap<String, Vec<Bar>>, HashMap<String, AssetInfo>),
    Error(String),
    Done,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum SortCol {
    Ticker,
    Name,
    Price,
    Change,
    Avg,
    DistNow,
    DistPrev,
    Ret12,
    Ret3,
    Min,
    Max,
    Volume,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum ChartRange {
    M6,
    Y1,
    Y2,
    All,
}

impl ChartRange {
    const ALL: [(ChartRange, &str); 4] =
        [(ChartRange::M6, "6M"), (ChartRange::Y1, "1A"), (ChartRange::Y2, "2A"), (ChartRange::All, "Tudo")];

    fn months(self) -> Option<u32> {
        match self {
            ChartRange::M6 => Some(6),
            ChartRange::Y1 => Some(12),
            ChartRange::Y2 => Some(24),
            ChartRange::All => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum ChartKind {
    Line,
    Candle,
}

/// Preferências salvas entre execuções.
#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    only_universe: bool,
    min_volume_mi: f64,
    only_crossed: bool,
    only_below: bool,
    hide_events: bool,
    sort: SortCol,
    sort_desc: bool,
    chart_range: ChartRange,
    chart_kind: ChartKind,
    /// Sempre aparecem na lista (mesmo abaixo da liquidez mínima) e sempre são consultados ao vivo.
    favorites: BTreeSet<String>,
    only_favorites: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            only_universe: true,
            min_volume_mi: 15.0,
            only_crossed: false,
            only_below: false,
            hide_events: false,
            sort: SortCol::DistNow,
            sort_desc: false,
            chart_range: ChartRange::Y2,
            chart_kind: ChartKind::Line,
            favorites: BTreeSet::new(),
            only_favorites: false,
        }
    }
}

pub struct App {
    s: Settings,
    db_path: PathBuf,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    loading: bool,
    last_sync_try: Option<Instant>,
    status: String,

    bars: HashMap<String, Vec<Bar>>,
    assets: HashMap<String, AssetInfo>,
    ref_date: u32,
    metrics: Vec<Metrics>,

    live_ctl: Arc<Mutex<live::Control>>,
    live_rx: Receiver<live::Event>,
    live_state: live::State,
    live: HashMap<String, LiveQuote>,
    ibov: Option<LiveQuote>,
    /// Ticker → (instante em `ctx.input.time`, subiu?).
    flashes: HashMap<String, (f64, bool)>,

    selected: Option<String>,
    search: String,
    /// Histórico completo do ativo aberto, para o gráfico "Tudo" (`bars` só tem ~40 meses).
    history: Option<(String, Vec<Bar>)>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        let (tx, rx) = channel();
        let (live_tx, live_rx) = channel();
        let live_ctl = Arc::new(Mutex::new(live::Control::default()));
        let ctx = cc.egui_ctx.clone();
        live::spawn(live_ctl.clone(), live_tx, move || ctx.request_repaint());

        let mut app = Self {
            s: cc.storage.and_then(|st| eframe::get_value(st, eframe::APP_KEY)).unwrap_or_default(),
            db_path: db::default_path(),
            tx,
            rx,
            loading: false,
            last_sync_try: None,
            status: String::new(),
            bars: HashMap::new(),
            assets: HashMap::new(),
            ref_date: 0,
            metrics: Vec::new(),
            live_ctl,
            live_rx,
            live_state: live::State::Off,
            live: HashMap::new(),
            ibov: None,
            flashes: HashMap::new(),
            selected: std::env::args().skip_while(|a| a != "--ticker").nth(1).map(|t| t.to_uppercase()),
            search: String::new(),
            history: None,
        };
        // Na abertura: mostra o que já está no banco e baixa só os dias que faltam.
        app.spawn_load(cc.egui_ctx.clone(), true);
        app
    }

    /// Lê o banco em segundo plano; com `sync`, depois baixa o que falta da B3 e relê se veio algo.
    fn spawn_load(&mut self, ctx: egui::Context, sync: bool) {
        self.loading = true;
        if sync {
            self.last_sync_try = Some(Instant::now());
        }
        let tx = self.tx.clone();
        let path = self.db_path.clone();
        std::thread::spawn(move || {
            let send = |m: Msg| {
                let _ = tx.send(m);
                ctx.request_repaint();
            };
            let load = |conn: &rusqlite::Connection| -> anyhow::Result<()> {
                send(Msg::Status("Lendo banco local…".into()));
                let since = analysis::from_date(b3::brt_now().date_naive() - chrono::Months::new(40));
                send(Msg::Loaded(db::load_bars(conn, since)?, db::load_assets(conn)?));
                Ok(())
            };
            let result = (|| -> anyhow::Result<()> {
                let mut conn = db::open(&path)?;
                load(&conn)?;
                if sync {
                    let loaded = b3::sync(&mut conn, &|s| send(Msg::Status(s)))?;
                    if loaded > 0 {
                        load(&conn)?;
                    }
                    send(Msg::Status(String::new()));
                }
                Ok(())
            })();
            if let Err(e) = result {
                send(Msg::Error(format!("{e:#}")));
            }
            send(Msg::Done);
        });
    }

    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Status(s) => self.status = s,
                Msg::Loaded(bars, assets) => {
                    self.bars = bars;
                    self.assets = assets;
                    self.history = None;
                    self.ref_date = analysis::ref_date(&self.bars);
                    self.metrics = analysis::compute_all(&self.bars, &self.assets, &self.live);
                    self.status = if self.bars.is_empty() {
                        "Banco vazio: baixando o histórico…".into()
                    } else {
                        String::new()
                    };
                }
                Msg::Error(e) => self.status = format!("Erro: {e}"),
                Msg::Done => self.loading = false,
            }
        }

        let now = ctx.input(|i| i.time);
        while let Ok(ev) = self.live_rx.try_recv() {
            match ev {
                live::Event::State(s) => self.live_state = s,
                live::Event::Quote(t, q) => {
                    let old = if t == IBOV { self.ibov.replace(q.clone()) } else { self.live.insert(t.clone(), q.clone()) };
                    if let Some(old) = &old
                        && old.price != q.price
                    {
                        self.flashes.insert(t.clone(), (now, q.price > old.price));
                    }
                    if t != IBOV && old.is_none_or(|o| !o.same_prices(&q)) {
                        self.recompute(&t);
                    }
                }
            }
        }
        self.flashes.retain(|_, (t, _)| now - *t < FLASH_SECS);
        if !self.flashes.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    /// Recalcula as métricas de um ativo depois de uma cotação nova.
    fn recompute(&mut self, ticker: &str) {
        let Some(bars) = self.bars.get(ticker) else { return };
        let m = analysis::compute(ticker, self.assets.get(ticker), bars, self.live.get(ticker), self.ref_date);
        let pos = self.metrics.iter().position(|x| x.ticker == ticker);
        match (pos, m) {
            (Some(i), Some(m)) => self.metrics[i] = m,
            (None, Some(m)) => self.metrics.push(m),
            (Some(i), None) => {
                self.metrics.remove(i);
            }
            (None, None) => {}
        }
    }

    /// Passa ao worker o universo atual, o ativo aberto e se deve rodar.
    fn update_live_control(&self) {
        let min_volume = self.s.min_volume_mi * 1e6;
        let mut tickers: Vec<String> =
            self.metrics.iter().filter(|m| m.in_universe(min_volume) || self.s.favorites.contains(&m.ticker)).map(|m| m.ticker.clone()).collect();
        tickers.sort();
        let mut ctl = self.live_ctl.lock().unwrap();
        ctl.enabled = !self.bars.is_empty();
        ctl.focus = self.selected.clone();
        if ctl.tickers != tickers {
            ctl.tickers = tickers;
        }
    }

    /// À noite, tenta baixar o arquivo diário do pregão que acabou (a cada 30 min até sair).
    fn maybe_sync(&mut self, ctx: &egui::Context) {
        if self.loading || self.bars.is_empty() || self.last_sync_try.is_some_and(|t| t.elapsed() < SYNC_RETRY) {
            return;
        }
        let now = b3::brt_now();
        let mut expected = now.date_naive();
        if now.hour() < DAILY_FILE_HOUR {
            expected = expected.pred_opt().unwrap();
        }
        while matches!(expected.weekday(), Weekday::Sat | Weekday::Sun) {
            expected = expected.pred_opt().unwrap();
        }
        if self.ref_date < analysis::from_date(expected) {
            self.spawn_load(ctx.clone(), true);
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("OpenBolsa").size(16.0).strong().color(TEXT_STRONG));
            ui.add_space(6.0);
            ui.separator();
            ui.add_enabled_ui(!self.loading, |ui| {
                if ui
                    .button("Sincronizar B3")
                    .on_hover_text("Baixa o histórico que falta: do Mirror OpenBolsa e, se ele falhar, da B3 direto")
                    .clicked()
                {
                    self.spawn_load(ui.ctx().clone(), true);
                }
            });
            self.live_status(ui);
            if self.loading {
                ui.spinner();
            }
            if !self.status.is_empty() {
                let color = if self.status.starts_with("Erro") { RED } else { TEXT_WEAK };
                ui.label(RichText::new(&self.status).color(color));
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(q) = &self.ibov {
                    let flash = self.flash_color(ui, IBOV);
                    pct_label(ui, Some(q.change_pct / 100.0));
                    let mut text = RichText::new(fmt_num(q.price, 0)).strong().color(TEXT_STRONG);
                    if let Some(c) = flash {
                        text = text.background_color(c);
                    }
                    ui.label(text);
                    ui.label(RichText::new("IBOV").small().color(TEXT_WEAK));
                    ui.separator();
                }
                if self.ref_date > 0 {
                    ui.label(RichText::new(format!("Fechamento até {}", fmt_date(self.ref_date))).small().color(TEXT_WEAK));
                }
            });
        });
    }

    /// Só aparece quando o ao vivo está com problema.
    fn live_status(&self, ui: &mut egui::Ui) {
        match &self.live_state {
            live::State::Off | live::State::Running { .. } | live::State::Idle(_) => {}
            live::State::Backoff { reason, until } => {
                let secs = until.saturating_duration_since(Instant::now()).as_secs();
                ui.colored_label(RED, format!("Ao vivo pausado, nova tentativa em {secs} s")).on_hover_text(reason);
                ui.ctx().request_repaint_after(Duration::from_secs(1));
            }
        }
    }

    /// Cor de fundo do destaque de mudança de preço, esmaecendo com o tempo.
    fn flash_color(&self, ui: &egui::Ui, ticker: &str) -> Option<Color32> {
        let (t, up) = self.flashes.get(ticker)?;
        let age = ui.input(|i| i.time) - t;
        let alpha = (1.0 - age / FLASH_SECS).clamp(0.0, 1.0) as f32;
        Some((if *up { GREEN } else { RED }).gamma_multiply(0.45 * alpha))
    }

    fn filtered(&self) -> Vec<&Metrics> {
        let search = self.search.trim().to_uppercase();
        let min_volume = self.s.min_volume_mi * 1e6;
        let s = &self.s;
        let mut rows: Vec<&Metrics> = self
            .metrics
            .iter()
            .filter(|m| !s.only_universe || m.in_universe(min_volume) || s.favorites.contains(&m.ticker))
            .filter(|m| !s.only_favorites || s.favorites.contains(&m.ticker))
            .filter(|m| !s.only_crossed || m.crossed_below())
            .filter(|m| !s.only_below || m.dist_now < 0.0)
            .filter(|m| !s.hide_events || m.events.is_empty())
            .filter(|m| search.is_empty() || m.ticker.contains(&search) || m.name.to_uppercase().contains(&search))
            .collect();

        let key = |m: &Metrics| -> f64 {
            match s.sort {
                SortCol::Price => m.price,
                SortCol::Change => m.change_pct,
                SortCol::Avg => m.avg2y,
                SortCol::DistNow => m.dist_now,
                SortCol::DistPrev => m.dist_prev,
                SortCol::Ret12 => m.ret12m.unwrap_or(f64::NEG_INFINITY),
                SortCol::Ret3 => m.ret3m.unwrap_or(f64::NEG_INFINITY),
                SortCol::Min => m.min2y,
                SortCol::Max => m.max2y,
                SortCol::Volume => m.avg_volume,
                SortCol::Ticker | SortCol::Name => 0.0,
            }
        };
        rows.sort_by(|a, b| {
            let ord = match s.sort {
                SortCol::Ticker => a.ticker.cmp(&b.ticker),
                SortCol::Name => a.name.cmp(&b.name),
                _ => key(a).total_cmp(&key(b)),
            };
            if s.sort_desc { ord.reverse() } else { ord }
        });
        rows
    }

    fn list_view(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("Buscar ticker ou nome").desired_width(170.0));
            ui.add_space(4.0);
            ui.separator();
            ui.checkbox(&mut self.s.only_universe, "Liquidez mínima").on_hover_text(
                "Volume financeiro médio dos últimos 60 pregões acima do valor e pelo menos 300 pregões de \
                 histórico. É também o universo consultado ao vivo.",
            );
            ui.add(
                egui::DragValue::new(&mut self.s.min_volume_mi)
                    .range(0.0..=1000.0)
                    .speed(0.5)
                    .prefix("R$ ")
                    .suffix(" mi/dia"),
            );
            ui.separator();
            ui.checkbox(&mut self.s.only_crossed, "Cruzou abaixo da média hoje")
                .on_hover_text("Estava acima da média de 2 anos no pregão anterior e está abaixo agora");
            ui.checkbox(&mut self.s.only_below, "Abaixo da média");
            ui.checkbox(&mut self.s.hide_events, "Ocultar sinalizados (±40%)");
            ui.separator();
            ui.checkbox(&mut self.s.only_favorites, format!("Só favoritos ({})", self.s.favorites.len()));
        });
        ui.add_space(6.0);

        let rows = self.filtered();
        let live_count = rows.iter().filter(|m| m.live).count();
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{} ativos", rows.len())).strong());
            if live_count > 0 {
                ui.weak(format!("· {live_count} com preço ao vivo (●)"));
            }
            ui.weak("· linhas destacadas cruzaram abaixo da média hoje");
        });
        ui.add_space(2.0);

        let headers = [
            (SortCol::Ticker, "Ticker", "Código de negociação"),
            (SortCol::Name, "Nome", "Nome resumido (COTAHIST)"),
            (SortCol::Price, "Preço", "Preço ao vivo (●) ou último fechamento"),
            (SortCol::Change, "Dia", "Variação do dia"),
            (SortCol::Avg, "Média", "Média simples dos fechamentos dos últimos 2 anos"),
            (SortCol::DistNow, "Dist.", "Distância da média hoje: preço atual / média 2a − 1"),
            (SortCol::DistPrev, "Ant.", "Distância da média no pregão anterior: fechamento anterior / média 2a − 1"),
            (SortCol::Ret12, "12m", "Retorno de 12 meses até o fechamento anterior"),
            (SortCol::Ret3, "3m", "Retorno dos últimos 63 pregões"),
            (SortCol::Min, "Mín", "Menor fechamento em 2 anos"),
            (SortCol::Max, "Máx", "Maior fechamento em 2 anos"),
            (SortCol::Volume, "Vol.", "Volume financeiro médio dos últimos 60 pregões"),
        ];

        let mut clicked_sort = None;
        let mut open = None;
        let mut toggle_fav = None;
        egui::ScrollArea::horizontal().show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            TableBuilder::new(ui)
                .striped(true)
                .sense(egui::Sense::click())
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::exact(18.0))
                .column(Column::exact(58.0))
                .column(Column::exact(8.0))
                .column(Column::initial(96.0).clip(true))
                .column(Column::auto().at_least(58.0))
                .columns(Column::auto().at_least(44.0), 9)
                .header(26.0, |mut header| {
                    for (i, (col, label, tip)) in headers.into_iter().enumerate() {
                        if i == 0 {
                            header.col(|ui| {
                                ui.label(RichText::new("★").color(AMBER)).on_hover_text(
                                    "Favoritos: sempre aparecem na lista, mesmo abaixo da liquidez mínima, \
                                     e são consultados ao vivo",
                                );
                            });
                        }
                        if i == 1 {
                            header.col(|ui| {
                                ui.label("").on_hover_text("Possível desdobramento/grupamento");
                            });
                        }
                        header.col(|ui| {
                            let active = self.s.sort == col;
                            let arrow = if active { if self.s.sort_desc { " ↓" } else { " ↑" } } else { "" };
                            let text = RichText::new(format!("{label}{arrow}")).small().strong().color(if active {
                                TEXT_STRONG
                            } else {
                                TEXT_WEAK
                            });
                            if ui.add(egui::Button::new(text).frame(false)).on_hover_text(tip).clicked() {
                                clicked_sort = Some(col);
                            }
                        });
                    }
                })
                .body(|body| {
                    body.rows(24.0, rows.len(), |mut row| {
                        let m = rows[row.index()];
                        row.set_selected(m.crossed_below());
                        row.col(|ui| {
                            if star_button(ui, self.s.favorites.contains(&m.ticker)).clicked() {
                                toggle_fav = Some(m.ticker.clone());
                            }
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(&m.ticker).strong().color(TEXT_STRONG));
                        });
                        row.col(|ui| {
                            if !m.events.is_empty() {
                                let dates: Vec<String> = m.events.iter().map(|&d| fmt_date(d)).collect();
                                ui.label(RichText::new("!").color(AMBER).strong()).on_hover_text(format!(
                                    "Variação > ±40% entre pregões em {}.\nProvável desdobramento/grupamento: média distorcida.",
                                    dates.join(", ")
                                ));
                            }
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(&m.name).color(TEXT_WEAK)).on_hover_text(&m.spec);
                        });
                        row.col(|ui| {
                            if let Some(c) = self.flash_color(ui, &m.ticker) {
                                ui.painter().rect_filled(ui.max_rect(), 2.0, c);
                            }
                            ui.label(fmt_num(m.price, 2));
                            if m.live {
                                ui.label(RichText::new("●").size(7.0).color(ACCENT)).on_hover_text("Preço ao vivo");
                            }
                        });
                        row.col(|ui| pct_label(ui, Some(m.change_pct / 100.0)));
                        row.col(|ui| {
                            ui.label(fmt_num(m.avg2y, 2));
                        });
                        row.col(|ui| pct_label(ui, Some(m.dist_now)));
                        row.col(|ui| pct_label(ui, Some(m.dist_prev)));
                        row.col(|ui| pct_label(ui, m.ret12m));
                        row.col(|ui| pct_label(ui, m.ret3m));
                        row.col(|ui| {
                            ui.label(fmt_num(m.min2y, 2));
                        });
                        row.col(|ui| {
                            ui.label(fmt_num(m.max2y, 2));
                        });
                        row.col(|ui| {
                            ui.label(fmt_money_short(m.avg_volume));
                        });
                        if row.response().clicked() {
                            open = Some(m.ticker.clone());
                        }
                    });
                });
        });

        if let Some(col) = clicked_sort {
            if self.s.sort == col {
                self.s.sort_desc = !self.s.sort_desc;
            } else {
                self.s.sort = col;
                self.s.sort_desc = matches!(col, SortCol::Volume | SortCol::Ret12 | SortCol::Ret3);
            }
        }
        if let Some(t) = toggle_fav {
            toggle(&mut self.s.favorites, t);
        } else if open.is_some() {
            self.selected = open;
        }
    }

    /// Lê do banco o histórico completo do ativo, se ainda não estiver em memória.
    fn ensure_history(&mut self, ticker: &str) {
        if self.history.as_ref().is_some_and(|(t, _)| t == ticker) {
            return;
        }
        match db::open(&self.db_path).and_then(|conn| db::load_ticker(&conn, ticker)) {
            Ok(h) => self.history = Some((ticker.to_string(), h)),
            Err(e) => self.status = format!("Erro: {e:#}"),
        }
    }

    fn asset_view(&mut self, ui: &mut egui::Ui, ticker: &str) {
        if self.s.chart_range == ChartRange::All && !self.loading {
            self.ensure_history(ticker);
        }
        let Some(bars) = self.bars.get(ticker) else {
            if self.loading {
                ui.spinner();
            } else {
                self.status = format!("{ticker} não encontrado no banco local");
                self.selected = None;
            }
            return;
        };
        let metrics = self.metrics.iter().find(|m| m.ticker == ticker);

        ui.horizontal(|ui| {
            if ui.button("← Lista").on_hover_text("Esc").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.selected = None;
            }
            ui.add_space(6.0);
            ui.label(RichText::new(ticker).heading().strong().color(TEXT_STRONG));
            if star_button(ui, self.s.favorites.contains(ticker)).clicked() {
                toggle(&mut self.s.favorites, ticker.to_string());
            }
            if let Some(info) = self.assets.get(ticker) {
                ui.label(RichText::new(format!("{}  ·  {}", info.name.trim(), info.spec.trim())).color(TEXT_WEAK));
            }
        });
        ui.add_space(8.0);

        if let Some(m) = metrics {
            let price_flash = self.flash_color(ui, ticker);
            theme::card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 28.0;
                    stat(ui, if m.live { "Preço ao vivo" } else { "Último fechamento" }, |ui| {
                        let mut price = RichText::new(fmt_num(m.price, 2)).size(18.0).strong().color(TEXT_STRONG);
                        if let Some(c) = price_flash {
                            price = price.background_color(c);
                        }
                        ui.label(price);
                    });
                    stat(ui, "Dia", |ui| pct_label(ui, Some(m.change_pct / 100.0)));
                    stat(ui, "Fech. anterior", |ui| {
                        ui.label(fmt_num(m.prev_close, 2));
                    });
                    stat(ui, "Média 2a", |ui| {
                        ui.label(fmt_num(m.avg2y, 2));
                    });
                    stat(ui, "Dist. hoje", |ui| pct_label(ui, Some(m.dist_now)));
                    stat(ui, "Dist. anterior", |ui| pct_label(ui, Some(m.dist_prev)));
                    stat(ui, "12m", |ui| pct_label(ui, m.ret12m));
                    stat(ui, "3m", |ui| pct_label(ui, m.ret3m));
                    stat(ui, "Mín / Máx 2a", |ui| {
                        ui.label(format!("{} / {}", fmt_num(m.min2y, 2), fmt_num(m.max2y, 2)));
                    });
                    stat(ui, "Vol. 60d", |ui| {
                        ui.label(fmt_money_short(m.avg_volume));
                    });
                });
            });
            if !m.events.is_empty() {
                ui.add_space(4.0);
                ui.colored_label(
                    AMBER,
                    "⚠ Variação acima de ±40% entre pregões (linhas tracejadas): provável desdobramento ou grupamento. Preços não ajustados; a média está distorcida.",
                );
            }
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            for (r, label) in ChartRange::ALL {
                ui.selectable_value(&mut self.s.chart_range, r, label);
            }
            ui.separator();
            ui.selectable_value(&mut self.s.chart_kind, ChartKind::Line, "Linha");
            ui.selectable_value(&mut self.s.chart_kind, ChartKind::Candle, "Candle");
        });

        let bars = match &self.history {
            Some((t, h)) if self.s.chart_range == ChartRange::All && t == ticker && h.last().map(|b| b.date) == bars.last().map(|b| b.date) => h,
            _ => bars,
        };
        let last = bars.last().map(|b| b.date).unwrap_or(0);
        let from = self
            .s
            .chart_range
            .months()
            .map(|m| analysis::from_date(analysis::to_date(last) - chrono::Months::new(m)))
            .unwrap_or(0);
        // Eixo x por pregão (índice no histórico), sem buracos de fim de semana e feriado.
        let first = bars.partition_point(|b| b.date < from);
        let live_quote = metrics.filter(|m| m.live).and_then(|_| self.live.get(ticker));
        let now_x = bars.len() as f64;
        let dates: Vec<u32> = bars.iter().map(|b| b.date).collect();
        let date_at = move |x: f64| -> Option<String> {
            let i = x.round();
            if i < 0.0 {
                return None;
            }
            match dates.get(i as usize) {
                Some(&d) => Some(fmt_date(d)),
                None if i as usize == dates.len() => Some("Hoje".into()),
                None => None,
            }
        };
        let axis_dates = date_at.clone();

        let mut plot = Plot::new(("chart", ticker, self.s.chart_range))
            .legend(Legend::default())
            .grid_color(theme::BORDER.gamma_multiply(1.6))
            .x_axis_formatter(move |mark, _| {
                // Só rotula posições inteiras (pregões).
                if (mark.value - mark.value.round()).abs() > 0.01 {
                    return String::new();
                }
                axis_dates(mark.value).unwrap_or_default()
            })
            .label_formatter(move |pos| {
                let p = match pos {
                    egui_plot::HoverPosition::NearDataPoint { position, .. } => position,
                    egui_plot::HoverPosition::Elsewhere { position } => position,
                };
                Some(format!("{}\n{}", date_at(p.x).unwrap_or_default(), fmt_num(p.y, 2)))
            });
        if let Some(q) = live_quote {
            plot = plot.include_x(now_x + 2.0).include_y(q.low).include_y(q.high);
        }
        let kind = self.s.chart_kind;
        plot.show(ui, |plot| {
            match kind {
                ChartKind::Line => {
                    let mut closes: Vec<[f64; 2]> =
                        bars.iter().enumerate().skip(first).map(|(i, b)| [i as f64, b.close]).collect();
                    if let Some(q) = live_quote {
                        closes.push([now_x, q.price]);
                    }
                    plot.line(Line::new("Fechamento", closes).width(1.5).color(TEXT));
                    if let Some(q) = live_quote {
                        plot.points(Points::new("", vec![[now_x, q.price]]).radius(4.0).color(AMBER));
                    }
                }
                ChartKind::Candle => {
                    let mut candles: Vec<BoxElem> = bars
                        .iter()
                        .enumerate()
                        .skip(first)
                        .map(|(i, b)| candle(i as f64, b.open, b.high, b.low, b.close, &fmt_date(b.date)))
                        .collect();
                    if let Some(q) = live_quote {
                        candles.push(candle(now_x, q.open, q.high, q.low, q.price, "Hoje (ao vivo)"));
                    }
                    plot.box_plot(
                        BoxPlot::new("Candles", candles)
                            .color(TEXT)
                            .element_formatter(Box::new(|e, _| e.name.clone())),
                    );
                }
            }
            if let Some(m) = metrics {
                plot.hline(HLine::new("Média 2a", m.avg2y).color(ACCENT).style(LineStyle::dashed_loose()));
                plot.hline(HLine::new("Máx 2a", m.max2y).color(GREEN.gamma_multiply(0.7)).style(LineStyle::dotted_dense()));
                plot.hline(HLine::new("Mín 2a", m.min2y).color(RED.gamma_multiply(0.7)).style(LineStyle::dotted_dense()));
                for &d in m.events.iter().filter(|&&d| d >= from) {
                    let i = bars.partition_point(|b| b.date < d);
                    plot.vline(VLine::new("Evento (±40%)", i as f64).color(AMBER).style(LineStyle::dashed_dense()));
                }
            }
            if let Some(q) = live_quote {
                plot.hline(
                    HLine::new(format!("Agora {}", fmt_num(q.price, 2)), q.price)
                        .color(AMBER)
                        .width(1.5)
                        .style(LineStyle::dashed_loose()),
                );
            }
        });
    }
}

/// Candle como box plot: corpo entre abertura e fechamento, pavio até máxima e mínima.
/// O candle ao vivo é desenhado igual aos outros; o destaque fica na linha tracejada do preço atual.
fn candle(x: f64, open: f64, high: f64, low: f64, close: f64, label: &str) -> BoxElem {
    let color = if close >= open { GREEN } else { RED };
    let stroke = egui::Stroke::new(1.0, color);
    BoxElem::new(x, BoxSpread::new(low, open.min(close), close, open.max(close), high))
        .box_width(0.7)
        .whisker_width(0.0)
        .fill(color.gamma_multiply(0.6))
        .stroke(stroke)
        .name(format!(
            "{label}\nAbertura {}\nMáxima {}\nMínima {}\nFechamento {}",
            fmt_num(open, 2),
            fmt_num(high, 2),
            fmt_num(low, 2),
            fmt_num(close, 2)
        ))
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        self.maybe_sync(&ctx);
        self.update_live_control();
        // Acorda de vez em quando mesmo sem eventos, para a sincronização da noite.
        ctx.request_repaint_after(Duration::from_secs(60));

        egui::Panel::top("top").frame(theme::top_bar_frame()).show(ui, |ui| self.top_bar(ui));
        egui::CentralPanel::default().frame(theme::central_frame()).show(ui, |ui| match self.selected.clone() {
            Some(t) => self.asset_view(ui, &t),
            None => self.list_view(ui),
        });
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.s);
    }
}

/// Estrela de favorito: cheia quando marcado.
fn star_button(ui: &mut egui::Ui, on: bool) -> egui::Response {
    let (glyph, color, tip) = if on {
        ("★", AMBER, "Remover dos favoritos")
    } else {
        ("☆", ui.visuals().weak_text_color(), "Adicionar aos favoritos")
    };
    ui.add(egui::Button::new(RichText::new(glyph).color(color).size(15.0)).frame(false)).on_hover_text(tip)
}

/// Indicador com rótulo pequeno acima do valor.
fn stat(ui: &mut egui::Ui, label: &str, value: impl FnOnce(&mut egui::Ui)) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.label(RichText::new(label).small().color(TEXT_WEAK));
        value(ui);
    });
}

fn toggle(set: &mut BTreeSet<String>, ticker: String) {
    if !set.remove(&ticker) {
        set.insert(ticker);
    }
}

fn pct_label(ui: &mut egui::Ui, v: Option<f64>) {
    match v {
        Some(v) => {
            let color = if v > 0.0 { GREEN } else if v < 0.0 { RED } else { ui.visuals().text_color() };
            ui.label(RichText::new(fmt_pct(v)).color(color));
        }
        None => {
            ui.label("—");
        }
    }
}

/// Número no formato brasileiro: `1.234,56`.
fn fmt_num(v: f64, decimals: usize) -> String {
    let s = format!("{:.*}", decimals, v.abs());
    let (int, frac) = s.split_once('.').unwrap_or((&s, ""));
    let mut grouped = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push('.');
        }
        grouped.push(c);
    }
    let sign = if v < 0.0 && s.chars().any(|c| c.is_ascii_digit() && c != '0') { "-" } else { "" };
    if frac.is_empty() { format!("{sign}{grouped}") } else { format!("{sign}{grouped},{frac}") }
}

fn fmt_pct(v: f64) -> String {
    let sign = if v > 0.0 { "+" } else { "" };
    format!("{sign}{}%", fmt_num(v * 100.0, 1))
}

fn fmt_money_short(v: f64) -> String {
    if v >= 1e9 {
        format!("{} bi", fmt_num(v / 1e9, 1))
    } else {
        format!("{} mi", fmt_num(v / 1e6, 1))
    }
}

fn fmt_date(d: u32) -> String {
    analysis::to_date(d).format("%d/%m/%Y").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_brazilian_numbers() {
        assert_eq!(fmt_num(1234567.891, 2), "1.234.567,89");
        assert_eq!(fmt_num(176608.23, 0), "176.608");
        assert_eq!(fmt_num(-0.5, 1), "-0,5");
        assert_eq!(fmt_num(-0.001, 1), "0,0");
        assert_eq!(fmt_pct(0.1234), "+12,3%");
        assert_eq!(fmt_pct(-0.05), "-5,0%");
    }
}
