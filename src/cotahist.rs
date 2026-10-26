//! Parser do arquivo COTAHIST da B3 (TXT de largura fixa, 245 colunas, latin-1).

use std::io::{BufRead, BufReader, Read};

/// Uma linha de cotação (registro `01`) de ação à vista, lote padrão.
#[derive(Debug, Clone)]
pub struct Quote {
    /// Data do pregão como inteiro `AAAAMMDD`.
    pub date: u32,
    pub ticker: String,
    pub name: String,
    pub spec: String,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub trades: i64,
    pub quantity: i64,
    /// Volume financeiro em R$.
    pub volume: f64,
    pub isin: String,
}

/// Campo 1-indexado e inclusivo, como no layout oficial.
fn field(line: &[u8], start: usize, end: usize) -> &[u8] {
    &line[start - 1..end]
}

/// Converte bytes latin-1 em `String` (cada byte é o code point).
fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect::<String>().trim().to_string()
}

fn int(bytes: &[u8]) -> Option<i64> {
    std::str::from_utf8(bytes).ok()?.trim().parse().ok()
}

/// Lê uma linha e devolve a cotação se passar no filtro de ações à vista.
pub fn parse_line(line: &[u8]) -> Option<Quote> {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    if line.len() < 245 {
        return None;
    }
    // TIPREG == 01, CODBDI == 02, TPMERC == 010
    if field(line, 1, 2) != b"01" || field(line, 11, 12) != b"02" || field(line, 25, 27) != b"010" {
        return None;
    }

    let fatcot = int(field(line, 211, 217)).filter(|&f| f > 0).unwrap_or(1) as f64;
    let price = |s, e| int(field(line, s, e)).map(|v| v as f64 / 100.0 / fatcot);

    Some(Quote {
        date: int(field(line, 3, 10))? as u32,
        ticker: latin1(field(line, 13, 24)),
        name: latin1(field(line, 28, 39)),
        spec: latin1(field(line, 40, 49)),
        open: price(57, 69)?,
        high: price(70, 82)?,
        low: price(83, 95)?,
        close: price(109, 121)?,
        trades: int(field(line, 148, 152))?,
        quantity: int(field(line, 153, 170))?,
        volume: int(field(line, 171, 188))? as f64 / 100.0,
        isin: latin1(field(line, 231, 242)),
    })
}

/// Lê todas as linhas de um TXT do COTAHIST.
pub fn parse_reader(reader: impl Read) -> std::io::Result<Vec<Quote>> {
    let mut out = Vec::new();
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::with_capacity(256);
    loop {
        buf.clear();
        if reader.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        let line = buf.strip_suffix(b"\n").unwrap_or(&buf);
        if let Some(q) = parse_line(line) {
            out.push(q);
        }
    }
    Ok(out)
}

/// Abre o ZIP e lê o primeiro arquivo de dentro.
pub fn parse_zip(bytes: &[u8]) -> anyhow::Result<Vec<Quote>> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
    let file = archive.by_index(0)?;
    Ok(parse_reader(file)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> String {
        // Monta uma linha de 245 colunas com os campos nas posições do layout.
        let mut l = vec![b' '; 245];
        let mut put = |start: usize, s: &str| {
            l[start - 1..start - 1 + s.len()].copy_from_slice(s.as_bytes());
        };
        put(1, "01");
        put(3, "20261023");
        put(11, "02");
        put(13, "BBAS3       ");
        put(25, "010");
        put(28, "BRASIL      ");
        put(40, "ON      NM");
        put(57, "0000000002450");
        put(70, "0000000002500");
        put(83, "0000000002400");
        put(96, "0000000002460");
        put(109, "0000000002470");
        put(148, "12345");
        put(153, "000000000001000000");
        put(171, "000000002470000000");
        put(211, "0000001");
        put(231, "BRBBASACNOR3");
        String::from_utf8(l).unwrap()
    }

    #[test]
    fn parses_stock_line() {
        let q = parse_line(sample().as_bytes()).unwrap();
        assert_eq!(q.date, 20261023);
        assert_eq!(q.ticker, "BBAS3");
        assert_eq!(q.name, "BRASIL");
        assert_eq!(q.close, 24.70);
        assert_eq!(q.high, 25.00);
        assert_eq!(q.volume, 24_700_000.0);
        assert_eq!(q.isin, "BRBBASACNOR3");
    }

    #[test]
    fn skips_fractional_market() {
        let s = sample().replacen("02BBAS3", "96BBAS3", 1);
        assert!(parse_line(s.as_bytes()).is_none());
    }
}
