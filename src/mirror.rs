//! Mirror do histórico: o COTAHIST já filtrado (só ações à vista), um ZIP com CSV por ano.
//!
//! Gerado uma vez por dia (`openbolsa --build-mirror <dir>`, ver `.github/workflows/dados.yml`)
//! e publicado como arquivos estáticos. O app baixa daqui em vez dos ~90 MB por ano da B3;
//! se o mirror estiver fora do ar, volta para a B3 direto.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;

use chrono::{Datelike, NaiveDate, Weekday};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::b3;
use crate::cotahist::Quote;
use crate::db;

/// Onde o workflow publica os arquivos. `OPENBOLSA_MIRROR` troca a URL; vazia desliga o mirror.
const DEFAULT_URL: &str = "https://github.com/N7z/OpenBolsa/releases/download/dados";
const FORMAT: u32 = 1;
const MANIFEST: &str = "manifest.json";
const CSV_HEADER: &str = "date;ticker;open;high;low;close;trades;quantity;volume;name;spec;isin";

#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub generated_at: String,
    pub source: String,
    pub years: Vec<YearFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YearFile {
    pub year: i32,
    pub file: String,
    pub rows: usize,
    pub bytes: usize,
    /// Primeiro e último pregão do arquivo (`AAAAMMDD`).
    pub first_date: u32,
    pub last_date: u32,
}

pub fn url() -> Option<String> {
    let url = std::env::var("OPENBOLSA_MIRROR").unwrap_or_else(|_| DEFAULT_URL.into());
    let url = url.trim().trim_end_matches('/');
    (!url.is_empty()).then(|| url.to_string())
}

/// Campos de texto não podem conter o separador.
fn clean(s: &str) -> String {
    s.replace([';', '\n', '\r'], " ")
}

pub fn write_zip(quotes: &[Quote], csv_name: &str) -> anyhow::Result<Vec<u8>> {
    let mut csv = String::with_capacity(quotes.len() * 80);
    csv.push_str(CSV_HEADER);
    csv.push('\n');
    for q in quotes {
        csv.push_str(&format!(
            "{};{};{};{};{};{};{};{};{};{};{};{}\n",
            q.date,
            clean(&q.ticker),
            q.open,
            q.high,
            q.low,
            q.close,
            q.trades,
            q.quantity,
            q.volume,
            clean(&q.name),
            clean(&q.spec),
            clean(&q.isin)
        ));
    }
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(9));
    zip.start_file(csv_name, opts)?;
    zip.write_all(csv.as_bytes())?;
    Ok(zip.finish()?.into_inner())
}

pub fn parse_csv(reader: impl Read) -> anyhow::Result<Vec<Quote>> {
    let mut out = Vec::new();
    for (i, line) in BufReader::new(reader).lines().enumerate() {
        let line = line?;
        if i == 0 {
            anyhow::ensure!(line == CSV_HEADER, "cabeçalho do CSV inesperado: {line}");
            continue;
        }
        if line.is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split(';').collect();
        anyhow::ensure!(f.len() == 12, "linha {} do CSV com {} campos", i + 1, f.len());
        let num = |k: usize| -> anyhow::Result<f64> { Ok(f[k].parse()?) };
        out.push(Quote {
            date: f[0].parse()?,
            ticker: f[1].into(),
            open: num(2)?,
            high: num(3)?,
            low: num(4)?,
            close: num(5)?,
            trades: f[6].parse()?,
            quantity: f[7].parse()?,
            volume: num(8)?,
            name: f[9].into(),
            spec: f[10].into(),
            isin: f[11].into(),
        });
    }
    Ok(out)
}

pub fn parse_zip(bytes: &[u8]) -> anyhow::Result<Vec<Quote>> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
    parse_csv(archive.by_index(0)?)
}

/// Baixa do mirror os anos a partir de `first_year` que o banco ainda não tem completos.
/// Devolve quantos arquivos foram gravados; erro se o mirror não responder.
pub fn sync(
    client: &reqwest::blocking::Client,
    conn: &mut Connection,
    first_year: i32,
    progress: &dyn Fn(String),
) -> anyhow::Result<usize> {
    let base = url().ok_or_else(|| anyhow::anyhow!("mirror desligado (OPENBOLSA_MIRROR vazia)"))?;
    progress("Consultando o mirror do histórico…".into());
    let manifest: Manifest = client.get(format!("{base}/{MANIFEST}")).send()?.error_for_status()?.json()?;
    anyhow::ensure!(manifest.format == FORMAT, "formato {} do mirror não suportado", manifest.format);

    let mut loaded = 0;
    for y in manifest.years.iter().filter(|y| y.year >= first_year) {
        // Já tem até o último pregão do arquivo (do mirror ou da B3 direto): nada a fazer.
        if db::last_date_in(conn, y.year)?.is_some_and(|d| d >= y.last_date) {
            continue;
        }
        progress(format!("Baixando {} do mirror ({:.1} MB)…", y.year, y.bytes as f64 / 1e6));
        let bytes = client.get(format!("{base}/{}", y.file)).send()?.error_for_status()?.bytes()?;
        let quotes = parse_zip(&bytes)?;
        db::insert_quotes(conn, &quotes)?;
        db::mark_loaded(conn, &format!("mirror:A{}:{}", y.year, y.last_date))?;
        loaded += 1;
    }
    Ok(loaded)
}

/// Gera ou atualiza o mirror em `dir`.
///
/// Anos antigos já presentes em `dir` são mantidos; o ano corrente e o anterior são sempre
/// refeitos a partir do anual da B3, completado com os arquivos diários publicados depois dele.
pub fn build(dir: &Path, first_year: i32, progress: &dyn Fn(String)) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    let client = b3::client()?;
    let today = b3::brt_now().date_naive();
    let year = today.year();
    let old: Vec<YearFile> = std::fs::read(dir.join(MANIFEST))
        .ok()
        .and_then(|b| serde_json::from_slice::<Manifest>(&b).ok())
        .filter(|m| m.format == FORMAT)
        .map(|m| m.years)
        .unwrap_or_default();

    let mut years = Vec::new();
    for y in first_year..=year {
        let file = format!("A{y}.zip");
        if y < year - 1
            && let Some(e) = old.iter().find(|e| e.year == y)
            && dir.join(&file).exists()
        {
            years.push(e.clone());
            continue;
        }

        let mut quotes = b3::fetch_cotahist(&client, &format!("A{y}"), progress)?.unwrap_or_default();
        if y >= year - 1 {
            // O anual atrasa alguns dias em relação aos diários.
            let mut day = quotes
                .iter()
                .map(|q| q.date)
                .max()
                .and_then(b3::to_date)
                .and_then(|d| d.succ_opt())
                .unwrap_or_else(|| NaiveDate::from_ymd_opt(y, 1, 1).unwrap());
            let end = today.min(NaiveDate::from_ymd_opt(y, 12, 31).unwrap());
            while day <= end {
                if !matches!(day.weekday(), Weekday::Sat | Weekday::Sun)
                    && let Some(q) = b3::fetch_cotahist(&client, &format!("D{}", day.format("%d%m%Y")), progress)?
                {
                    quotes.extend(q);
                }
                day = day.succ_opt().unwrap();
            }
        }
        if quotes.is_empty() {
            progress(format!("{y}: sem dados na B3"));
            continue;
        }

        quotes.sort_by(|a, b| a.date.cmp(&b.date).then_with(|| a.ticker.cmp(&b.ticker)));
        quotes.dedup_by(|a, b| a.date == b.date && a.ticker == b.ticker);
        let bytes = write_zip(&quotes, &format!("A{y}.csv"))?;
        let tmp = dir.join(format!("{file}.tmp"));
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, dir.join(&file))?;
        progress(format!("{y}: {} linhas, {:.1} MB", quotes.len(), bytes.len() as f64 / 1e6));
        years.push(YearFile {
            year: y,
            file,
            rows: quotes.len(),
            bytes: bytes.len(),
            first_date: quotes[0].date,
            last_date: quotes[quotes.len() - 1].date,
        });
    }

    let manifest = Manifest {
        format: FORMAT,
        generated_at: b3::brt_now().to_rfc3339(),
        source: "B3 COTAHIST: ações à vista, lote padrão (CODBDI 02, TPMERC 010). Preços não ajustados.".into(),
        years,
    };
    let tmp = dir.join(format!("{MANIFEST}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec_pretty(&manifest)?)?;
    std::fs::rename(&tmp, dir.join(MANIFEST))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_round_trip() {
        let q = Quote {
            date: 20261023,
            ticker: "BBAS3".into(),
            name: "BRASIL;X".into(),
            spec: "ON      NM".into(),
            open: 24.5,
            high: 25.0,
            low: 24.0,
            close: 24.7,
            trades: 12345,
            quantity: 1_000_000,
            volume: 24_700_000.37,
            isin: "BRBBASACNOR3".into(),
        };
        let back = parse_zip(&write_zip(std::slice::from_ref(&q), "A2026.csv").unwrap()).unwrap();
        assert_eq!(back.len(), 1);
        let b = &back[0];
        assert_eq!((b.date, b.ticker.as_str(), b.close, b.volume), (q.date, "BBAS3", 24.7, 24_700_000.37));
        assert_eq!(b.name, "BRASIL X");
        assert_eq!(b.spec, "ON      NM");
    }
}
