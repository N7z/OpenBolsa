//! Tema visual: escuro, neutro e discreto. Cor só onde carrega informação (alta/queda, alertas).

use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Margin, Shadow, Stroke, TextStyle, Theme, Vec2, Visuals,
};

/// Fundo da área de conteúdo.
pub const BG: Color32 = Color32::from_rgb(20, 22, 26);
/// Barra superior e cartões, um degrau acima do fundo.
pub const SURFACE: Color32 = Color32::from_rgb(27, 30, 35);
/// Campos de texto e fundo do gráfico.
pub const SUNKEN: Color32 = Color32::from_rgb(15, 17, 20);
pub const BORDER: Color32 = Color32::from_rgb(42, 46, 54);
pub const TEXT: Color32 = Color32::from_rgb(214, 218, 224);
pub const TEXT_STRONG: Color32 = Color32::from_rgb(240, 242, 245);
pub const TEXT_WEAK: Color32 = Color32::from_rgb(128, 136, 148);
/// Destaque de interface (seleção, foco, média no gráfico). Azul acinzentado, sem saturar.
pub const ACCENT: Color32 = Color32::from_rgb(98, 140, 200);

pub const GREEN: Color32 = Color32::from_rgb(78, 170, 118);
pub const RED: Color32 = Color32::from_rgb(214, 96, 92);
pub const AMBER: Color32 = Color32::from_rgb(208, 162, 74);

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(Theme::Dark);
    ctx.style_mut_of(Theme::Dark, |style| {
        style.visuals = visuals();

        use FontFamily::{Monospace, Proportional};
        style.text_styles = [
            (TextStyle::Small, FontId::new(11.0, Proportional)),
            (TextStyle::Body, FontId::new(13.5, Proportional)),
            (TextStyle::Button, FontId::new(13.5, Proportional)),
            (TextStyle::Heading, FontId::new(19.0, Proportional)),
            (TextStyle::Monospace, FontId::new(13.0, Monospace)),
        ]
        .into();

        let s = &mut style.spacing;
        s.item_spacing = Vec2::new(8.0, 6.0);
        s.button_padding = Vec2::new(10.0, 4.0);
        s.interact_size.y = 24.0;
        s.window_margin = Margin::same(12);
        s.menu_margin = Margin::same(6);
        // Barra sólida: a flutuante cobre a última coluna da tabela.
        s.scroll.floating = false;
        s.scroll.bar_width = 8.0;
    });
}

fn visuals() -> Visuals {
    let mut v = Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = SURFACE;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.window_corner_radius = CornerRadius::same(6);
    v.menu_corner_radius = CornerRadius::same(6);
    v.window_shadow = Shadow { offset: [0, 6], blur: 16, spread: 0, color: Color32::from_black_alpha(90) };
    v.popup_shadow = Shadow { offset: [0, 4], blur: 10, spread: 0, color: Color32::from_black_alpha(80) };
    v.extreme_bg_color = SUNKEN;
    v.faint_bg_color = Color32::from_rgb(24, 26, 31);
    v.code_bg_color = SURFACE;
    v.hyperlink_color = ACCENT;
    v.warn_fg_color = AMBER;
    v.error_fg_color = RED;
    v.weak_text_color = Some(TEXT_WEAK);
    v.selection.bg_fill = ACCENT.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, TEXT_STRONG);
    v.striped = true;

    let radius = CornerRadius::same(4);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = BG;
    w.noninteractive.weak_bg_fill = BG;
    w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    w.noninteractive.corner_radius = radius;

    w.inactive.weak_bg_fill = Color32::from_rgb(34, 38, 44);
    w.inactive.bg_fill = Color32::from_rgb(40, 44, 51);
    w.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    w.inactive.corner_radius = radius;

    w.hovered.weak_bg_fill = Color32::from_rgb(42, 47, 55);
    w.hovered.bg_fill = Color32::from_rgb(48, 53, 62);
    w.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(64, 70, 80));
    w.hovered.fg_stroke = Stroke::new(1.0, TEXT_STRONG);
    w.hovered.corner_radius = radius;
    w.hovered.expansion = 0.0;

    w.active.weak_bg_fill = Color32::from_rgb(50, 56, 66);
    w.active.bg_fill = Color32::from_rgb(56, 62, 72);
    w.active.bg_stroke = Stroke::new(1.0, ACCENT);
    w.active.fg_stroke = Stroke::new(1.0, TEXT_STRONG);
    w.active.corner_radius = radius;
    w.active.expansion = 0.0;

    w.open.weak_bg_fill = Color32::from_rgb(42, 47, 55);
    w.open.bg_fill = Color32::from_rgb(42, 47, 55);
    w.open.bg_stroke = Stroke::new(1.0, BORDER);
    w.open.fg_stroke = Stroke::new(1.0, TEXT_STRONG);
    w.open.corner_radius = radius;
    v
}

/// Moldura da barra superior.
pub fn top_bar_frame() -> egui::Frame {
    egui::Frame::new().fill(SURFACE).inner_margin(Margin::symmetric(14, 8)).stroke(Stroke::new(1.0, BORDER))
}

pub fn central_frame() -> egui::Frame {
    egui::Frame::new().fill(BG).inner_margin(Margin::symmetric(14, 10))
}

/// Cartão discreto para agrupar indicadores.
pub fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(14, 10))
}
