mod analysis;
mod app;
mod b3;
mod cotahist;
mod db;
mod live;
mod mirror;
mod theme;

fn main() -> eframe::Result {
    if let Some(dir) = arg_value("--build-mirror") {
        let first = arg_value("--from").and_then(|y| y.parse().ok()).unwrap_or(b3::FIRST_YEAR);
        if let Err(e) = mirror::build(std::path::Path::new(&dir), first, &|s| eprintln!("{s}")) {
            eprintln!("Erro: {e:#}");
            std::process::exit(1);
        }
        return Ok(());
    }
    if std::env::args().any(|a| a == "--sync") {
        if let Err(e) = sync_cli() {
            eprintln!("Erro: {e:#}");
            std::process::exit(1);
        }
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("OpenBolsa")
            .with_app_id("openbolsa")
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!("../assets/openbolsa.png"))
                    .expect("ícone embutido inválido"),
            )
            .with_inner_size([1280.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native("OpenBolsa", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}

/// Valor depois de uma opção, ex.: `--from 2020`.
fn arg_value(name: &str) -> Option<String> {
    std::env::args().skip_while(|a| a != name).nth(1)
}

/// Sincroniza sem abrir a janela e imprime os ativos que cruzaram abaixo da média.
/// Com `--live`, usa a cotação ao vivo como preço atual.
fn sync_cli() -> anyhow::Result<()> {
    let path = db::default_path();
    let mut conn = db::open(&path)?;
    b3::sync(&mut conn, &|s| eprintln!("{s}"))?;
    let since = analysis::from_date(b3::brt_now().date_naive() - chrono::Months::new(40));
    let bars = db::load_bars(&conn, since)?;
    let assets = db::load_assets(&conn)?;
    let mut metrics = analysis::compute_all(&bars, &assets, &Default::default());
    if std::env::args().any(|a| a == "--live") {
        let client = b3::client()?;
        let mut live = std::collections::HashMap::new();
        for m in metrics.iter().filter(|m| m.in_universe(15e6)) {
            match b3::live_quote(&client, &m.ticker) {
                Ok(q) => {
                    live.insert(m.ticker.clone(), q);
                }
                Err(e) => eprintln!("{}: {e:#}", m.ticker),
            }
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
        eprintln!("{} cotações ao vivo", live.len());
        metrics = analysis::compute_all(&bars, &assets, &live);
    }
    let universe: Vec<_> = metrics.iter().filter(|m| m.in_universe(15e6)).collect();
    eprintln!("{}: {} ativos, {} no universo", path.display(), metrics.len(), universe.len());
    for m in universe.iter().filter(|m| m.crossed_below()) {
        println!(
            "{:<7} {:>9.2}  média 2a {:>9.2}  hoje {:>+6.1}%  ant. {:>+6.1}%{}",
            m.ticker,
            m.price,
            m.avg2y,
            m.dist_now * 100.0,
            m.dist_prev * 100.0,
            if m.events.is_empty() { "" } else { "  [evento ±40%]" }
        );
    }
    Ok(())
}
