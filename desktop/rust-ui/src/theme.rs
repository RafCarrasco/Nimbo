//! Cores, fontes e medidas, copiadas de `src/style.css` do site para as duas telas ficarem iguais.

use std::sync::Arc;

use egui::epaint::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{Color32, FontData, FontDefinitions, FontFamily, FontId, Galley, Painter};

pub const BG: Color32 = Color32::from_rgb(0x0b, 0x10, 0x20);
pub const SIDEBAR: Color32 = Color32::from_rgb(0x0f, 0x15, 0x27);
pub const PANEL: Color32 = Color32::from_rgb(0x0c, 0x12, 0x22);
pub const SURFACE: Color32 = Color32::from_rgb(0x15, 0x1d, 0x32);
pub const SURFACE_2: Color32 = Color32::from_rgb(0x1c, 0x26, 0x42);
pub const SURFACE_3: Color32 = Color32::from_rgb(0x25, 0x30, 0x4f);
pub const BORDER: Color32 = Color32::from_rgb(0x25, 0x30, 0x50);
pub const TEXT: Color32 = Color32::from_rgb(0xe8, 0xed, 0xf7);
pub const MUTED: Color32 = Color32::from_rgb(0x8d, 0x98, 0xb0);
pub const PRIMARY: Color32 = Color32::from_rgb(0x7c, 0x6c, 0xff);
pub const PRIMARY_STRONG: Color32 = Color32::from_rgb(0x68, 0x57, 0xf2);
pub const DANGER: Color32 = Color32::from_rgb(0xf0, 0x47, 0x5a);
pub const LIVE: Color32 = Color32::from_rgb(0xff, 0x3b, 0x4e);

pub const SIDEBAR_WIDTH: f32 = 288.0;

const SEMIBOLD: &str = "semibold";
const BOLD: &str = "bold";
const ICONS: &str = "icons";

pub fn regular(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}
pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}
pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(BOLD.into()))
}
pub fn icons(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(ICONS.into()))
}

/// O site usa "Segoe UI" (a fonte do Windows); aqui ela é lida do próprio Windows. Sem os arquivos,
/// fica a fonte que vem com o egui.
pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let windows = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    let mut load = |name: &str, file: &str| -> bool {
        match std::fs::read(format!("{windows}\\Fonts\\{file}")) {
            Ok(bytes) => {
                fonts.font_data.insert(name.to_owned(), Arc::new(FontData::from_owned(bytes)));
                true
            }
            Err(_) => false,
        }
    };
    let (has_regular, has_semibold, has_bold) =
        (load("segoe", "segoeui.ttf"), load("segoe-semibold", "seguisb.ttf"), load("segoe-bold", "segoeuib.ttf"));
    // Emoji e símbolos que a Segoe UI não tem.
    let has_emoji = load("segoe-emoji", "seguiemj.ttf");
    fonts.font_data.insert(ICONS.to_owned(), Arc::new(FontData::from_static(lucide_icons::LUCIDE_FONT_BYTES)));

    let fallback = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let family = |first: Option<&str>| -> Vec<String> {
        first
            .map(str::to_owned)
            .into_iter()
            .chain(has_regular.then(|| "segoe".to_owned()))
            .chain(has_emoji.then(|| "segoe-emoji".to_owned()))
            .chain(fallback.iter().cloned())
            .collect()
    };
    fonts.families.insert(FontFamily::Proportional, family(None));
    fonts.families.insert(FontFamily::Name(SEMIBOLD.into()), family(has_semibold.then_some("segoe-semibold")));
    fonts.families.insert(FontFamily::Name(BOLD.into()), family(has_bold.then_some("segoe-bold")));
    fonts.families.insert(FontFamily::Name(ICONS.into()), vec![ICONS.to_owned()]);
    ctx.set_fonts(fonts);

    ctx.global_style_mut(|style| {
        style.visuals = egui::Visuals::dark();
        style.visuals.panel_fill = BG;
        style.visuals.window_fill = SURFACE;
        style.visuals.override_text_color = Some(TEXT);
        style.visuals.selection.bg_fill = PRIMARY.gamma_multiply(0.45);
        style.spacing.item_spacing = egui::vec2(0.0, 0.0);
        style.interaction.selectable_labels = false;
        for font in style.text_styles.values_mut() {
            *font = regular(14.0);
        }
    });
}

/// Uma linha de texto que termina em "…" quando não cabe na largura.
pub fn line(painter: &Painter, text: &str, font: FontId, color: Color32, max_width: f32) -> Arc<Galley> {
    spaced(painter, text, font, color, max_width, 0.0)
}

/// Como `line`, com espaço extra entre as letras (os títulos de categoria do site).
pub fn spaced(
    painter: &Painter,
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
    letter_spacing: f32,
) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(
        text.to_owned(),
        TextFormat { font_id: font, color, extra_letter_spacing: letter_spacing, ..TextFormat::default() },
    );
    job.wrap = TextWrapping { max_width, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    painter.layout_job(job)
}

/// Cor `#rrggbb` de um cargo.
pub fn hex(color: &str) -> Option<Color32> {
    let digits = color.strip_prefix('#').filter(|d| d.len() == 6)?;
    let part = |i: usize| u8::from_str_radix(digits.get(i..i + 2)?, 16).ok();
    Some(Color32::from_rgb(part(0)?, part(2)?, part(4)?))
}
