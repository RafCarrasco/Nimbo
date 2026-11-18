//! Menu do botão direito, no estilo do Discord (`src/ui/ContextMenu.tsx` do site): abre onde o mouse
//! está, cabe na janela, fecha com clique fora, Esc, rolagem ou a janela perdendo o foco. Quem abre
//! monta os itens a cada quadro com os dados atuais: marcar um cargo atualiza a caixa sem fechar.

use std::time::{Duration, Instant};

use egui::{Color32, CursorIcon, Pos2, Rect, Sense, TextureHandle, Ui, pos2, vec2};
use lucide_icons::Icon;

use crate::theme;
use crate::widgets::{check_box, icon, photo, role_dot};

const WIDTH: f32 = 232.0;

pub enum Item {
    /// Cabeçalho da pessoa: foto, nome (na cor do cargo, para o proprietário), uma linha embaixo e
    /// os cargos como etiquetas (só o proprietário recebe cargos).
    Header {
        photo: Option<TextureHandle>,
        name: String,
        color: Color32,
        detail: String,
        tags: Vec<(String, Color32)>,
    },
    Label(String),
    Separator,
    Action(Entry),
}

pub struct Entry {
    pub label: String,
    pub icon: Option<Icon>,
    pub danger: bool,
    /// Caixa de marcar: mostra o estado e o menu continua aberto depois do clique.
    pub checked: Option<bool>,
    /// Bolinha com a cor do cargo à esquerda.
    pub color: Option<Color32>,
    pub enabled: bool,
    pub hint: String,
}

impl Entry {
    pub fn new(label: impl Into<String>, glyph: Icon) -> Self {
        Self {
            label: label.into(),
            icon: Some(glyph),
            danger: false,
            checked: None,
            color: None,
            enabled: true,
            hint: String::new(),
        }
    }

    pub fn check(label: impl Into<String>, on: bool) -> Self {
        Self {
            label: label.into(),
            icon: None,
            danger: false,
            checked: Some(on),
            color: None,
            enabled: true,
            hint: String::new(),
        }
    }

    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    pub fn dot(mut self, color: Color32) -> Self {
        self.color = Some(color);
        self
    }

    pub fn enabled(mut self, enabled: bool, hint: &str) -> Self {
        self.enabled = enabled;
        if !enabled {
            self.hint = hint.to_owned();
        }
        self
    }
}

/// Onde o menu abriu e quando (o clique que abriu não pode fechar o menu no mesmo instante).
pub struct Place {
    pub at: Pos2,
    opened: Instant,
    /// A janela estava com o foco: perder o foco depois disso fecha o menu (como no site).
    focused: bool,
}

impl Place {
    pub fn new(at: Pos2) -> Self {
        Self {
            at,
            opened: Instant::now(),
            focused: false,
        }
    }
}

pub struct Shown {
    /// Índice do item clicado (em `items`).
    pub clicked: Option<usize>,
    /// O menu deve fechar (clique fora, Esc, rolagem, perdeu o foco, ou um item comum clicado).
    pub close: bool,
}

/// Desenha o menu por cima de tudo.
pub fn show(ctx: &egui::Context, id: &str, place: &mut Place, items: &[Item]) -> Shown {
    let mut clicked = None;
    let frame = egui::Frame::new()
        .fill(Color32::from_rgb(0x11, 0x17, 0x2a))
        .stroke((1.0, theme::BORDER))
        .corner_radius(10.0)
        .inner_margin(egui::Margin::same(6))
        .shadow(egui::epaint::Shadow {
            offset: [0, 10],
            blur: 28,
            spread: 0,
            color: Color32::from_black_alpha(110),
        });
    let area = egui::Area::new(egui::Id::unique(("menu", id)))
        .order(egui::Order::Foreground)
        .fixed_pos(place.at)
        .constrain(true)
        .show(ctx, |ui| {
            frame.show(ui, |ui| {
                ui.set_width(WIDTH);
                for (index, item) in items.iter().enumerate() {
                    if draw(ui, index, item) {
                        clicked = Some(index);
                    }
                }
            })
        });
    let rect = area.response.rect;
    let settled = place.opened.elapsed() > Duration::from_millis(120);
    let (escape, outside, scrolled, focused) = ctx.input(|input| {
        let outside = input.pointer.any_pressed()
            && input
                .pointer
                .interact_pos()
                .is_some_and(|pos| !rect.contains(pos));
        let scrolled = input
            .events
            .iter()
            .any(|event| matches!(event, egui::Event::MouseWheel { .. }));
        (
            input.key_pressed(egui::Key::Escape),
            outside,
            scrolled,
            input.focused,
        )
    });
    let keeps = clicked.and_then(|index| match &items[index] {
        Item::Action(entry) => Some(entry.checked.is_some()),
        _ => None,
    });
    let unfocused = place.focused && !focused;
    place.focused |= focused;
    let close = escape || (settled && (outside || scrolled || unfocused)) || keeps == Some(false);
    Shown { clicked, close }
}

fn draw(ui: &mut Ui, index: usize, item: &Item) -> bool {
    match item {
        Item::Header {
            photo: texture,
            name,
            color,
            detail,
            tags,
        } => {
            let (rect, _) = ui.allocate_exact_size(vec2(WIDTH, 52.0), Sense::hover());
            let painter = ui.painter();
            photo(
                painter,
                pos2(rect.left() + 8.0 + 18.0, rect.center().y),
                36.0,
                name,
                texture.as_ref(),
            );
            let left = rect.left() + 8.0 + 36.0 + 10.0;
            let title = theme::line(
                painter,
                name,
                theme::bold(14.0),
                *color,
                rect.right() - 8.0 - left,
            );
            let below = theme::line(
                painter,
                detail,
                theme::regular(12.0),
                theme::MUTED,
                rect.right() - 8.0 - left,
            );
            let top = rect.center().y - (title.size().y + below.size().y) / 2.0;
            let second = top + title.size().y;
            painter.galley(pos2(left, top), title, *color);
            painter.galley(pos2(left, second), below, theme::MUTED);
            if !tags.is_empty() {
                tag_rows(ui, tags);
            }
            false
        }
        Item::Label(text) => {
            let (rect, _) = ui.allocate_exact_size(vec2(WIDTH, 26.0), Sense::hover());
            let label = theme::spaced(
                ui.painter(),
                &text.to_uppercase(),
                theme::bold(10.5),
                theme::MUTED,
                WIDTH - 16.0,
                0.6,
            );
            ui.painter().galley(
                pos2(rect.left() + 8.0, rect.bottom() - 4.0 - label.size().y),
                label,
                theme::MUTED,
            );
            false
        }
        Item::Separator => {
            let (rect, _) = ui.allocate_exact_size(vec2(WIDTH, 9.0), Sense::hover());
            ui.painter().hline(
                rect.x_range().shrink(4.0),
                rect.center().y,
                (1.0, theme::BORDER),
            );
            false
        }
        Item::Action(entry) => {
            let (rect, _) = ui.allocate_exact_size(vec2(WIDTH, 32.0), Sense::hover());
            let sense = if entry.enabled {
                Sense::click()
            } else {
                Sense::hover()
            };
            let response = ui.interact(rect, ui.scope_id().with(("item", index)), sense);
            let hovered = entry.enabled && response.hovered();
            let painter = ui.painter();
            let accent = if entry.danger {
                theme::DANGER
            } else {
                theme::PRIMARY
            };
            if hovered {
                painter.rect_filled(
                    rect,
                    6.0,
                    if entry.danger {
                        theme::DANGER
                    } else {
                        accent.gamma_multiply(0.85)
                    },
                );
            }
            let mut color = match (hovered, entry.danger) {
                (true, _) => Color32::WHITE,
                (false, true) => theme::DANGER,
                (false, false) => theme::TEXT,
            };
            if !entry.enabled {
                color = color.gamma_multiply(0.45);
            }
            let mut x = rect.left() + 10.0;
            if let Some(dot) = entry.color {
                role_dot(painter, pos2(x + 6.0, rect.center().y), dot);
                x += 12.0 + 8.0;
            }
            if let Some(glyph) = entry.icon {
                icon(painter, pos2(x + 8.0, rect.center().y), glyph, 16.0, color);
                x += 16.0 + 10.0;
            }
            let right = rect.right() - if entry.checked.is_some() { 36.0 } else { 10.0 };
            let label = theme::line(
                painter,
                &entry.label,
                theme::regular(13.5),
                color,
                right - x,
            );
            painter.galley(
                pos2(x, rect.center().y - label.size().y / 2.0),
                label,
                color,
            );
            if let Some(on) = entry.checked {
                check_box(
                    painter,
                    pos2(rect.right() - 10.0 - 9.0, rect.center().y),
                    on,
                    entry.enabled,
                );
            }
            let response = if entry.hint.is_empty() {
                response
            } else {
                response.on_hover_text(&entry.hint)
            };
            if entry.enabled {
                response.on_hover_cursor(CursorIcon::PointingHand).clicked()
            } else {
                false
            }
        }
    }
}

/// Etiquetas dos cargos embaixo do cabeçalho, quebrando a linha quando não cabem.
fn tag_rows(ui: &mut Ui, tags: &[(String, Color32)]) {
    let painter = ui.painter().clone();
    let galleys: Vec<_> = tags
        .iter()
        .map(|(name, color)| {
            (
                theme::line(
                    &painter,
                    name,
                    theme::semibold(11.5),
                    theme::TEXT,
                    WIDTH - 40.0,
                ),
                *color,
            )
        })
        .collect();
    let mut rows: Vec<Vec<usize>> = vec![Vec::new()];
    let mut x = 0.0;
    for (index, (galley, _)) in galleys.iter().enumerate() {
        let width = 8.0 + 8.0 + 6.0 + galley.size().x + 8.0;
        if x + width > WIDTH - 16.0 && !rows.last().is_none_or(Vec::is_empty) {
            rows.push(Vec::new());
            x = 0.0;
        }
        rows.last_mut().expect("linha").push(index);
        x += width + 4.0;
    }
    ui.add_space(2.0);
    for row in rows {
        let (rect, _) = ui.allocate_exact_size(vec2(WIDTH, 24.0), Sense::hover());
        let mut x = rect.left() + 8.0;
        for index in row {
            let (galley, color) = &galleys[index];
            let width = 8.0 + 8.0 + 6.0 + galley.size().x + 8.0;
            let chip = Rect::from_min_size(pos2(x, rect.top() + 2.0), vec2(width, 20.0));
            painter.rect_filled(chip, 10.0, theme::SURFACE_2);
            painter.circle_filled(pos2(chip.left() + 8.0 + 4.0, chip.center().y), 4.0, *color);
            painter.galley(
                pos2(chip.left() + 22.0, chip.center().y - galley.size().y / 2.0),
                galley.clone(),
                theme::TEXT,
            );
            x += width + 4.0;
        }
    }
    ui.add_space(4.0);
}
