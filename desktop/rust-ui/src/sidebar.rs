//! Barra lateral: categorias, canais, quem está em cada um e o painel da conta. Segue
//! `src/components/Sidebar.tsx` e `UserPanel.tsx` do site, com as mesmas regras do que aparece.

use std::collections::BTreeSet;

use egui::{Align2, CursorIcon, Rect, Sense, TextureHandle, Ui, pos2, vec2};
use lucide_icons::Icon;
use nimbo_core::community::Snapshot;

use crate::theme;
use crate::widgets::{avatar, icon, icon_button, icon_button_at, live_count, live_pill};

/// O que a pessoa pediu ao clicar na barra.
pub enum Action {
    ToggleCategory(String),
    Join(String),
    Watch(String, String),
    OpenAdmin,
    EditProfile,
    SignOut,
}

pub struct Sidebar<'a> {
    pub data: &'a Snapshot,
    pub collapsed: &'a BTreeSet<String>,
    pub room: &'a str,
    pub logo: &'a TextureHandle,
    pub version: &'a str,
}

impl Sidebar<'_> {
    pub fn show(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let frame = egui::Frame::new().fill(theme::SIDEBAR);
        egui::Panel::left("barra").exact_size(theme::SIDEBAR_WIDTH).resizable(false).show_separator_line(false).frame(frame).show(
            ui,
            |ui| {
                let whole = ui.max_rect();
                egui::Panel::top("barra-topo").exact_size(56.0).resizable(false).show_separator_line(false).frame(egui::Frame::new()).show(
                    ui,
                    |ui| self.head(ui, actions),
                );
                egui::Panel::bottom("barra-conta")
                    .resizable(false).show_separator_line(false)
                    .frame(egui::Frame::new().fill(theme::PANEL))
                    .show(ui, |ui| self.account(ui, actions));
                egui::CentralPanel::default().frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(8, 10))).show(
                    ui,
                    |ui| {
                        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.channels(ui, actions));
                    },
                );
                ui.painter().vline(whole.right() - 0.5, whole.y_range(), (1.0, theme::BORDER));
            },
        );
    }

    fn head(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let rect = ui.max_rect();
        let painter = ui.painter().clone();
        painter.hline(rect.x_range(), rect.bottom() - 0.5, (1.0, theme::BORDER));
        let logo = Rect::from_center_size(pos2(rect.left() + 14.0 + 15.0, rect.center().y), vec2(30.0, 30.0));
        painter.image(self.logo.id(), logo, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), egui::Color32::WHITE);
        painter.text(pos2(logo.right() + 10.0, rect.center().y), Align2::LEFT_CENTER, "Nimbo", theme::bold(16.0), theme::TEXT);
        if self.data.level.is_staff() || self.data.net_admins.contains(&self.data.uid) {
            let button = Rect::from_center_size(pos2(rect.right() - 10.0 - 16.0, rect.center().y), vec2(32.0, 32.0));
            let response = ui.interact(button, ui.scope_id().with("admin"), Sense::click());
            if icon_button_at(ui, button, response, Icon::Settings, 18.0, "Gerenciar comunidade", false).clicked() {
                actions.push(Action::OpenAdmin);
            }
        }
    }

    fn channels(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let data = self.data;
        let uid = data.uid.as_str();
        let staff = data.level.is_staff();
        let mut first = true;
        for (category_id, category) in data.ordered_categories() {
            let channels: Vec<_> = data
                .ordered_channels()
                .into_iter()
                .filter(|(id, c)| c.enabled && c.category_id == *category_id && (data.owner || data.channel_allowed(id, uid)))
                .collect();
            // Só o proprietário vê o que está trancado para ele mesmo. Administrador vê a categoria
            // vazia (para criar canais nela), desde que tenha acesso a ela.
            if !data.owner && (!data.category_allowed(category_id, uid) || (!staff && channels.is_empty())) {
                continue;
            }
            if !first {
                ui.add_space(12.0);
            }
            first = false;
            let collapsed = self.collapsed.contains(category_id);
            let lock = data.restricted_label(data.category_access.get(category_id).copied(), data.category_roles.get(category_id));
            self.category_row(ui, actions, category_id, &category.name, collapsed, &lock, staff);
            if collapsed {
                continue;
            }
            for (id, channel) in channels {
                self.channel_row(ui, actions, id, &channel.name, staff);
            }
        }
    }

    #[expect(clippy::too_many_arguments)]
    fn category_row(
        &self,
        ui: &mut Ui,
        actions: &mut Vec<Action>,
        id: &str,
        name: &str,
        collapsed: bool,
        lock: &str,
        staff: bool,
    ) {
        let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click());
        let hovered = ui.rect_contains_pointer(rect);
        let color = if response.hovered() { theme::TEXT } else { theme::MUTED };
        let painter = ui.painter().clone();
        let chevron = if collapsed { Icon::ChevronRight } else { Icon::ChevronDown };
        icon(&painter, pos2(rect.left() + 4.0 + 7.0, rect.center().y), chevron, 14.0, color);
        let reserved = if staff { 52.0 } else { 0.0 } + if lock.is_empty() { 0.0 } else { 16.0 };
        let title = theme::spaced(&painter, &name.to_uppercase(), theme::bold(11.0), color, rect.width() - 26.0 - reserved, 0.66);
        let text_at = pos2(rect.left() + 22.0, rect.center().y - title.size().y / 2.0);
        let after = text_at.x + title.size().x;
        painter.galley(text_at, title, color);
        if !lock.is_empty() {
            icon(&painter, pos2(after + 4.0 + 5.5, rect.center().y), Icon::Lock, 11.0, color);
        }
        let response = response.on_hover_cursor(CursorIcon::PointingHand);
        let response = if lock.is_empty() { response } else { response.on_hover_text(lock) };
        // Os botões da equipe aparecem ao passar o mouse, como no site. Ainda sem ação nesta etapa.
        if staff && hovered {
            for (i, (glyph, size, hint)) in [
                (Icon::Settings, 13.0, format!("Permissões de {name}")),
                (Icon::Plus, 14.0, format!("Criar canal em {name}")),
            ]
            .into_iter()
            .enumerate()
            {
                let button = Rect::from_center_size(pos2(rect.right() - 12.0 - i as f32 * 24.0, rect.center().y), vec2(24.0, 24.0));
                let button_response = ui.interact(button, ui.scope_id().with(("cat", id, i)), Sense::click());
                icon_button_at(ui, button, button_response, glyph, size, &hint, false);
            }
        }
        if response.clicked() {
            actions.push(Action::ToggleCategory(id.to_owned()));
        }
    }

    fn channel_row(&self, ui: &mut Ui, actions: &mut Vec<Action>, id: &str, name: &str, staff: bool) {
        let data = self.data;
        let uid = data.uid.as_str();
        let allowed = data.channel_allowed(id, uid);
        let empty = Default::default();
        let here = data.presence.get(id).filter(|_| allowed).unwrap_or(&empty);
        // Sem perfil: sessão abandonada, que só a equipe vê (para remover). Enquanto o perfil ainda
        // está sendo buscado, a pessoa não aparece como abandonada.
        let members: Vec<&String> = here
            .members
            .iter()
            .filter(|m| *m == uid || data.profiles.contains_key(*m) || (staff && !data.resolving.contains(*m)))
            .collect();
        let live_now = members.iter().filter(|m| here.streams.contains_key(**m)).count();
        let lock = data.restricted_label(data.channel_access.get(id).copied(), data.channel_roles.get(id));

        ui.add_space(2.0);
        let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
        let hovered = ui.rect_contains_pointer(rect);
        let active = self.room == id;
        let painter = ui.painter().clone();
        if active {
            painter.rect_filled(rect, 8.0, theme::SURFACE_2);
        } else if hovered {
            painter.rect_filled(rect, 8.0, theme::SURFACE);
        }
        let mut color = if active || (hovered && allowed) { theme::TEXT } else { theme::MUTED };
        if !allowed {
            color = color.gamma_multiply(0.5);
        }
        let (glyph, size) = if allowed { (Icon::MonitorPlay, 16.0) } else { (Icon::Lock, 15.0) };
        icon(&painter, pos2(rect.left() + 8.0 + 8.0, rect.center().y), glyph, size, color);
        let mut right = rect.right() - 8.0 - if staff { 24.0 } else { 0.0 };
        if live_now > 0 {
            right -= live_count(&painter, pos2(right, rect.center().y), live_now) + 8.0;
        }
        if !lock.is_empty() && allowed {
            icon(&painter, pos2(right - 5.5, rect.center().y), Icon::Lock, 11.0, theme::MUTED);
            right -= 11.0 + 8.0;
        }
        let label = theme::line(&painter, name, theme::regular(14.0), color, right - (rect.left() + 32.0));
        painter.galley(pos2(rect.left() + 32.0, rect.center().y - label.size().y / 2.0), label, color);
        let hint = if allowed { format!("Entrar em {name}") } else { "Você não tem acesso a este canal".to_owned() };
        let response = response.on_hover_text(hint);
        if allowed && response.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            actions.push(Action::Join(id.to_owned()));
        }
        if staff && hovered {
            let button = Rect::from_center_size(pos2(rect.right() - 4.0 - 12.0, rect.center().y), vec2(24.0, 24.0));
            let button_response = ui.interact(button, ui.scope_id().with(("chan", id)), Sense::click());
            icon_button_at(ui, button, button_response, Icon::Settings, 13.0, &format!("Permissões do canal {name}"), false);
        }

        if members.is_empty() {
            return;
        }
        ui.add_space(2.0);
        for member in members {
            let profile = data.profiles.get(member);
            let live = here.streams.get(member).copied().unwrap_or_default();
            let on_air = live.screen || live.camera;
            let (row, response) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
            let item = Rect::from_min_max(pos2(row.left() + 22.0, row.top()), row.max);
            let hovered = on_air && allowed && ui.rect_contains_pointer(item);
            if hovered {
                painter.rect_filled(item, 6.0, theme::SURFACE);
            }
            let shown = match profile {
                Some(name) if member == uid => format!("{name} (você)"),
                Some(name) => name.clone(),
                None => "Sessão sem perfil".to_owned(),
            };
            avatar(&painter, pos2(item.left() + 6.0 + 11.0, item.center().y), 22.0, profile.map_or("", String::as_str));
            let mut right = item.right() - 6.0;
            if on_air {
                right -= live_pill(&painter, pos2(right, item.center().y), live.screen, live.camera) + 8.0;
            }
            let color = data.role_color(member).and_then(theme::hex).unwrap_or(if hovered { theme::TEXT } else { theme::MUTED });
            let left = item.left() + 6.0 + 22.0 + 8.0;
            let label = theme::line(&painter, &shown, theme::regular(13.0), color, right - left);
            painter.galley(pos2(left, item.center().y - label.size().y / 2.0), label, color);
            if on_air && allowed {
                let response = response.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Assistir");
                if response.clicked() {
                    actions.push(Action::Watch(id.to_owned(), member.clone()));
                }
            }
        }
        ui.add_space(4.0);
    }

    fn account(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let data = self.data;
        let top = ui.max_rect();
        ui.painter().hline(top.x_range(), top.top() + 0.5, (1.0, theme::BORDER));
        let nickname = data.profiles.get(&data.uid).map_or("", String::as_str);
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 48.0), Sense::hover());
        let painter = ui.painter().clone();
        avatar(&painter, pos2(rect.left() + 10.0 + 16.0, rect.center().y), 32.0, nickname);
        let left = rect.left() + 10.0 + 32.0 + 8.0;
        let room = rect.right() - 8.0 - 64.0 - 8.0 - left;
        let name = theme::line(&painter, nickname, theme::bold(13.0), theme::TEXT, room);
        let level = theme::line(&painter, data.level.label(), theme::regular(12.0), theme::MUTED, room);
        let height = name.size().y + level.size().y;
        let y = rect.center().y - height / 2.0;
        let level_y = y + name.size().y;
        painter.galley(pos2(left, y), name, theme::TEXT);
        painter.galley(pos2(left, level_y), level, theme::MUTED);
        let mut buttons = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_size(pos2(rect.right() - 8.0 - 64.0, rect.center().y - 16.0), vec2(64.0, 32.0)))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        if icon_button(&mut buttons, Icon::Settings, 32.0, 16.0, "Editar perfil").clicked() {
            actions.push(Action::EditProfile);
        }
        if icon_button(&mut buttons, Icon::LogOut, 32.0, 16.0, "Sair da conta").clicked() {
            actions.push(Action::SignOut);
        }
        let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 21.0), Sense::hover());
        ui.painter().text(
            pos2(line.center().x, line.top() + 7.0),
            Align2::CENTER_CENTER,
            format!("Nimbo {}", self.version),
            theme::regular(10.5),
            theme::MUTED.gamma_multiply(0.55),
        );
    }
}
