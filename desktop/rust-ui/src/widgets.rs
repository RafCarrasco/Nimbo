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
