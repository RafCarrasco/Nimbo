//! Peças pequenas que se repetem: foto/inicial, ícone, botão de ícone, etiquetas.

use egui::{
    Align2, Color32, CursorIcon, Mesh, Painter, Pos2, Rect, Response, Sense, Shape, Ui, pos2, vec2,
};
use lucide_icons::Icon;

use crate::theme;

/// Ícone (da mesma família do site) centrado num ponto.
pub fn icon(painter: &Painter, center: Pos2, glyph: Icon, size: f32, color: Color32) {
    painter.text(
        center,
        Align2::CENTER_CENTER,
        char::from(glyph),
        theme::icons(size),
        color,
    );
}

/// Inicial do nome num círculo com o degradê do site.
pub fn avatar(painter: &Painter, center: Pos2, diameter: f32, name: &str) {
    let radius = diameter / 2.0;
    let (from, to) = ([0x8a_u8, 0x7c, 0xff], [0x4e_u8, 0x5b, 0xd8]);
    let mix = |p: Pos2| {
        // Degradê de 135°: do canto de cima à esquerda ao de baixo à direita.
        let t = (((p.x - center.x) + (p.y - center.y)) / (4.0 * radius) + 0.5).clamp(0.0, 1.0);
        let channel = |i: usize| {
            (f32::from(from[i]) + (f32::from(to[i]) - f32::from(from[i])) * t).round() as u8
        };
        Color32::from_rgb(channel(0), channel(1), channel(2))
    };
    // A borda suave vem do círculo de baixo; o degradê vai por cima, um fio menor.
    painter.circle_filled(center, radius, mix(center));
    const SEGMENTS: u32 = 48;
    let inner = radius - 0.6;
    let mut mesh = Mesh::default();
    mesh.colored_vertex(center, mix(center));
    for i in 0..=SEGMENTS {
        let angle = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
        let point = pos2(
            center.x + inner * angle.cos(),
            center.y + inner * angle.sin(),
        );
        mesh.colored_vertex(point, mix(point));
    }
    for i in 0..SEGMENTS {
        mesh.add_triangle(0, i + 1, i + 2);
    }
    painter.add(Shape::mesh(mesh));
    let initial: String = name
        .chars()
        .next()
        .map_or_else(|| "?".to_owned(), |c| c.to_uppercase().collect());
    let size = if diameter < 28.0 {
        11.0
    } else {
        13.0 * (diameter / 32.0).max(1.0)
    };
    painter.text(
        center,
        Align2::CENTER_CENTER,
        initial,
        theme::bold(size),
        Color32::WHITE,
    );
}

/// Botão quadrado só com ícone (32 px, ou 24 no tamanho pequeno).
pub fn icon_button(ui: &mut Ui, glyph: Icon, side: f32, icon_size: f32, hint: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(side, side), Sense::click());
    icon_button_at(ui, rect, response, glyph, icon_size, hint, false)
}

/// O mesmo botão, num retângulo já reservado.
pub fn icon_button_at(
    ui: &Ui,
    rect: Rect,
    response: Response,
    glyph: Icon,
    icon_size: f32,
    hint: &str,
    danger: bool,
) -> Response {
    let hovered = response.hovered();
    if hovered {
        ui.painter().rect_filled(
            rect,
            if rect.width() < 28.0 { 6.0 } else { 8.0 },
            theme::SURFACE_2,
        );
    }
    let color = match (hovered, danger) {
        (true, true) => theme::DANGER,
        (true, false) => theme::TEXT,
        _ => theme::MUTED,
    };
    icon(ui.painter(), rect.center(), glyph, icon_size, color);
    response
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text(hint)
}

/// Etiqueta "AO VIVO" ao lado de quem está transmitindo. Devolve a largura usada.
pub fn live_pill(painter: &Painter, right_center: Pos2, screen: bool, camera: bool) -> f32 {
    let label = painter.layout_no_wrap("AO VIVO".to_owned(), theme::bold(9.0), Color32::WHITE);
    let icons = usize::from(screen) + usize::from(camera);
    let width = 5.0 + icons as f32 * 13.0 + label.size().x + 5.0;
    let rect = Rect::from_min_size(
        pos2(right_center.x - width, right_center.y - 7.5),
        vec2(width, 15.0),
    );
    painter.rect_filled(rect, 4.0, theme::LIVE);
    let mut x = rect.left() + 5.0;
    for (on, glyph) in [(screen, Icon::Radio), (camera, Icon::Video)] {
        if on {
            icon(
                painter,
                pos2(x + 5.0, rect.center().y),
                glyph,
                10.0,
                Color32::WHITE,
            );
            x += 13.0;
        }
    }
    painter.galley(
        pos2(x, rect.center().y - label.size().y / 2.0),
        label,
        Color32::WHITE,
    );
    width
}

/// Contador de lives no canal ("📡 2"). Devolve a largura usada.
pub fn live_count(painter: &Painter, right_center: Pos2, count: usize) -> f32 {
    let label = painter.layout_no_wrap(count.to_string(), theme::bold(11.0), theme::LIVE);
    let width = 6.0 + 14.0 + label.size().x + 6.0;
    let rect = Rect::from_min_size(
        pos2(right_center.x - width, right_center.y - 9.0),
        vec2(width, 18.0),
    );
    painter.rect_filled(rect, 6.0, Color32::from_rgba_unmultiplied(255, 59, 78, 38));
    icon(
        painter,
        pos2(rect.left() + 6.0 + 5.5, rect.center().y),
        Icon::Radio,
        11.0,
        theme::LIVE,
    );
    painter.galley(
        pos2(rect.left() + 20.0, rect.center().y - label.size().y / 2.0),
        label,
        theme::LIVE,
    );
    width
}

/// Foto de perfil num círculo (recortada no meio, como `object-fit: cover`), ou a inicial enquanto
/// a foto não chega.
pub fn photo(
    painter: &Painter,
    center: Pos2,
    diameter: f32,
    name: &str,
    texture: Option<&egui::TextureHandle>,
) {
    let Some(texture) = texture else {
        return avatar(painter, center, diameter, name);
    };
    let [width, height] = texture.size().map(|side| side.max(1) as f32);
    let side = width.min(height);
    let (u, v) = ((width - side) / 2.0 / width, (height - side) / 2.0 / height);
    let uv = Rect::from_min_max(pos2(u, v), pos2(1.0 - u, 1.0 - v));
    let rect = Rect::from_center_size(center, vec2(diameter, diameter));
    painter.add(
        egui::epaint::RectShape::filled(rect, diameter / 2.0, Color32::WHITE)
            .with_texture(texture.id(), uv),
    );
}

/// Imagem preenchendo o retângulo sem distorcer (corta as sobras), com cantos arredondados.
pub fn cover(painter: &Painter, rect: Rect, radius: f32, texture: &egui::TextureHandle) {
    let [width, height] = texture.size().map(|side| side.max(1) as f32);
    let (want, have) = (rect.width() / rect.height().max(1.0), width / height);
    let uv = if have > want {
        let cut = (1.0 - want / have) / 2.0;
        Rect::from_min_max(pos2(cut, 0.0), pos2(1.0 - cut, 1.0))
    } else {
        let cut = (1.0 - have / want) / 2.0;
        Rect::from_min_max(pos2(0.0, cut), pos2(1.0, 1.0 - cut))
    };
    painter.add(
        egui::epaint::RectShape::filled(rect, radius, Color32::WHITE)
            .with_texture(texture.id(), uv),
    );
}

/// Estilo dos botões com texto, como `.button` no site.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Primary,
    Ghost,
    Danger,
}

/// Botão com texto (e ícone opcional) num retângulo já escolhido.
pub fn button_at(
    ui: &Ui,
    rect: Rect,
    id: egui::Id,
    label: &str,
    glyph: Option<Icon>,
    kind: Kind,
    enabled: bool,
) -> Response {
    let response = ui.interact(
        rect,
        id,
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let hovered = enabled && response.hovered();
    let (fill, color) = match kind {
        Kind::Primary => (
            if hovered {
                theme::PRIMARY_STRONG
            } else {
                theme::PRIMARY
            },
            Color32::WHITE,
        ),
        Kind::Ghost => (
            if hovered {
                theme::SURFACE_3
            } else {
                theme::SURFACE_2
            },
            theme::TEXT,
        ),
        Kind::Danger => (
            if hovered {
                theme::DANGER.gamma_multiply(0.85)
            } else {
                theme::DANGER
            },
            Color32::WHITE,
        ),
    };
    let alpha = if enabled { 1.0 } else { 0.45 };
    let painter = ui.painter();
    painter.rect_filled(rect, 8.0, fill.gamma_multiply(alpha));
    let text = painter.layout_no_wrap(
        label.to_owned(),
        theme::semibold(13.5),
        color.gamma_multiply(alpha),
    );
    let icon_width = if glyph.is_some() { 16.0 + 6.0 } else { 0.0 };
    let x = rect.center().x - (icon_width + text.size().x) / 2.0;
    if let Some(glyph) = glyph {
        icon(
            painter,
            pos2(x + 8.0, rect.center().y),
            glyph,
            16.0,
            color.gamma_multiply(alpha),
        );
    }
    painter.galley(
        pos2(x + icon_width, rect.center().y - text.size().y / 2.0),
        text,
        color,
    );
    if enabled {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    }
}

/// Largura que um botão de texto precisa.
pub fn button_width(ui: &Ui, label: &str, glyph: bool) -> f32 {
    let text = ui
        .painter()
        .layout_no_wrap(label.to_owned(), theme::semibold(13.5), theme::TEXT);
    text.size().x + 28.0 + if glyph { 22.0 } else { 0.0 }
}

/// Botão com texto no fluxo do layout.
pub fn button(
    ui: &mut Ui,
    label: &str,
    glyph: Option<Icon>,
    kind: Kind,
    enabled: bool,
) -> Response {
    let (rect, _) = ui.allocate_exact_size(
        vec2(button_width(ui, label, glyph.is_some()), 36.0),
        Sense::hover(),
    );
    button_at(
        ui,
        rect,
        ui.scope_id()
            .with(("botao", label, rect.min.x as i32, rect.min.y as i32)),
        label,
        glyph,
        kind,
        enabled,
    )
}

/// Caixa de marcar (quadrada, com o visto), como `.menu-check` do site.
pub fn check_box(painter: &Painter, center: Pos2, on: bool, enabled: bool) {
    let rect = Rect::from_center_size(center, vec2(18.0, 18.0));
    let alpha = if enabled { 1.0 } else { 0.45 };
    if on {
        painter.rect_filled(rect, 4.0, theme::PRIMARY.gamma_multiply(alpha));
        icon(
            painter,
            center,
            Icon::Check,
            13.0,
            Color32::WHITE.gamma_multiply(alpha),
        );
    } else {
        painter.rect_stroke(
            rect,
            4.0,
            (1.5, theme::MUTED.gamma_multiply(alpha)),
            egui::StrokeKind::Inside,
        );
    }
}

/// Chave liga/desliga.
pub fn switch(painter: &Painter, rect: Rect, on: bool, enabled: bool) {
    let alpha = if enabled { 1.0 } else { 0.45 };
    let track = if on { theme::SUCCESS } else { theme::SURFACE_3 };
    painter.rect_filled(rect, rect.height() / 2.0, track.gamma_multiply(alpha));
    let radius = rect.height() / 2.0 - 3.0;
    let x = if on {
        rect.right() - 3.0 - radius
    } else {
        rect.left() + 3.0 + radius
    };
    painter.circle_filled(
        pos2(x, rect.center().y),
        radius,
        Color32::WHITE.gamma_multiply(alpha),
    );
}

/// Bolinha com a cor do cargo.
pub fn role_dot(painter: &Painter, center: Pos2, color: Color32) {
    painter.circle_filled(center, 6.0, color);
}

/// Olho com o número de pessoas assistindo (`.live-viewers` no site). Devolve o retângulo usado.
pub fn viewers_chip(painter: &Painter, left_center: Pos2, count: usize, fill: Color32) -> Rect {
    let label = painter.layout_no_wrap(count.to_string(), theme::semibold(12.0), Color32::WHITE);
    let width = 7.0 + 13.0 + 4.0 + label.size().x + 7.0;
    let rect = Rect::from_min_size(pos2(left_center.x, left_center.y - 10.0), vec2(width, 20.0));
    painter.rect_filled(rect, 10.0, fill);
    icon(
        painter,
        pos2(rect.left() + 7.0 + 6.5, rect.center().y),
        Icon::Eye,
        13.0,
        Color32::WHITE,
    );
    painter.galley(
        pos2(rect.left() + 24.0, rect.center().y - label.size().y / 2.0),
        label,
        Color32::WHITE,
    );
    rect
}

/// "12:34" ou "1:02:03" (`formatElapsed` do site).
pub fn elapsed(ms: i64) -> String {
    let total = (ms / 1000).max(0);
    let (hours, minutes, seconds) = (total / 3600, total % 3600 / 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// Campo de texto de uma linha no estilo do site (`input`), ocupando a largura dada.
pub fn text_field(
    ui: &mut Ui,
    text: &mut String,
    width: f32,
    limit: usize,
    hint: &str,
) -> Response {
    let frame = egui::Frame::new()
        .fill(theme::PANEL)
        .stroke((1.0, theme::BORDER))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::symmetric(10, 8));
    frame
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::singleline(text)
                    .frame(egui::Frame::NONE)
                    .desired_width(width - 22.0)
                    .char_limit(limit)
                    .font(theme::regular(14.0))
                    .text_color(theme::TEXT)
                    .hint_text(egui::RichText::new(hint).color(theme::MUTED)),
            )
        })
        .inner
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_matches_the_site() {
        assert_eq!(elapsed(-5), "0:00");
        assert_eq!(elapsed(754_000), "12:34");
        assert_eq!(elapsed(3_723_000), "1:02:03");
    }
}
