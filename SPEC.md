# OpenBolsa — Spec

Visualizador leve de ativos da B3, nativo em Rust, com gráfico de preços e análises sobre média histórica. Apenas dados públicos e gratuitos.

## Objetivo

- Baixar e guardar localmente o histórico diário oficial das ações da B3.
- Mostrar preço atual (durante o pregão) contra a média dos últimos 2 anos.
- Encontrar ações "fortes que vinham bem" e caíram abaixo da média de 2 anos.
- Gráfico por ativo e, depois, acompanhamento de carteira.

## Fontes de dados

### 1. B3 COTAHIST (histórico oficial) — fonte principal

- Grátis, sem cadastro, todas as ações desde 1986.
- Só fechamento: o arquivo do dia sai à noite, depois do pregão.
- **Preços não ajustados** por dividendos, desdobramentos e grupamentos.

URLs:

| Arquivo | URL | Observação |
|---|---|---|
| Anual | `https://bvmf.bmfbovespa.com.br/InstDados/SerHist/COTAHIST_A{AAAA}.ZIP` | O do ano corrente é atualizado ao longo do ano. Testado: 2024 (79 MB), 2025 (89 MB), 2026 (96 MB, ia até 23/10/2026) |
| Diário | `https://bvmf.bmfbovespa.com.br/InstDados/SerHist/COTAHIST_D{DDMMAAAA}.ZIP` | Retorna 404 até ser publicado (à noite) |
| Mensal | `https://bvmf.bmfbovespa.com.br/InstDados/SerHist/COTAHIST_M{MMAAAA}.ZIP` | Não testado |

Formato: TXT de largura fixa, 245 caracteres por linha, codificação latin-1.
Registro `00` = header, `01` = cotação, `99` = trailer.

Layout do registro `01` (posições 1-indexadas, inclusivas):

| Campo | Pos. | Descrição |
|---|---|---|
| TIPREG | 1–2 | `01` |
| DATA | 3–10 | Data do pregão, `AAAAMMDD` |
| CODBDI | 11–12 | `02` = lote padrão, `96` = fracionário |
| CODNEG | 13–24 | Ticker (ex.: `BBAS3`), com espaços à direita |
| TPMERC | 25–27 | `010` = à vista, `020` = fracionário |
| NOMRES | 28–39 | Nome resumido da empresa |
| ESPECI | 40–49 | Especificação (ON, PN, UNT...) |
| PREABE | 57–69 | Abertura |
| PREMAX | 70–82 | Máxima |
| PREMIN | 83–95 | Mínima |
| PREMED | 96–108 | Média |
| PREULT | 109–121 | Fechamento |
| TOTNEG | 148–152 | Número de negócios |
| QUATOT | 153–170 | Quantidade de títulos negociados |
| VOLTOT | 171–188 | Volume financeiro |
| FATCOT | 211–217 | Fator de cotação (1 = por unidade, 1000 = por lote de mil) |
| CODISI | 231–242 | Código ISIN |

Preços e volume são inteiros com 2 casas implícitas: dividir por 100.

Filtro usado para ações à vista: `TIPREG == "01" && CODBDI == "02" && TPMERC == "010"`.

### Mirror OpenBolsa (COTAHIST filtrado)

Os anuais da B3 trazem todos os mercados (opções, termo, fracionário…) e pesam ~90 MB por ano. O mirror guarda só as linhas do filtro acima, um ZIP com CSV por ano (~2 MB), e é por ele que o app baixa o histórico.

- Gerado por `openbolsa --build-mirror <dir> [--from AAAA]` (`src/mirror.rs`). Anos antigos já gerados são mantidos; o ano corrente e o anterior são refeitos a partir do anual da B3, completados com os diários publicados depois dele.
- Publicado pelo workflow `.github/workflows/dados.yml` (dias úteis, 23h e 7h de Brasília) como assets da release `dados` do GitHub: `manifest.json` + `A{AAAA}.zip`.
- `manifest.json`: `format` (1), `generated_at`, `source` e `years[]` com `year`, `file`, `rows`, `bytes`, `first_date`, `last_date`.
- CSV separado por `;`, com cabeçalho: `date;ticker;open;high;low;close;trades;quantity;volume;name;spec;isin`. Preços em R$ já divididos por 100 e pelo FATCOT.
- O app baixa um ano quando o banco local não tem pregões até o `last_date` dele. Se o mirror falhar, baixa os anuais da B3 direto. Os diários depois do último pregão do mirror vêm sempre da B3.
- `OPENBOLSA_MIRROR` troca a URL do mirror (vazia desliga).
- Por padrão só os últimos 3 anos; "Histórico completo" (`--full` na linha de comando) baixa desde 1986. Antes de julho/1994 os preços estão em moedas antigas.

### 2. Cotação ao vivo da B3 — preço durante o pregão

- `GET https://cotacao.b3.com.br/mds/api/v1/instrumentQuotation/{TICKER}`
- Funciona para ações e para o índice (`IBOV`). Grátis, sem token.
- **Endpoint não documentado**, usado pelo site da B3: pode mudar ou bloquear sem aviso. Não tem limite publicado, então use com moderação (≈0,15 s entre chamadas funcionou para ~113 tickers).

Resposta (exemplo real, BBAS3 em 26/10/2026 16:37):

```json
{
  "BizSts": {"cd": "OK"},
  "Msg": {"dtTm": "2026-10-26 16:37:06"},
  "Trad": [{
    "scty": {
      "SctyQtn": {
        "opngPric": 16.48, "minPric": 16.48, "maxPric": 18.12,
        "avrgPric": 17.327, "curPrc": 17.8, "prcFlcn": -27.9935275
      },
      "mkt": {"nm": "Vista"},
      "symb": "BBAS3",
      "desc": "BRASIL      ON      NM",
      "indxCmpnInd": true
    },
    "ttlQty": 280617
  }]
}
```

- `curPrc` = preço atual; `prcFlcn` = variação % do dia; `indxCmpnInd` = compõe índice.

### 3. Fontes descartadas

| Fonte | Motivo |
|---|---|
| brapi.dev (plano grátis) | Histórico só de `1d, 5d, 1mo, 3mo`. Range maior só para PETR4, VALE3, ITUB4, MGLU3 (ações de teste). Plano Pro custa R$ 139,99/mês |
| Yahoo Finance (`query1/query2.finance.yahoo.com/v8/finance/chart`) | Não oficial; retornou `Too Many Requests` em todas as tentativas |
| Okanebox (`/api/acoes/hist/{TICKER}/...`) | Só ~1 ano e com atraso de dias |
| Stooq | Pede desafio JavaScript (bloqueio de bot) |

### Regras de requisição

- User-Agent sempre de navegador comum: `Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:144.0) Gecko/20100101 Firefox/144.0`.
- Nenhum dado pessoal em cabeçalhos, URL ou corpo.

## Análises

### Média de 2 anos

- Média simples dos fechamentos (`PREULT`) dos pregões nos últimos 2 anos.
- Universo: ações com volume financeiro médio > R$ 15 milhões/dia nos últimos 60 pregões e pelo menos 300 pregões de histórico.

### Métricas por ativo

| Métrica | Cálculo |
|---|---|
| Preço atual | `curPrc` ao vivo ou último `PREULT` |
| Variação do dia | `prcFlcn` |
| Distância da média hoje | `preço atual / média 2a − 1` |
| Distância da média no pregão anterior | `fechamento anterior / média 2a − 1` |
| Retorno 12 meses | `fechamento anterior / fechamento há 1 ano − 1` |
| Retorno 3 meses | `fechamento anterior / fechamento há 63 pregões − 1` |
| Mínima e máxima de 2 anos | min/max de `PREULT` |

### Filtro "cruzou abaixo da média"

Estava acima da média de 2 anos no pregão anterior **e** está abaixo agora.
"Empresa forte" é julgamento qualitativo; o programa só mostra os números.

### Distorção por eventos corporativos

Como o COTAHIST não é ajustado:

- Desdobramentos e grupamentos distorcem a média. Sinalizar o ativo quando o fechamento variar mais de ±40% entre dois pregões seguidos (casos vistos: SIMH3, SBSP3, HAPV3, ORVR3, DIRR3, VIVT3).
- Dividendos deixam a média um pouco acima da real em boas pagadoras.
- Melhoria futura: ajustar com os eventos corporativos publicados pela B3.

## Stack (Rust)

| Parte | Crate |
|---|---|
| Interface nativa | `eframe` / `egui` |
| Gráficos | `egui_plot` |
| HTTP | `reqwest` |
| Descompactar ZIP | `zip` |
| Armazenamento local | `rusqlite` (SQLite) |
| JSON | `serde`, `serde_json` |

Alternativa em terminal: `ratatui`.

Cache: SQLite local com as cotações; na abertura, baixar do mirror os anos que faltam e da B3 só os dias que faltam (arquivo diário).

## Telas

1. **Lista de ativos** — tabela com as métricas acima, ordenável, com filtros (ex.: "cruzou abaixo da média hoje", liquidez mínima).
2. **Ativo** — gráfico de preço (linha ou candle) com a média de 2 anos, máxima e mínima de 2 anos e marcação de possíveis eventos corporativos.
3. **Carteira** — posições, quantidade, preço médio, resultado (fase posterior).

## Fases

1. Download e parse do COTAHIST para SQLite; atualização incremental pelo arquivo diário.
2. Tela de lista com as métricas e o filtro de cruzamento.
3. Gráfico por ativo.
4. Cotação ao vivo durante o pregão.
5. Carteira.
6. Ajuste por proventos e eventos corporativos.
