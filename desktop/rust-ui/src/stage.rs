//! Palco do canal: cabeçalho com as lives no ar, os cartões "Ao vivo agora" (quando há lives e
//! nenhuma aberta) e os ladrilhos das que a pessoa abriu. Segue `src/components/Stage.tsx`,
//! `LiveTile.tsx` e `LiveMeta.tsx` do site: tempo no ar ao lado do AO VIVO, plateia (olho com o
//! número e os nomes), jogo, resolução e quadros por segundo, duplo clique para a tela cheia.

use std::collections::HashMap;

use egui::{Align2, Color32, CursorIcon, Rect, Sense, TextureHandle, Ui, pos2, vec2};
use lucide_icons::Icon;
use nimbo_core::community::Snapshot;
use nimbo_core::room::RoomSnapshot;

use crate::images::Images;
use crate::live::{BroadcastState, Broadcasting, LiveState, Watching, parse_key};
use crate::sidebar::Action;
use crate::theme;
use crate::video::Videos;
use crate::widgets::{cover, elapsed, icon, photo, viewers_chip};

/// Espaço entre os ladrilhos, como no site.
const GAP: f32 = 10.0;
const RATIO: f32 = 16.0 / 9.0;
/// Lado a lado é mais natural em telas largas: aceita lives até 20% menores para ganhar uma coluna.
const PREFER_COLUMNS: f32 = 0.8;
const TILE_FILL: Color32 = Color32::from_rgb(0x05, 0x07, 0x0d);
const CHIP_FILL: Color32 = Color32::from_rgba_premultiplied(5, 7, 13, 140);

/// Escolhe o número de colunas para as lives (16:9) ocuparem o máximo da área (`fitGrid` do site).
/// Devolve colunas, largura e altura de cada ladrilho.
pub fn fit_grid(count: usize, width: f32, height: f32) -> (usize, f32, f32) {
    let options: Vec<(usize, f32)> = (1..=count.max(1))
        .map(|cols| {
            let rows = count.div_ceil(cols).max(1);
            let by_width = (width - GAP * (cols - 1) as f32) / cols as f32;
            let by_height = (height - GAP * (rows - 1) as f32) / rows as f32 * RATIO;
            (cols, by_width.min(by_height).floor().max(0.0))
        })
        .collect();
    let largest = options.iter().map(|option| option.1).fold(0.0, f32::max);
    let (cols, tile) = options
        .iter()
        .rev()
        .find(|option| option.1 >= largest * PREFER_COLUMNS)
        .copied()
        .unwrap_or((1, 0.0));
    (cols, tile, (tile / RATIO).floor())
}

pub struct Stage<'a> {
    pub data: &'a Snapshot,
    pub room: &'a RoomSnapshot,
    pub channel_name: &'a str,
    /// Lives no ar (próprias e dos outros), na ordem das etiquetas do topo.
    pub active: &'a [String],
    /// Lives abertas no palco.
    pub shown: &'a [String],
    pub watching: &'a Watching,
    pub broadcasting: Option<&'a Broadcasting>,
    pub videos: &'a Videos,
    pub logo: &'a TextureHandle,
    pub notice: &'a str,
    pub images: &'a mut Images,
    /// Hora do servidor agora, em ms (para o tempo no ar).
    pub now_ms: i64,
    /// "1080p · 60 fps" de cada live que está chegando.
    pub quality: &'a HashMap<String, String>,
    /// Live em tela cheia, se houver.
    pub fullscreen: Option<&'a str>,
}

impl Stage<'_> {
    fn name_of(&self, key: &str) -> String {
        let (uid, kind) = parse_key(key);
        if uid == self.data.uid {
            return if kind == "camera" {
                "Sua câmera"
            } else {
                "Sua tela"
            }
            .to_owned();
        }
        self.person(uid)
    }

    fn person(&self, uid: &str) -> String {
        self.data
            .profiles
            .get(uid)
            .cloned()
            .unwrap_or_else(|| "Participante".to_owned())
    }

    /// Quem está assistindo uma live (só gente que está no canal), com "Você" para a própria pessoa.
    fn viewers_of(&self, key: &str) -> Vec<String> {
        self.room
            .watchers
            .get(key)
            .into_iter()
            .flatten()
            .filter(|uid| self.room.members.contains(uid))
            .map(|uid| {
                if *uid == self.data.uid {
                    "Você".to_owned()
                } else {
                    self.person(uid)
                }
            })
            .collect()
    }

    /// Há quanto tempo a live está no ar ("12:34"), se o início veio no anúncio.
    fn elapsed_of(&self, key: &str) -> Option<String> {
        let (uid, kind) = parse_key(key);
        let started = self.room.broadcasts.get(uid)?.get(kind)?.started_at;
        (started > 0).then(|| elapsed(self.now_ms - started))
    }

    fn game_of(&self, key: &str) -> Option<&str> {
        let (uid, kind) = parse_key(key);
        Some(self.room.broadcasts.get(uid)?.get(kind)?.game.as_str())
            .filter(|game| !game.is_empty())
    }

    pub fn show(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        if let Some(key) = self
            .fullscreen
            .filter(|key| self.shown.iter().any(|shown| shown == key))
        {
            let key = key.to_owned();
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(Color32::BLACK))
                .show(ui, |ui| {
                    let area = ui.max_rect();
                    self.tile(ui, area, &key, actions, true);
                });
            return;
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BG))
            .show(ui, |ui| {
                let area = ui.max_rect();
                let head = Rect::from_min_size(area.min, vec2(area.width(), 56.0));
                self.head(ui, head, actions);
                let body = Rect::from_min_max(pos2(area.left(), head.bottom()), area.max);
                if self.shown.is_empty() && !self.active.is_empty() {
                    self.cards(ui, body, actions);
                } else if self.shown.is_empty() {
                    self.empty(ui, body);
                } else {
                    self.tiles(ui, body.shrink(12.0), actions);
                }
                if !self.notice.is_empty() {
                    notice_bar(ui, body, self.notice);
                }
            });
    }

    /// Nome do canal, quantas pessoas, e uma etiqueta por live no ar (clicar abre ou fecha no palco).
    fn head(&self, ui: &mut Ui, head: Rect, actions: &mut Vec<Action>) {
        let painter = ui.painter().clone();
        painter.hline(head.x_range(), head.bottom() - 0.5, (1.0, theme::BORDER));
        let title =
            painter.layout_no_wrap(self.channel_name.to_owned(), theme::bold(16.0), theme::TEXT);
        let mut x = head.left() + 16.0;
        let title_width = title.size().x;
        painter.galley(
            pos2(x, head.center().y - title.size().y / 2.0),
            title,
            theme::TEXT,
        );
        x += title_width + 10.0;
        icon(
            &painter,
            pos2(x + 7.0, head.center().y),
            Icon::Users,
            14.0,
            theme::MUTED,
        );
        let count = painter.layout_no_wrap(
            self.room.members.len().to_string(),
            theme::regular(13.0),
            theme::MUTED,
        );
        let count_width = count.size().x;
        painter.galley(
            pos2(x + 18.0, head.center().y - count.size().y / 2.0),
            count,
            theme::MUTED,
        );
        x += 18.0 + count_width + 14.0;
        for key in self.active {
            let (uid, kind) = parse_key(key);
            let on = self.shown.contains(key);
            let name = if uid == self.data.uid {
                "Você".to_owned()
            } else {
                self.name_of(key)
            };
            let label = painter.layout_no_wrap(name, theme::semibold(12.0), theme::TEXT);
            let camera = kind == "camera";
            let width = 11.0 + 7.0 + 6.0 + label.size().x + if camera { 19.0 } else { 0.0 } + 11.0;
            let chip = Rect::from_min_size(pos2(x, head.center().y - 14.0), vec2(width, 28.0));
            if chip.right() > head.right() - 8.0 {
                break;
            }
            let response = ui.interact(chip, ui.scope_id().with(("etiqueta", key)), Sense::click());
            let color = if on || response.hovered() {
                theme::TEXT
            } else {
                theme::MUTED
            };
            if on {
                painter.rect_filled(chip, 14.0, theme::PRIMARY.gamma_multiply(0.16));
                painter.rect_stroke(
                    chip,
                    14.0,
                    (1.0, theme::PRIMARY.gamma_multiply(0.55)),
                    egui::StrokeKind::Inside,
                );
            } else {
                painter.rect_stroke(chip, 14.0, (1.0, theme::BORDER), egui::StrokeKind::Inside);
            }
            painter.circle_filled(
                pos2(chip.left() + 11.0 + 3.5, chip.center().y),
                3.5,
                theme::LIVE,
            );
            let label_width = label.size().x;
            painter.galley(
                pos2(chip.left() + 24.0, chip.center().y - label.size().y / 2.0),
                label,
                color,
            );
            if camera {
                icon(
                    &painter,
                    pos2(chip.left() + 24.0 + label_width + 12.0, chip.center().y),
                    Icon::Video,
                    13.0,
                    color,
                );
            }
            let hint = if on { "Ocultar live" } else { "Assistir live" };
            if response
                .on_hover_cursor(CursorIcon::PointingHand)
                .on_hover_text(hint)
                .clicked()
            {
                actions.push(Action::ToggleLive(key.clone()));
            }
            x += width + 6.0;
        }
    }

    fn empty(&self, ui: &Ui, body: Rect) {
        let painter = ui.painter();
        let (strong, hint) = (
            "Ninguém ao vivo por aqui",
            "Compartilhe sua tela pelo botão no canto inferior esquerdo: a live aparece aqui.",
        );
        let strong = painter.layout_no_wrap(strong.to_owned(), theme::bold(17.0), theme::TEXT);
        let hint = theme::centered(painter, hint, theme::regular(14.0), theme::MUTED, 420.0);
        let height = 90.0 + 8.0 + strong.size().y + 8.0 + hint.size().y;
        let mut y = body.center().y - height / 2.0;
        let logo = Rect::from_min_size(pos2(body.center().x - 45.0, y), vec2(90.0, 90.0));
        painter.image(
            self.logo.id(),
            logo,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE.gamma_multiply(0.9),
        );
        y += 98.0;
        painter.galley(
            pos2(body.center().x - strong.size().x / 2.0, y),
            strong.clone(),
            theme::TEXT,
        );
        y += strong.size().y + 8.0;
        painter.galley(pos2(body.center().x, y), hint, theme::MUTED);
    }

    /// Como no Discord: cada live do canal vira um cartão, com a prévia, quem transmite, o que é,
    /// há quanto tempo está no ar e quantos estão assistindo.
    fn cards(&mut self, ui: &mut Ui, body: Rect, actions: &mut Vec<Action>) {
        let ctx = ui.ctx().clone();
        let width = body.width().min(760.0) - 32.0;
        let count = self.active.len();
        let cols = ((width + 12.0) / (230.0 + 12.0))
            .floor()
            .clamp(1.0, count as f32) as usize;
        let card_width = ((width - 12.0 * (cols - 1) as f32) / cols as f32).min(340.0);
        let preview_height = (card_width - 28.0) / RATIO;
        let card_height = 14.0 + preview_height + 12.0 + 40.0 + 12.0 + 28.0 + 14.0;
        let rows = count.div_ceil(cols);
        let grid_height = rows as f32 * card_height + (rows.saturating_sub(1)) as f32 * 12.0;
        let total = 24.0 + 14.0 + grid_height + 14.0 + 18.0;
        let top = (body.center().y - total / 2.0).max(body.top() + 16.0);
        let painter = ui.painter().clone();
        painter.text(
            pos2(body.center().x, top + 12.0),
            Align2::CENTER_CENTER,
            "Ao vivo agora",
            theme::bold(17.0),
            theme::TEXT,
        );
        let grid_top = top + 24.0 + 14.0;
        for (index, key) in self.active.iter().enumerate() {
            let (row, col) = (index / cols, index % cols);
            let in_row = if row == rows - 1 {
                count - row * cols
            } else {
                cols
            };
            let row_width = in_row as f32 * card_width + 12.0 * (in_row - 1) as f32;
            let left = body.center().x - row_width / 2.0 + col as f32 * (card_width + 12.0);
            let card = Rect::from_min_size(
                pos2(left, grid_top + row as f32 * (card_height + 12.0)),
                vec2(card_width, card_height),
            );
            if card.top() > body.bottom() {
                break;
            }
            let (uid, kind) = parse_key(key);
            let own = uid == self.data.uid;
            let name = if own {
                "Você".to_owned()
            } else {
                self.person(uid)
            };
            let response = ui.interact(card, ui.scope_id().with(("cartao", key)), Sense::click());
            let hovered = response.hovered();
            painter.rect_filled(
                card,
                12.0,
                if hovered {
                    theme::SURFACE_2
                } else {
                    theme::SURFACE
                },
            );
            painter.rect_stroke(
                card,
                12.0,
                (
                    1.0,
                    if hovered {
                        theme::PRIMARY.gamma_multiply(0.55)
                    } else {
                        theme::BORDER
                    },
                ),
                egui::StrokeKind::Inside,
            );
            let preview = Rect::from_min_size(
                card.min + vec2(14.0, 14.0),
                vec2(card_width - 28.0, preview_height),
            );
            painter.rect_filled(preview, 8.0, theme::PANEL);
            match self
                .room
                .previews
                .get(key)
                .and_then(|data| self.images.get(&ctx, data))
            {
                Some(texture) => cover(&painter, preview, 8.0, &texture),
                None => {
                    let glyph = if kind == "camera" {
                        Icon::Video
                    } else {
                        Icon::MonitorPlay
                    };
                    icon(
                        &painter,
                        preview.center(),
                        glyph,
                        30.0,
                        theme::MUTED.gamma_multiply(0.6),
                    );
                }
            }
            // Cabeçalho: foto, nome e o que é (webcam, jogo ou tela).
            let head_y = preview.bottom() + 12.0 + 20.0;
            let texture = self
                .data
                .avatars
                .get(uid)
                .and_then(|source| self.images.get(&ctx, source));
            photo(
                &painter,
                pos2(card.left() + 14.0 + 18.0, head_y),
                36.0,
                &self.person(uid),
                texture.as_ref(),
            );
            let text_left = card.left() + 14.0 + 36.0 + 10.0;
            let room = card.right() - 14.0 - text_left;
            let title = theme::line(&painter, &name, theme::bold(14.0), theme::TEXT, room);
            let what = if kind == "camera" {
                "Webcam".to_owned()
            } else {
                self.game_of(key).unwrap_or("Tela").to_owned()
            };
            let sub = theme::line(
                &painter,
                &what,
                theme::regular(12.0),
                theme::MUTED,
                room - 18.0,
            );
            let title_top = head_y - (title.size().y + sub.size().y) / 2.0;
            let sub_top = title_top + title.size().y;
            painter.galley(pos2(text_left, title_top), title, theme::TEXT);
            let glyph = if kind == "camera" {
                Icon::Video
            } else if self.game_of(key).is_some() {
                Icon::Gamepad2
            } else {
                Icon::MonitorPlay
            };
            icon(
                &painter,
                pos2(text_left + 6.0, sub_top + sub.size().y / 2.0),
                glyph,
                12.0,
                theme::MUTED,
            );
            painter.galley(pos2(text_left + 17.0, sub_top), sub, theme::MUTED);
            // Embaixo: AO VIVO, tempo no ar, plateia e o botão.
            let meta_y = head_y + 20.0 + 12.0 + 14.0;
            let badge =
                painter.layout_no_wrap("AO VIVO".to_owned(), theme::bold(10.0), Color32::WHITE);
            let badge_rect = Rect::from_min_size(
                pos2(card.left() + 14.0, meta_y - 9.0),
                badge.size() + vec2(12.0, 6.0),
            );
            painter.rect_filled(badge_rect, 4.0, theme::LIVE);
            painter.galley(badge_rect.min + vec2(6.0, 3.0), badge, Color32::WHITE);
            let mut x = badge_rect.right() + 8.0;
            if let Some(time) = self.elapsed_of(key) {
                let galley = painter.layout_no_wrap(time, theme::semibold(12.0), theme::MUTED);
                painter.galley(
                    pos2(x, meta_y - galley.size().y / 2.0),
                    galley.clone(),
                    theme::MUTED,
                );
                x += galley.size().x + 8.0;
            }
            let viewers = self.viewers_of(key);
            if !viewers.is_empty() {
                let chip = viewers_chip(&painter, pos2(x, meta_y), viewers.len(), theme::SURFACE_3);
                ui.interact(
                    chip,
                    ui.scope_id().with(("plateia-cartao", key)),
                    Sense::hover(),
                )
                .on_hover_text(format!("Assistindo: {}", viewers.join(", ")));
            }
            let label = if own { "Ver prévia" } else { "Assistir" };
            let galley =
                painter.layout_no_wrap(label.to_owned(), theme::semibold(12.5), Color32::WHITE);
            let action = Rect::from_min_size(
                pos2(card.right() - 14.0 - galley.size().x - 24.0, meta_y - 14.0),
                vec2(galley.size().x + 24.0, 28.0),
            );
            painter.rect_filled(
                action,
                8.0,
                if hovered {
                    theme::PRIMARY_STRONG
                } else {
                    theme::PRIMARY
                },
            );
            painter.galley(
                action.center() - galley.size() / 2.0,
                galley,
                Color32::WHITE,
            );
            let hint = if own {
                "Ver a sua prévia".to_owned()
            } else {
                format!("Assistir a live de {name}")
            };
            if response
                .on_hover_cursor(CursorIcon::PointingHand)
                .on_hover_text(hint)
                .clicked()
            {
                actions.push(Action::ToggleLive(key.clone()));
            }
        }
        let note =
            "Dá para assistir várias ao mesmo tempo, pelos nomes acima ou pela barra lateral.";
        painter.text(
            pos2(body.center().x, grid_top + grid_height + 14.0 + 9.0),
            Align2::CENTER_CENTER,
            note,
            theme::regular(13.0),
            theme::MUTED,
        );
    }

    /// Grade de ladrilhos, centrada, como `.tile-grid` no site.
    fn tiles(&mut self, ui: &mut Ui, area: Rect, actions: &mut Vec<Action>) {
        let count = self.shown.len();
        let (cols, width, height) = fit_grid(count, area.width(), area.height());
        let rows = count.div_ceil(cols);
        let total_height = rows as f32 * height + GAP * (rows - 1) as f32;
        let top = area.center().y - total_height / 2.0;
        for (index, key) in self.shown.iter().enumerate() {
            let (row, col) = (index / cols, index % cols);
            let in_row = if row == rows - 1 {
                count - row * cols
            } else {
                cols
            };
            let row_width = in_row as f32 * width + GAP * (in_row - 1) as f32;
            let left = area.center().x - row_width / 2.0;
            let tile = Rect::from_min_size(
                pos2(
                    left + col as f32 * (width + GAP),
                    top + row as f32 * (height + GAP),
                ),
                vec2(width, height),
            );
            self.tile(ui, tile, key, actions, false);
        }
    }

    fn tile(&mut self, ui: &mut Ui, tile: Rect, key: &str, actions: &mut Vec<Action>, full: bool) {
        let (uid, kind) = parse_key(key);
        let own = uid == self.data.uid;
        let radius = if full { 0.0 } else { 12.0 };
        let painter = ui.painter().clone();
        painter.rect_filled(tile, radius, TILE_FILL);
        // O ladrilho inteiro escuta o duplo clique (tela cheia); os botões por cima têm prioridade.
        let surface = ui.interact(tile, ui.scope_id().with(("ladrilho", key)), Sense::click());
        let hovered = ui.rect_contains_pointer(tile);
        let state = if own {
            LiveState::Playing
        } else {
            self.watching.state(key)
        };
        let playing = !own && state == LiveState::Playing && self.videos.size(key).is_some();
        if playing && let Some((width, height)) = self.videos.size(key) {
            // A imagem inteira cabe no ladrilho, sem cortar (`object-fit: contain`).
            let scale = (tile.width() / width as f32).min(tile.height() / height as f32);
            let video = Rect::from_center_size(
                tile.center(),
                vec2(width as f32 * scale, height as f32 * scale),
            );
            self.videos.paint(ui, video, key);
        } else if own {
            self.own_placeholder(ui, tile, key);
        } else {
            self.placeholder(ui, tile, key, state, actions);
        }
        if !full {
            painter.rect_stroke(
                tile,
                radius,
                (1.0, Color32::from_white_alpha(15)),
                egui::StrokeKind::Inside,
            );
        }

        // Topo: "AO VIVO", tempo no ar, o nome, o jogo, a plateia e, com o mouse em cima, fechar.
        let badge = painter.layout_no_wrap("AO VIVO".to_owned(), theme::bold(10.0), Color32::WHITE);
        let badge_rect =
            Rect::from_min_size(tile.min + vec2(10.0, 10.0), badge.size() + vec2(12.0, 4.0));
        painter.rect_filled(badge_rect, 4.0, theme::LIVE);
        painter.galley(badge_rect.min + vec2(6.0, 2.0), badge, Color32::WHITE);
        let shadow = |at: egui::Pos2, galley: std::sync::Arc<egui::Galley>| {
            painter.galley(
                at + vec2(1.0, 1.0),
                galley.clone(),
                Color32::from_black_alpha(90),
            );
            painter.galley_with_override_text_color(at, galley, Color32::WHITE);
        };
        let mut x = badge_rect.right() + 8.0;
        if let Some(time) = self.elapsed_of(key) {
            let galley = painter.layout_no_wrap(time, theme::semibold(12.0), Color32::WHITE);
            let width = galley.size().x;
            shadow(
                pos2(x, badge_rect.center().y - galley.size().y / 2.0),
                galley,
            );
            x += width + 8.0;
        }
        if kind == "camera" {
            icon(
                &painter,
                pos2(x + 7.0, badge_rect.center().y),
                Icon::Video,
                14.0,
                Color32::WHITE,
            );
            x += 19.0;
        }
        let viewers = self.viewers_of(key);
        let viewers_width = if viewers.is_empty() { 0.0 } else { 52.0 };
        let reserve = 52.0 + viewers_width;
        let name = theme::line(
            &painter,
            &self.name_of(key),
            theme::semibold(13.0),
            Color32::WHITE,
            (tile.right() - reserve - x).max(20.0),
        );
        let name_width = name.size().x;
        shadow(pos2(x, badge_rect.center().y - name.size().y / 2.0), name);
        x += name_width + 8.0;
        if let Some(game) = self.game_of(key) {
            let room = tile.right() - reserve - x;
            if room > 40.0 {
                let galley = theme::line(
                    &painter,
                    game,
                    theme::regular(12.0),
                    Color32::from_white_alpha(205),
                    room,
                );
                let width = galley.size().x;
                painter.galley(
                    pos2(x + 1.0, badge_rect.center().y - galley.size().y / 2.0 + 1.0),
                    galley.clone(),
                    Color32::from_black_alpha(90),
                );
                painter.galley(
                    pos2(x, badge_rect.center().y - galley.size().y / 2.0),
                    galley,
                    Color32::from_white_alpha(205),
                );
                x += width + 8.0;
            }
        }
        if !viewers.is_empty() {
            let chip = viewers_chip(
                &painter,
                pos2(x, badge_rect.center().y),
                viewers.len(),
                CHIP_FILL,
            );
            ui.interact(chip, ui.scope_id().with(("plateia", key)), Sense::hover())
                .on_hover_text(format!("Assistindo: {}", viewers.join(", ")));
        }
        if hovered && !full {
            let close = Rect::from_min_size(
                pos2(tile.right() - 42.0, tile.top() + 10.0),
                vec2(32.0, 32.0),
            );
            let response = ui.interact(close, ui.scope_id().with(("fechar", key)), Sense::click());
            let fill = if response.hovered() { 217 } else { 140 };
            painter.rect_filled(
                close,
                8.0,
                Color32::from_rgba_unmultiplied(10, 14, 26, fill),
            );
            icon(&painter, close.center(), Icon::X, 16.0, Color32::WHITE);
            if response
                .on_hover_cursor(CursorIcon::PointingHand)
                .on_hover_text("Fechar visualização")
                .clicked()
            {
                actions.push(Action::ToggleLive(key.to_owned()));
            }
        }
        // Embaixo: "Sem som" (com o mouse em cima) e a imagem que está chegando ("1080p · 60 fps").
        if hovered && !own && playing && !self.watching.has_sound(key) {
            let label =
                painter.layout_no_wrap("Sem som".to_owned(), theme::regular(12.0), Color32::WHITE);
            let at = pos2(
                tile.left() + 12.0 + 22.0,
                tile.bottom() - 12.0 - label.size().y,
            );
            icon(
                &painter,
                pos2(tile.left() + 12.0 + 8.0, at.y + label.size().y / 2.0),
                Icon::VolumeX,
                16.0,
                Color32::WHITE,
            );
            painter.galley(at, label, Color32::WHITE);
        }
        let mut right = tile.right() - 12.0;
        if hovered {
            // Tela cheia (ou sair dela), como o botão do site.
            let button = Rect::from_min_size(
                pos2(right - 32.0, tile.bottom() - 10.0 - 32.0),
                vec2(32.0, 32.0),
            );
            let response = ui.interact(
                button,
                ui.scope_id().with(("tela-cheia", key)),
                Sense::click(),
            );
            let fill = if response.hovered() { 217 } else { 140 };
            painter.rect_filled(
                button,
                8.0,
                Color32::from_rgba_unmultiplied(10, 14, 26, fill),
            );
            icon(
                &painter,
                button.center(),
                if full { Icon::Minimize } else { Icon::Maximize },
                16.0,
                Color32::WHITE,
            );
            let hint = if full {
                "Sair da tela cheia (Esc)"
            } else {
                "Tela cheia (duplo clique)"
            };
            if response
                .on_hover_cursor(CursorIcon::PointingHand)
                .on_hover_text(hint)
                .clicked()
            {
                actions.push(Action::Fullscreen(key.to_owned()));
            }
            right = button.left() - 8.0;
        }
        if playing && let Some(quality) = self.quality.get(key).filter(|text| !text.is_empty()) {
            let galley = painter.layout_no_wrap(
                quality.clone(),
                theme::regular(12.0),
                Color32::from_white_alpha(230),
            );
            let chip = Rect::from_min_size(
                pos2(
                    right - galley.size().x - 16.0,
                    tile.bottom() - 10.0 - 16.0 - 11.0,
                ),
                galley.size() + vec2(16.0, 6.0),
            );
            painter.rect_filled(chip, 6.0, CHIP_FILL);
            painter.galley(
                chip.min + vec2(8.0, 3.0),
                galley,
                Color32::from_white_alpha(230),
            );
        }
        if surface.double_clicked() {
            actions.push(Action::Fullscreen(key.to_owned()));
        }
    }

    /// Enquanto a imagem não chega: conectando, ou "não conectou" com o botão de tentar de novo.
    fn placeholder(
        &self,
        ui: &mut Ui,
        tile: Rect,
        key: &str,
        state: LiveState,
        actions: &mut Vec<Action>,
    ) {
        let painter = ui.painter().clone();
        let failed = state == LiveState::Failed;
        let text = if failed {
            "Não foi possível conectar a esta live"
        } else {
            "Conectando à transmissão…"
        };
        let label = painter.layout_no_wrap(text.to_owned(), theme::regular(13.0), theme::MUTED);
        let button_height = if failed { 10.0 + 36.0 } else { 0.0 };
        let top = tile.center().y - (28.0 + 10.0 + label.size().y + button_height) / 2.0;
        if failed {
            icon(
                &painter,
                pos2(tile.center().x, top + 14.0),
                Icon::WifiOff,
                28.0,
                theme::MUTED,
            );
        } else {
            let spinner =
                Rect::from_center_size(pos2(tile.center().x, top + 14.0), vec2(28.0, 28.0));
            egui::Spinner::new()
                .color(theme::MUTED)
                .paint_at(ui, spinner);
        }
        let label_height = label.size().y;
        painter.galley(
            pos2(tile.center().x - label.size().x / 2.0, top + 38.0),
            label,
            theme::MUTED,
        );
        if failed {
            let text = painter.layout_no_wrap(
                "Tentar de novo".to_owned(),
                theme::semibold(13.0),
                theme::TEXT,
            );
            let button = Rect::from_center_size(
                pos2(tile.center().x, top + 38.0 + label_height + 10.0 + 18.0),
                vec2(text.size().x + 20.0 + 32.0, 36.0),
            );
            let response = ui.interact(button, ui.scope_id().with(("tentar", key)), Sense::click());
            painter.rect_filled(
                button,
                8.0,
                if response.hovered() {
                    theme::SURFACE_3
                } else {
                    theme::SURFACE_2
                },
            );
            icon(
                &painter,
                pos2(button.left() + 16.0 + 7.0, button.center().y),
                Icon::RotateCw,
                15.0,
                theme::TEXT,
            );
            painter.galley(
                pos2(
                    button.left() + 16.0 + 20.0,
                    button.center().y - text.size().y / 2.0,
                ),
                text,
                theme::TEXT,
            );
            if response.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                actions.push(Action::RetryLive(parse_key(key).0.to_owned()));
            }
        }
    }

    /// A própria live: a última prévia (a mesma foto que os outros veem nos cartões), escurecida,
    /// com o que o motor está fazendo por cima.
    fn own_placeholder(&mut self, ui: &Ui, tile: Rect, key: &str) {
        let Some(broadcasting) = self.broadcasting else {
            return;
        };
        let painter = ui.painter();
        let ctx = ui.ctx().clone();
        if let Some(texture) = broadcasting
            .preview()
            .and_then(|data| self.images.get(&ctx, &data))
            .or_else(|| {
                let data = self.room.previews.get(key)?.clone();
                self.images.get(&ctx, &data)
            })
        {
            let [width, height] = texture.size().map(|side| side.max(1) as f32);
            let scale = (tile.width() / width).min(tile.height() / height);
            let image = Rect::from_center_size(tile.center(), vec2(width * scale, height * scale));
            painter.image(
                texture.id(),
                image,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE.gamma_multiply(0.35),
            );
        }
        let (title, detail) = match broadcasting.state() {
            BroadcastState::Starting => ("Começando a transmissão…".to_owned(), String::new()),
            BroadcastState::Failed => {
                ("A transmissão não começou".to_owned(), broadcasting.error())
            }
            BroadcastState::Stopped => ("Transmissão encerrada".to_owned(), String::new()),
            BroadcastState::Live => {
                let stats = broadcasting.stats();
                let viewers = match stats.viewers {
                    0 => "ninguém assistindo".to_owned(),
                    1 => "1 pessoa assistindo".to_owned(),
                    n => format!("{n} pessoas assistindo"),
                };
                (
                    "Você está transmitindo".to_owned(),
                    format!(
                        "{viewers} · {:.1} Mbps · {}",
                        f64::from(stats.produced_bitrate) / 1e6,
                        stats.encoder
                    ),
                )
            }
        };
        icon(
            painter,
            pos2(tile.center().x, tile.center().y - 30.0),
            Icon::MonitorUp,
            28.0,
            theme::TEXT,
        );
        painter.text(
            tile.center(),
            Align2::CENTER_CENTER,
            title,
            theme::semibold(14.0),
            theme::TEXT,
        );
        painter.text(
            pos2(tile.center().x, tile.center().y + 22.0),
            Align2::CENTER_CENTER,
            detail,
            theme::regular(12.0),
            theme::MUTED,
        );
    }
}

/// Aviso no rodapé da área principal (não deu para entrar, saiu do canal...).
pub fn notice_bar(ui: &Ui, area: Rect, notice: &str) {
    let painter = ui.painter();
    let text = painter.layout(
        notice.to_owned(),
        theme::regular(13.0),
        theme::TEXT,
        (area.width() - 80.0).min(560.0),
    );
    let size = text.size() + vec2(28.0, 20.0);
    let bar = Rect::from_center_size(
        pos2(area.center().x, area.bottom() - 24.0 - size.y / 2.0),
        size,
    );
    painter.rect_filled(bar, 10.0, theme::SURFACE_2);
    painter.rect_stroke(
        bar,
        10.0,
        (1.0, theme::DANGER.gamma_multiply(0.6)),
        egui::StrokeKind::Inside,
    );
    painter.galley(bar.min + vec2(14.0, 10.0), text, theme::TEXT);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_matches_the_site() {
        // Uma live ocupa a largura inteira, limitada pela altura.
        assert_eq!(fit_grid(1, 1600.0, 900.0), (1, 1600.0, 900.0));
        assert_eq!(fit_grid(1, 1600.0, 500.0), (1, 888.0, 499.0));
        // Duas lives numa tela larga: lado a lado.
        assert_eq!(fit_grid(2, 1610.0, 900.0).0, 2);
        // Quatro lives: duas colunas.
        assert_eq!(fit_grid(4, 1600.0, 900.0).0, 2);
        // Área estreita e alta: uma coluna.
        assert_eq!(fit_grid(2, 600.0, 1200.0).0, 1);
    }
}
