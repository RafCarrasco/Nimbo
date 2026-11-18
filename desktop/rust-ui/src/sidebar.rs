//! Barra lateral: categorias, canais, quem está em cada um e o painel da conta. Segue
//! `src/components/Sidebar.tsx` e `UserPanel.tsx` do site, com as mesmas regras do que aparece.
//!
//! Botão direito: numa pessoa abre o menu dela; num canal ou categoria (equipe), o menu de
//! edição. Cadeados e cores de cargo só aparecem para o proprietário (cargos são segredo dele).

use std::collections::BTreeSet;

use egui::{Align2, CursorIcon, Pos2, Rect, Sense, TextureHandle, Ui, pos2, vec2};
use lucide_icons::Icon;
use nimbo_core::community::Snapshot;

use crate::dialogs::Dialog;
use crate::images::Images;
use crate::live::BroadcastState;
use crate::ops::Op;
use crate::settings::Tab as SettingsTab;
use crate::theme;
use crate::widgets::{avatar, icon, icon_button, icon_button_at, live_count, live_pill, photo};

/// O que a pessoa pediu ao clicar (na barra, no palco, nos menus e janelas).
pub enum Action {
    ToggleCategory(String),
    Join(String),
    Watch(String, String),
    Leave,
    /// Abre ou fecha uma live no palco (a chave é "uid:screen" ou "uid:camera").
    ToggleLive(String),
    /// Abre uma live no palco (o "Assistir" do aviso de live nova).
    OpenLive(String),
    /// "Tentar de novo" nas lives de uma pessoa.
    RetryLive(String),
    /// Abre o seletor do que transmitir.
    PickSource,
    StopBroadcast,
    OpenAdmin,
    CloseAdmin,
    EditProfile,
    SignOut,
    /// Menu do botão direito numa pessoa (no canal, ou na lista de membros sem canal).
    PersonMenu {
        channel: Option<String>,
        uid: String,
        at: Pos2,
    },
    ChannelMenu {
        id: String,
        at: Pos2,
    },
    CategoryMenu {
        id: String,
        at: Pos2,
    },
    /// Janela de configurações de um canal ou categoria.
    Settings {
        category: bool,
        id: String,
        tab: SettingsTab,
    },
    CreateChannel(String),
    CreateCategory,
    /// Uma gravação (cargo, nível, canal...), que roda fora da thread que desenha.
    Run(Op),
    /// Uma caixa de confirmação ou de texto antes de agir.
    Ask(Box<Dialog>),
    /// Duplo clique num ladrilho: tela cheia com aquela live (ou sai dela).
    Fullscreen(String),
}

/// Canal em que a pessoa está, para o painel de conexão acima da conta.
pub struct Connection<'a> {
    pub online: bool,
    pub channel: &'a str,
    /// Como está a transmissão desta pessoa, se houver.
    pub broadcasting: Option<BroadcastState>,
}

/// Pessoa ao vivo com o mouse em cima (para a prévia ao lado da barra).
#[derive(Clone, PartialEq)]
pub struct Hover {
    pub channel: String,
    pub uid: String,
    pub name: String,
    pub anchor: Rect,
}

pub struct Sidebar<'a> {
    pub data: &'a Snapshot,
    pub collapsed: &'a BTreeSet<String>,
    pub room: &'a str,
    pub logo: &'a TextureHandle,
    pub version: &'a str,
    pub connection: Option<Connection<'a>>,
    pub images: &'a mut Images,
    pub admin_open: bool,
    /// Sai daqui: quem está ao vivo com o mouse em cima.
    pub hover: Option<Hover>,
}

impl Sidebar<'_> {
    pub fn show(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let frame = egui::Frame::new().fill(theme::SIDEBAR);
        egui::Panel::left("barra")
            .exact_size(theme::SIDEBAR_WIDTH)
            .resizable(false)
            .show_separator_line(false)
            .frame(frame)
            .show(ui, |ui| {
                let whole = ui.max_rect();
                egui::Panel::top("barra-topo")
                    .exact_size(56.0)
                    .resizable(false)
                    .show_separator_line(false)
                    .frame(egui::Frame::new())
                    .show(ui, |ui| self.head(ui, actions));
                egui::Panel::bottom("barra-conta")
                    .resizable(false)
                    .show_separator_line(false)
                    .frame(egui::Frame::new().fill(theme::PANEL))
                    .show(ui, |ui| self.account(ui, actions));
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(8, 10)))
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| self.channels(ui, actions));
                    });
                ui.painter()
                    .vline(whole.right() - 0.5, whole.y_range(), (1.0, theme::BORDER));
            });
    }

    fn head(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let rect = ui.max_rect();
        let painter = ui.painter().clone();
        painter.hline(rect.x_range(), rect.bottom() - 0.5, (1.0, theme::BORDER));
        let logo = Rect::from_center_size(
            pos2(rect.left() + 14.0 + 15.0, rect.center().y),
            vec2(30.0, 30.0),
        );
        painter.image(
            self.logo.id(),
            logo,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
        painter.text(
            pos2(logo.right() + 10.0, rect.center().y),
            Align2::LEFT_CENTER,
            "Nimbo",
            theme::bold(16.0),
            theme::TEXT,
        );
        if self.data.level.is_staff() {
            let button = Rect::from_center_size(
                pos2(rect.right() - 10.0 - 16.0, rect.center().y),
                vec2(32.0, 32.0),
            );
            let response = ui.interact(button, ui.scope_id().with("admin"), Sense::click());
            if self.admin_open {
                painter.rect_filled(button, 8.0, theme::SURFACE_2);
            }
            let response = icon_button_at(
                ui,
                button,
                response,
                Icon::Settings,
                18.0,
                "Gerenciar comunidade",
                false,
            );
            // Solicitações esperando o proprietário: uma bolinha no canto da engrenagem.
            let pending = self
                .data
                .requests
                .keys()
                .filter(|uid| !self.data.approved.contains(*uid))
                .count();
            if pending > 0 {
                let dot = pos2(button.right() - 6.0, button.top() + 6.0);
                painter.circle_filled(dot, 7.0, theme::LIVE);
                painter.text(
                    dot,
                    Align2::CENTER_CENTER,
                    pending.min(9).to_string(),
                    theme::bold(9.5),
                    egui::Color32::WHITE,
                );
            }
            if response.clicked() {
                actions.push(if self.admin_open {
                    Action::CloseAdmin
                } else {
                    Action::OpenAdmin
                });
            }
        }
    }

    fn channels(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let data = self.data;
        let uid = data.uid.as_str();
        let staff = data.level.is_staff();
        let mut first = true;
        for (category_id, category) in data.ordered_categories() {
            let channels: Vec<_> = data
                .ordered_channels()
                .into_iter()
                .filter(|(id, c)| {
                    c.enabled
                        && c.category_id == *category_id
                        && (data.owner || data.channel_allowed(id, uid))
                })
                .collect();
            // Só o proprietário vê o que está trancado para ele mesmo. Administrador vê a categoria
            // vazia (para criar canais nela), desde que tenha acesso a ela.
            if !data.owner
                && (!data.category_allowed(category_id, uid) || (!staff && channels.is_empty()))
            {
                continue;
            }
            if !first {
                ui.add_space(12.0);
            }
            first = false;
            let collapsed = self.collapsed.contains(category_id);
            let lock = data.restricted_label(
                data.category_access.get(category_id).copied(),
                data.category_roles.get(category_id),
            );
            self.category_row(
                ui,
                actions,
                category_id,
                &category.name,
                collapsed,
                &lock,
                staff,
            );
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
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click());
        let hovered = ui.rect_contains_pointer(rect);
        let color = if response.hovered() {
            theme::TEXT
        } else {
            theme::MUTED
        };
        let painter = ui.painter().clone();
        let chevron = if collapsed {
            Icon::ChevronRight
        } else {
            Icon::ChevronDown
        };
        icon(
            &painter,
            pos2(rect.left() + 4.0 + 7.0, rect.center().y),
            chevron,
            14.0,
            color,
        );
        let reserved = if staff { 52.0 } else { 0.0 } + if lock.is_empty() { 0.0 } else { 16.0 };
        let title = theme::spaced(
            &painter,
            &name.to_uppercase(),
            theme::bold(11.0),
            color,
            rect.width() - 26.0 - reserved,
            0.66,
        );
        let text_at = pos2(rect.left() + 22.0, rect.center().y - title.size().y / 2.0);
        let after = text_at.x + title.size().x;
        painter.galley(text_at, title, color);
        if !lock.is_empty() {
            icon(
                &painter,
                pos2(after + 4.0 + 5.5, rect.center().y),
                Icon::Lock,
                11.0,
                color,
            );
        }
        let response = response.on_hover_cursor(CursorIcon::PointingHand);
        let response = if lock.is_empty() {
            response
        } else {
            response.on_hover_text(lock)
        };
        // Os botões da equipe aparecem ao passar o mouse, como no site.
        if staff && hovered {
            let buttons = [
                (Icon::Settings, 13.0, format!("Permissões de {name}")),
                (Icon::Plus, 14.0, format!("Criar canal em {name}")),
            ];
            for (i, (glyph, size, hint)) in buttons.into_iter().enumerate() {
                let button = Rect::from_center_size(
                    pos2(rect.right() - 12.0 - i as f32 * 24.0, rect.center().y),
                    vec2(24.0, 24.0),
                );
                let button_response =
                    ui.interact(button, ui.scope_id().with(("cat", id, i)), Sense::click());
                if icon_button_at(ui, button, button_response, glyph, size, &hint, false).clicked()
                {
                    actions.push(match i {
                        0 => Action::Settings {
                            category: true,
                            id: id.to_owned(),
                            tab: SettingsTab::Permissions,
                        },
                        _ => Action::CreateChannel(id.to_owned()),
                    });
                }
            }
        }
        if response.clicked() {
            actions.push(Action::ToggleCategory(id.to_owned()));
        }
        if staff && response.secondary_clicked() {
            let at = response
                .interact_pointer_pos()
                .unwrap_or(rect.left_bottom());
            actions.push(Action::CategoryMenu {
                id: id.to_owned(),
                at,
            });
        }
    }

    fn channel_row(
        &mut self,
        ui: &mut Ui,
        actions: &mut Vec<Action>,
        id: &str,
        name: &str,
        staff: bool,
    ) {
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
            .filter(|m| {
                *m == uid
                    || data.profiles.contains_key(*m)
                    || (staff && !data.resolving.contains(*m))
            })
            .collect();
        let live_now = members
            .iter()
            .filter(|m| here.streams.contains_key(**m))
            .count();
        let lock = data.restricted_label(
            data.channel_access.get(id).copied(),
            data.channel_roles.get(id),
        );

        ui.add_space(2.0);
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
        let hovered = ui.rect_contains_pointer(rect);
        let active = self.room == id;
        let painter = ui.painter().clone();
        if active {
            painter.rect_filled(rect, 8.0, theme::SURFACE_2);
        } else if hovered {
            painter.rect_filled(rect, 8.0, theme::SURFACE);
        }
        let mut color = if active || (hovered && allowed) {
            theme::TEXT
        } else {
            theme::MUTED
        };
        if !allowed {
            color = color.gamma_multiply(0.5);
        }
        let (glyph, size) = if allowed {
            (Icon::MonitorPlay, 16.0)
        } else {
            (Icon::Lock, 15.0)
        };
        icon(
            &painter,
            pos2(rect.left() + 8.0 + 8.0, rect.center().y),
            glyph,
            size,
            color,
        );
        let mut right = rect.right() - 8.0 - if staff { 24.0 } else { 0.0 };
        if live_now > 0 {
            right -= live_count(&painter, pos2(right, rect.center().y), live_now) + 8.0;
        }
        if !lock.is_empty() && allowed {
            icon(
                &painter,
                pos2(right - 5.5, rect.center().y),
                Icon::Lock,
                11.0,
                theme::MUTED,
            );
            right -= 11.0 + 8.0;
        }
        let label = theme::line(
            &painter,
            name,
            theme::regular(14.0),
            color,
            right - (rect.left() + 32.0),
        );
        painter.galley(
            pos2(rect.left() + 32.0, rect.center().y - label.size().y / 2.0),
            label,
            color,
        );
        let hint = if allowed {
            if lock.is_empty() {
                format!("Entrar em {name}")
            } else {
                format!("Entrar em {name} ({lock})")
            }
        } else {
            "Você não tem acesso a este canal".to_owned()
        };
        let response = response.on_hover_text(hint);
        if staff && response.secondary_clicked() {
            let at = response
                .interact_pointer_pos()
                .unwrap_or(rect.left_bottom());
            actions.push(Action::ChannelMenu {
                id: id.to_owned(),
                at,
            });
        }
        if allowed && response.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            actions.push(Action::Join(id.to_owned()));
        }
        if staff && hovered {
            let button = Rect::from_center_size(
                pos2(rect.right() - 4.0 - 12.0, rect.center().y),
                vec2(24.0, 24.0),
            );
            let button_response =
                ui.interact(button, ui.scope_id().with(("chan", id)), Sense::click());
            let hint = format!("Permissões do canal {name}");
            if icon_button_at(
                ui,
                button,
                button_response,
                Icon::Settings,
                13.0,
                &hint,
                false,
            )
            .clicked()
            {
                actions.push(Action::Settings {
                    category: false,
                    id: id.to_owned(),
                    tab: SettingsTab::Permissions,
                });
            }
        }

        if members.is_empty() {
            return;
        }
        ui.add_space(2.0);
        let ctx = ui.ctx().clone();
        for member in members {
            let profile = data.profiles.get(member);
            let live = here.streams.get(member).copied().unwrap_or_default();
            let on_air = live.screen || live.camera;
            let (row, response) =
                ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
            let item = Rect::from_min_max(pos2(row.left() + 22.0, row.top()), row.max);
            let pointer = ui.rect_contains_pointer(item);
            let hovered = on_air && allowed && pointer;
            if hovered {
                painter.rect_filled(item, 6.0, theme::SURFACE);
            }
            let shown = match profile {
                Some(name) if member == uid => format!("{name} (você)"),
                Some(name) => name.clone(),
                None => "Sessão sem perfil".to_owned(),
            };
            let texture = data
                .avatars
                .get(member)
                .and_then(|source| self.images.get(&ctx, source));
            let center = pos2(item.left() + 6.0 + 11.0, item.center().y);
            match profile {
                Some(name) => photo(&painter, center, 22.0, name, texture.as_ref()),
                None => avatar(&painter, center, 22.0, ""),
            }
            let mut right = item.right() - 6.0;
            if on_air {
                right -= live_pill(
                    &painter,
                    pos2(right, item.center().y),
                    live.screen,
                    live.camera,
                ) + 8.0;
            }
            let color = data
                .role_color(member)
                .and_then(theme::hex)
                .unwrap_or(if hovered { theme::TEXT } else { theme::MUTED });
            let left = item.left() + 6.0 + 22.0 + 8.0;
            let label = theme::line(&painter, &shown, theme::regular(13.0), color, right - left);
            painter.galley(
                pos2(left, item.center().y - label.size().y / 2.0),
                label,
                color,
            );
            if hovered {
                self.hover = Some(Hover {
                    channel: id.to_owned(),
                    uid: member.clone(),
                    name: profile
                        .cloned()
                        .unwrap_or_else(|| "Participante".to_owned()),
                    anchor: item,
                });
            }
            if response.secondary_clicked() {
                let at = response
                    .interact_pointer_pos()
                    .unwrap_or(item.left_bottom());
                actions.push(Action::PersonMenu {
                    channel: Some(id.to_owned()),
                    uid: member.clone(),
                    at,
                });
            } else if on_air && allowed {
                let response = response.on_hover_cursor(CursorIcon::PointingHand);
                if response.clicked() {
                    actions.push(Action::Watch(id.to_owned(), member.clone()));
                }
            }
        }
        ui.add_space(4.0);
    }

    /// "Conectado" + nome do canal + botão de sair, como o painel de voz do site.
    fn connection(&self, ui: &mut Ui, actions: &mut Vec<Action>, connection: &Connection<'_>) {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 50.0), Sense::hover());
        let painter = ui.painter().clone();
        let live = matches!(
            connection.broadcasting,
            Some(BroadcastState::Starting | BroadcastState::Live)
        );
        let (glyph, color, status) = match (connection.online, live) {
            (false, _) => (Icon::WifiOff, theme::WARNING, "Reconectando…"),
            (true, true) => (Icon::Signal, theme::LIVE, "Ao vivo"),
            (true, false) => (Icon::Signal, theme::SUCCESS, "Conectado"),
        };
        icon(
            &painter,
            pos2(rect.left() + 10.0 + 15.0, rect.center().y),
            glyph,
            16.0,
            color,
        );
        let left = rect.left() + 10.0 + 30.0 + 8.0;
        let room = rect.right() - 10.0 - 32.0 - 8.0 - left;
        let title = theme::line(&painter, status, theme::bold(13.0), color, room);
        let channel = theme::line(
            &painter,
            connection.channel,
            theme::regular(12.0),
            theme::MUTED,
            room,
        );
        let y = rect.center().y - (title.size().y + channel.size().y) / 2.0;
        let channel_y = y + title.size().y;
        painter.galley(pos2(left, y), title, color);
        painter.galley(pos2(left, channel_y), channel, theme::MUTED);
        let button = Rect::from_center_size(
            pos2(rect.right() - 10.0 - 16.0, rect.center().y),
            vec2(32.0, 32.0),
        );
        let response = ui.interact(button, ui.scope_id().with("sair-do-canal"), Sense::click());
        if icon_button_at(
            ui,
            button,
            response,
            Icon::PhoneOff,
            16.0,
            "Sair do canal",
            true,
        )
        .clicked()
        {
            actions.push(Action::Leave);
        }
        // Botão de transmitir a tela, como "Tela" / "Encerrar" no site (a webcam ainda não existe aqui).
        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::hover());
        let button = Rect::from_min_max(
            pos2(row.left() + 10.0, row.top()),
            pos2(row.right() - 10.0, row.bottom()),
        );
        let response = ui.interact(
            button,
            ui.scope_id().with("transmitir-tela"),
            Sense::click(),
        );
        let (fill, text, glyph, hint) = match (live, response.hovered()) {
            (true, hovered) => (
                theme::LIVE.gamma_multiply(if hovered { 0.85 } else { 1.0 }),
                "Encerrar",
                Icon::MonitorOff,
                "Encerrar transmissão",
            ),
            (false, true) => (
                theme::SURFACE_3,
                "Tela",
                Icon::MonitorUp,
                "Compartilhar tela",
            ),
            (false, false) => (
                theme::SURFACE_2,
                "Tela",
                Icon::MonitorUp,
                "Compartilhar tela",
            ),
        };
        ui.painter().rect_filled(button, 8.0, fill);
        let color = if live {
            egui::Color32::WHITE
        } else {
            theme::TEXT
        };
        let label = ui
            .painter()
            .layout_no_wrap(text.to_owned(), theme::semibold(13.0), color);
        let x = button.center().x - (18.0 + 6.0 + label.size().x) / 2.0;
        icon(
            ui.painter(),
            pos2(x + 9.0, button.center().y),
            glyph,
            18.0,
            color,
        );
        ui.painter().galley(
            pos2(x + 24.0, button.center().y - label.size().y / 2.0),
            label,
            color,
        );
        if response
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text(hint)
            .clicked()
        {
            actions.push(if live {
                Action::StopBroadcast
            } else {
                Action::PickSource
            });
        }
        ui.add_space(8.0);
        ui.painter().hline(
            rect.x_range(),
            ui.cursor().top() - 0.5,
            (1.0, theme::BORDER),
        );
    }

    fn account(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let data = self.data;
        let top = ui.max_rect();
        ui.painter()
            .hline(top.x_range(), top.top() + 0.5, (1.0, theme::BORDER));
        if let Some(connection) = &self.connection {
            self.connection(ui, actions, connection);
        }
        let nickname = data.profiles.get(&data.uid).map_or("", String::as_str);
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 48.0), Sense::hover());
        let painter = ui.painter().clone();
        let ctx = ui.ctx().clone();
        let texture = data
            .avatars
            .get(&data.uid)
            .and_then(|source| self.images.get(&ctx, source));
        photo(
            &painter,
            pos2(rect.left() + 10.0 + 16.0, rect.center().y),
            32.0,
            nickname,
            texture.as_ref(),
        );
        let left = rect.left() + 10.0 + 32.0 + 8.0;
        let room = rect.right() - 8.0 - 64.0 - 8.0 - left;
        let name = theme::line(&painter, nickname, theme::bold(13.0), theme::TEXT, room);
        let level = theme::line(
            &painter,
            data.level.label(),
            theme::regular(12.0),
            theme::MUTED,
            room,
        );
        let height = name.size().y + level.size().y;
        let y = rect.center().y - height / 2.0;
        let level_y = y + name.size().y;
        painter.galley(pos2(left, y), name, theme::TEXT);
        painter.galley(pos2(left, level_y), level, theme::MUTED);
        let mut buttons = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_size(
                    pos2(rect.right() - 8.0 - 64.0, rect.center().y - 16.0),
                    vec2(64.0, 32.0),
                ))
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
