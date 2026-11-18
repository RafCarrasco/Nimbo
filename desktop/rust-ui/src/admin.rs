//! "Gerenciar comunidade" (a engrenagem do topo da barra): Solicitações, Membros, Cargos e Canais,
//! como o `AdminPanel.tsx` do site, com o editor de cargos no estilo do Discord (lista à esquerda,
//! editor à direita com Exibição, Acesso e Membros).
//!
//! Solicitações, Membros e Cargos são só do proprietário (cargos são segredo dele); administrador vê
//! só Canais, e só os que enxerga. As gravações saem como `Action::Run` e o banco confere tudo.

use std::collections::{BTreeMap, BTreeSet};

use egui::{Align2, Color32, CursorIcon, Rect, Sense, Ui, pos2, vec2};
use lucide_icons::Icon;
use nimbo_core::admin::{ROLE_SLOTS, is_system_channel, role_name_error, valid_color};
use nimbo_core::community::{DEFAULT_CATEGORY, Level, RoleDef, Snapshot};

use crate::dialogs::{Dialog, Then};
use crate::images::Images;
use crate::ops::Op;
use crate::settings::Tab as SettingsTab;
use crate::sidebar::Action;
use crate::theme;
use crate::widgets::{Kind, button, button_at, check_box, icon, photo, role_dot, text_field};

/// Cores sugeridas para cargos: as mesmas 10 do site (`PALETTE` em `src/components/RoleEditor.tsx`). A cor
/// livre (`#rrggbb`) aceita qualquer outra.
pub const ROLE_COLORS: [&str; 10] = [
    "#7c6cff", "#3ecf8e", "#f0475a", "#f5b14c", "#4cc3f5", "#ff7ac6", "#a3e635", "#94a3b8",
    "#e879f9", "#fb923c",
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Requests,
    Members,
    Roles,
    Channels,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RoleTab {
    Display,
    Access,
    Members,
}

/// Rascunho da aba Exibição de um cargo.
struct RoleDraft {
    id: String,
    name: String,
    color: String,
    /// O que está digitado na cor livre (pode estar incompleto).
    custom: String,
}

pub struct Admin {
    pub tab: Tab,
    /// Cargos marcados em cada solicitação, para aprovar a pessoa já dentro deles.
    picked: BTreeMap<String, BTreeSet<String>>,
    role: Option<String>,
    role_tab: RoleTab,
    draft: Option<RoleDraft>,
    search: String,
}

fn level_label(data: &Snapshot, uid: &str) -> &'static str {
    data.level_of(uid).label()
}

/// "há 5 min", "há 2 h", "há 3 dias".
fn ago(now_ms: i64, at_ms: f64) -> String {
    let minutes = ((now_ms as f64 - at_ms) / 60_000.0).max(0.0) as i64;
    match minutes {
        0 => "agora há pouco".to_owned(),
        1..=59 => format!("há {minutes} min"),
        60..=1439 => format!("há {} h", minutes / 60),
        _ => format!(
            "há {} dia{}",
            minutes / 1440,
            if minutes / 1440 == 1 { "" } else { "s" }
        ),
    }
}

impl Admin {
    pub fn new(data: &Snapshot) -> Self {
        let tab = if data.owner {
            Tab::Requests
        } else {
            Tab::Channels
        };
        Self {
            tab,
            picked: BTreeMap::new(),
            role: None,
            role_tab: RoleTab::Display,
            draft: None,
            search: String::new(),
        }
    }

    /// Abre o editor de um cargo (o que acabou de ser criado, por exemplo).
    pub fn select_role(&mut self, id: &str) {
        self.tab = Tab::Roles;
        self.role = Some(id.to_owned());
        self.role_tab = RoleTab::Display;
        self.draft = None;
    }

    /// Aba do editor de cargo pelo nome (para as fotos de teste): `exibicao`, `acesso` ou `membros`.
    pub fn set_role_tab(&mut self, name: &str) {
        self.role_tab = match name {
            "acesso" => RoleTab::Access,
            "membros" => RoleTab::Members,
            _ => RoleTab::Display,
        };
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        data: &Snapshot,
        images: &mut Images,
        now_ms: i64,
        actions: &mut Vec<Action>,
    ) {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BG))
            .show(ui, |ui| {
                let area = ui.max_rect();
                let painter = ui.painter().clone();
                painter.text(
                    pos2(area.left() + 28.0, area.top() + 26.0),
                    Align2::LEFT_CENTER,
                    "ADMINISTRAÇÃO",
                    theme::bold(10.5),
                    theme::MUTED,
                );
                painter.text(
                    pos2(area.left() + 28.0, area.top() + 48.0),
                    Align2::LEFT_CENTER,
                    "Gerenciar comunidade",
                    theme::bold(21.0),
                    theme::TEXT,
                );
                let close = Rect::from_center_size(
                    pos2(area.right() - 30.0, area.top() + 36.0),
                    vec2(34.0, 34.0),
                );
                let response =
                    ui.interact(close, ui.scope_id().with("fechar-admin"), Sense::click());
                if response.hovered() {
                    painter.rect_filled(close, 8.0, theme::SURFACE_2);
                }
                icon(
                    &painter,
                    close.center(),
                    Icon::X,
                    20.0,
                    if response.hovered() {
                        theme::TEXT
                    } else {
                        theme::MUTED
                    },
                );
                if response
                    .on_hover_cursor(CursorIcon::PointingHand)
                    .on_hover_text("Fechar administração")
                    .clicked()
                {
                    actions.push(Action::CloseAdmin);
                }
                // Abas.
                let pending = data
                    .requests
                    .keys()
                    .filter(|uid| !data.approved.contains(*uid))
                    .count();
                let mut tabs = Vec::new();
                if data.owner {
                    tabs.extend([
                        (Tab::Requests, "Solicitações", pending),
                        (Tab::Members, "Membros", 0),
                        (Tab::Roles, "Cargos", 0),
                    ]);
                }
                if data.level.is_staff() {
                    tabs.push((Tab::Channels, "Canais", 0));
                }
                if !tabs.iter().any(|(tab, ..)| *tab == self.tab)
                    && let Some((tab, ..)) = tabs.first()
                {
                    self.tab = *tab;
                }
                let mut x = area.left() + 28.0;
                let tabs_y = area.top() + 82.0;
                for (tab, label, count) in tabs {
                    let text = painter.layout_no_wrap(
                        label.to_owned(),
                        theme::semibold(14.0),
                        theme::TEXT,
                    );
                    let badge = if count > 0 { 8.0 + 20.0 } else { 0.0 };
                    let rect = Rect::from_min_size(
                        pos2(x, tabs_y),
                        vec2(text.size().x + badge + 20.0, 34.0),
                    );
                    let response = ui.interact(
                        rect,
                        ui.scope_id().with(("aba-admin", label)),
                        Sense::click(),
                    );
                    let on = self.tab == tab;
                    let color = if on || response.hovered() {
                        theme::TEXT
                    } else {
                        theme::MUTED
                    };
                    painter.galley_with_override_text_color(
                        pos2(rect.left() + 10.0, rect.center().y - text.size().y / 2.0),
                        text.clone(),
                        color,
                    );
                    if count > 0 {
                        let pill = Rect::from_min_size(
                            pos2(
                                rect.left() + 10.0 + text.size().x + 8.0,
                                rect.center().y - 9.0,
                            ),
                            vec2(20.0, 18.0),
                        );
                        painter.rect_filled(pill, 9.0, theme::LIVE);
                        painter.text(
                            pill.center(),
                            Align2::CENTER_CENTER,
                            count.to_string(),
                            theme::bold(11.0),
                            Color32::WHITE,
                        );
                    }
                    if on {
                        painter.hline(
                            rect.x_range().shrink(6.0),
                            rect.bottom() - 1.0,
                            (2.0, theme::PRIMARY),
                        );
                    }
                    if response.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                        self.tab = tab;
                    }
                    x = rect.right() + 4.0;
                }
                painter.hline(area.x_range(), tabs_y + 34.0, (1.0, theme::BORDER));
                let body = Rect::from_min_max(
                    pos2(area.left() + 28.0, tabs_y + 34.0 + 16.0),
                    pos2(area.right() - 28.0, area.bottom() - 12.0),
                );
                let mut inner = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(body)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                match self.tab {
                    Tab::Requests => self.requests(&mut inner, data, images, now_ms, actions),
                    Tab::Members => self.members(&mut inner, data, images, actions),
                    Tab::Roles => self.roles(&mut inner, data, images, actions),
                    Tab::Channels => self.channels(&mut inner, data, actions),
                }
            });
    }

    fn note(ui: &mut Ui, text: &str) {
        let width = ui.available_width().min(760.0);
        let galley =
            ui.painter()
                .layout(text.to_owned(), theme::regular(13.0), theme::MUTED, width);
        let (rect, _) = ui.allocate_exact_size(vec2(width, galley.size().y), Sense::hover());
        ui.painter().galley(rect.min, galley, theme::MUTED);
        ui.add_space(14.0);
    }

    fn requests(
        &mut self,
        ui: &mut Ui,
        data: &Snapshot,
        _images: &mut Images,
        now_ms: i64,
        actions: &mut Vec<Action>,
    ) {
        let pending: Vec<_> = data
            .requests
            .iter()
            .filter(|(uid, _)| !data.approved.contains(*uid))
            .collect();
        if pending.is_empty() {
            Self::note(ui, "Nenhuma solicitação pendente.");
            return;
        }
        let roles = data.ordered_roles();
        if !roles.is_empty() {
            Self::note(
                ui,
                "Marque os cargos antes de aprovar: a pessoa já entra vendo os canais deles. Sem nenhum marcado, ela vê só os canais abertos a todos.",
            );
        }
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (uid, request) in pending {
                    let width = ui.available_width().min(760.0);
                    let chips_height = if roles.is_empty() { 0.0 } else { 34.0 };
                    let (card, _) =
                        ui.allocate_exact_size(vec2(width, 64.0 + chips_height), Sense::hover());
                    let painter = ui.painter().clone();
                    painter.rect_filled(card, 12.0, theme::SURFACE);
                    painter.rect_stroke(card, 12.0, (1.0, theme::BORDER), egui::StrokeKind::Inside);
                    crate::widgets::avatar(
                        &painter,
                        pos2(card.left() + 16.0 + 18.0, card.top() + 32.0),
                        36.0,
                        &request.email,
                    );
                    let left = card.left() + 16.0 + 36.0 + 12.0;
                    let reject = Rect::from_min_size(
                        pos2(card.right() - 14.0 - 104.0 - 8.0 - 92.0, card.top() + 15.0),
                        vec2(92.0, 34.0),
                    );
                    let approve = Rect::from_min_size(
                        pos2(card.right() - 14.0 - 104.0, card.top() + 15.0),
                        vec2(104.0, 34.0),
                    );
                    let email = theme::line(
                        &painter,
                        &request.email,
                        theme::bold(14.0),
                        theme::TEXT,
                        reject.left() - 12.0 - left,
                    );
                    painter.galley(pos2(left, card.top() + 13.0), email, theme::TEXT);
                    let when = format!("Pediu acesso {}", ago(now_ms, request.created_at));
                    painter.text(
                        pos2(left, card.top() + 44.0),
                        Align2::LEFT_CENTER,
                        when,
                        theme::regular(12.5),
                        theme::MUTED,
                    );
                    let chosen = self.picked.entry(uid.clone()).or_default();
                    if button_at(
                        ui,
                        reject,
                        ui.scope_id().with(("recusar", uid)),
                        "Recusar",
                        None,
                        Kind::Ghost,
                        true,
                    )
                    .clicked()
                    {
                        actions.push(Action::Run(Op::Reject { uid: uid.clone() }));
                    }
                    if button_at(
                        ui,
                        approve,
                        ui.scope_id().with(("aprovar", uid)),
                        "Aprovar",
                        Some(Icon::Check),
                        Kind::Primary,
                        true,
                    )
                    .clicked()
                    {
                        let ids: Vec<String> = roles
                            .iter()
                            .filter(|(id, _)| chosen.contains(*id))
                            .map(|(id, _)| (*id).clone())
                            .collect();
                        let names: Vec<&str> = roles
                            .iter()
                            .filter(|(id, _)| chosen.contains(*id))
                            .map(|(_, def)| def.name.as_str())
                            .collect();
                        let message = if names.is_empty() {
                            "Acesso liberado".to_owned()
                        } else {
                            format!("Acesso liberado em {}", names.join(", "))
                        };
                        actions.push(Action::Run(Op::Approve {
                            uid: uid.clone(),
                            roles: ids,
                            message,
                        }));
                    }
                    let mut x = left;
                    for (id, def) in &roles {
                        let on = chosen.contains(*id);
                        let galley = painter.layout_no_wrap(
                            def.name.clone(),
                            theme::semibold(12.5),
                            theme::TEXT,
                        );
                        let chip = Rect::from_min_size(
                            pos2(x, card.top() + 62.0),
                            vec2(8.0 + 10.0 + 6.0 + galley.size().x + 10.0, 26.0),
                        );
                        if chip.right() > card.right() - 14.0 {
                            break;
                        }
                        let response = ui.interact(
                            chip,
                            ui.scope_id().with(("escolher", uid, *id)),
                            Sense::click(),
                        );
                        let color = theme::hex(&def.color).unwrap_or(theme::MUTED);
                        painter.rect_filled(
                            chip,
                            13.0,
                            if on {
                                color.gamma_multiply(0.22)
                            } else {
                                theme::SURFACE_2
                            },
                        );
                        painter.rect_stroke(
                            chip,
                            13.0,
                            (1.0, if on { color } else { theme::BORDER }),
                            egui::StrokeKind::Inside,
                        );
                        painter.circle_filled(
                            pos2(chip.left() + 8.0 + 5.0, chip.center().y),
                            5.0,
                            if on { color } else { color.gamma_multiply(0.5) },
                        );
                        painter.galley(
                            pos2(chip.left() + 24.0, chip.center().y - galley.size().y / 2.0),
                            galley,
                            theme::TEXT,
                        );
                        let hint = if on {
                            "Não entrar neste cargo"
                        } else {
                            "Entrar já neste cargo"
                        };
                        if response
                            .on_hover_cursor(CursorIcon::PointingHand)
                            .on_hover_text(hint)
                            .clicked()
                        {
                            if on {
                                chosen.remove(*id);
                            } else {
                                chosen.insert((*id).clone());
                            }
                        }
                        x = chip.right() + 6.0;
                    }
                    ui.add_space(10.0);
                }
            });
    }

    fn members(
        &mut self,
        ui: &mut Ui,
        data: &Snapshot,
        images: &mut Images,
        actions: &mut Vec<Action>,
    ) {
        Self::note(
            ui,
            "Só você vê os cargos. Administrador cria e organiza os canais que enxerga, mas não entra em cargo do qual não faz parte. Clique com o botão direito numa pessoa (ou em \"…\") para dar cargos, mudar o nível ou revogar o acesso.",
        );
        let mut members: Vec<&String> = data.approved.iter().collect();
        let name_of = |uid: &str| {
            data.profiles
                .get(uid)
                .cloned()
                .unwrap_or_else(|| "Ainda sem nickname".to_owned())
        };
        members.sort_by_key(|uid| name_of(uid).to_lowercase());
        let ctx = ui.ctx().clone();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for uid in members {
                    let width = ui.available_width().min(760.0);
                    let (row, response) = ui.allocate_exact_size(vec2(width, 62.0), Sense::click());
                    let painter = ui.painter().clone();
                    if response.hovered() {
                        painter.rect_filled(row, 10.0, theme::SURFACE);
                    }
                    let name = name_of(uid);
                    let texture = data
                        .avatars
                        .get(uid)
                        .and_then(|source| images.get(&ctx, source));
                    photo(
                        &painter,
                        pos2(row.left() + 12.0 + 18.0, row.center().y),
                        36.0,
                        &name,
                        texture.as_ref(),
                    );
                    let left = row.left() + 12.0 + 36.0 + 12.0;
                    let more = Rect::from_center_size(
                        pos2(row.right() - 12.0 - 16.0, row.center().y),
                        vec2(32.0, 32.0),
                    );
                    let room = more.left() - 12.0 - left;
                    let color = data
                        .role_color(uid)
                        .and_then(theme::hex)
                        .unwrap_or(theme::TEXT);
                    let shown = if *uid == data.uid {
                        format!("{name} (você)")
                    } else {
                        name.clone()
                    };
                    let title = theme::line(&painter, &shown, theme::bold(14.0), color, room);
                    let sees = data.visible_channel_names(uid);
                    let detail = if data.level_of(uid) == Level::Owner {
                        format!("{} · vê tudo", level_label(data, uid))
                    } else if sees.is_empty() {
                        format!("{} · não vê nenhum canal", level_label(data, uid))
                    } else {
                        format!("{} · vê: {}", level_label(data, uid), sees.join(", "))
                    };
                    let detail =
                        theme::line(&painter, &detail, theme::regular(12.5), theme::MUTED, room);
                    painter.galley(pos2(left, row.top() + 11.0), title, color);
                    let mut x = left;
                    let tags_y = row.top() + 11.0 + 20.0;
                    let tags = data.roles_of(uid);
                    if tags.is_empty() {
                        painter.galley(pos2(left, tags_y), detail, theme::MUTED);
                    } else {
                        for (_, def) in tags.iter().take(4) {
                            let galley = painter.layout_no_wrap(
                                def.name.clone(),
                                theme::semibold(11.0),
                                theme::TEXT,
                            );
                            let chip = Rect::from_min_size(
                                pos2(x, tags_y),
                                vec2(6.0 + 8.0 + 5.0 + galley.size().x + 7.0, 18.0),
                            );
                            painter.rect_filled(chip, 9.0, theme::SURFACE_2);
                            painter.circle_filled(
                                pos2(chip.left() + 6.0 + 4.0, chip.center().y),
                                4.0,
                                theme::hex(&def.color).unwrap_or(theme::MUTED),
                            );
                            painter.galley(
                                pos2(chip.left() + 19.0, chip.center().y - galley.size().y / 2.0),
                                galley,
                                theme::TEXT,
                            );
                            x = chip.right() + 4.0;
                        }
                        let rest = theme::line(
                            &painter,
                            detail.text(),
                            theme::regular(12.5),
                            theme::MUTED,
                            (more.left() - 12.0 - x - 6.0).max(0.0),
                        );
                        painter.galley(
                            pos2(x + 6.0, tags_y + 9.0 - rest.size().y / 2.0),
                            rest,
                            theme::MUTED,
                        );
                    }
                    let more_response =
                        ui.interact(more, ui.scope_id().with(("mais", uid)), Sense::click());
                    if more_response.hovered() {
                        painter.rect_filled(more, 8.0, theme::SURFACE_2);
                    }
                    icon(
                        &painter,
                        more.center(),
                        Icon::Ellipsis,
                        18.0,
                        if more_response.hovered() {
                            theme::TEXT
                        } else {
                            theme::MUTED
                        },
                    );
                    if more_response
                        .on_hover_cursor(CursorIcon::PointingHand)
                        .on_hover_text("Mais opções")
                        .clicked()
                    {
                        actions.push(Action::PersonMenu {
                            channel: None,
                            uid: uid.clone(),
                            at: more.left_bottom() + vec2(0.0, 4.0),
                        });
                    } else if response.secondary_clicked() {
                        let at = response.interact_pointer_pos().unwrap_or(row.center());
                        actions.push(Action::PersonMenu {
                            channel: None,
                            uid: uid.clone(),
                            at,
                        });
                    }
                }
            });
    }

    fn roles(
        &mut self,
        ui: &mut Ui,
        data: &Snapshot,
        images: &mut Images,
        actions: &mut Vec<Action>,
    ) {
        let area = ui.max_rect();
        let list = Rect::from_min_max(area.min, pos2(area.left() + 240.0, area.bottom()));
        let roles = data.ordered_roles();
        if self
            .role
            .as_ref()
            .is_some_and(|id| !data.role_defs.contains_key(id))
        {
            self.role = None;
            self.draft = None;
        }
        if self.role.is_none() {
            self.role = roles.first().map(|(id, _)| (*id).clone());
        }
        // Lista à esquerda.
        let painter = ui.painter().clone();
        let create = Rect::from_min_size(list.min, vec2(list.width() - 12.0, 36.0));
        if button_at(
            ui,
            create,
            ui.scope_id().with("criar-cargo"),
            "Criar cargo",
            Some(Icon::Plus),
            Kind::Ghost,
            true,
        )
        .clicked()
        {
            let color = ROLE_COLORS[roles.len() % ROLE_COLORS.len()];
            actions.push(Action::Run(Op::CreateRole {
                name: "novo cargo".to_owned(),
                color: color.to_owned(),
            }));
        }
        painter.text(
            pos2(list.left() + 4.0, create.bottom() + 22.0),
            Align2::LEFT_CENTER,
            format!("CARGOS — {}", roles.len()),
            theme::bold(10.5),
            theme::MUTED,
        );
        let y = create.bottom() + 36.0;
        // A lista rola quando os cargos não cabem na altura.
        let rows = Rect::from_min_max(pos2(list.left(), y), pos2(list.right(), list.bottom()));
        let mut rows_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rows)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        egui::ScrollArea::vertical()
            .id_salt("lista-de-cargos")
            .auto_shrink([false, false])
            .show(&mut rows_ui, |ui| self.role_rows(ui, data, &roles));
        if roles.is_empty() {
            let hint = painter.layout(
                "Cargos são grupos de amigos que não se enxergam. Crie um, coloque as pessoas e marque o que ele vê.".to_owned(),
                theme::regular(12.5),
                theme::MUTED,
                list.width() - 16.0,
            );
            painter.galley(pos2(list.left() + 4.0, y + 4.0), hint, theme::MUTED);
        }
        self.role_editor(ui, area, list, data, images, actions);
    }

    /// Linhas da lista de cargos (bolinha, nome e quantas pessoas), dentro da rolagem.
    fn role_rows(&mut self, ui: &mut Ui, data: &Snapshot, roles: &[(&String, &RoleDef)]) {
        let painter = ui.painter().clone();
        for (id, def) in roles {
            let (row, _) =
                ui.allocate_exact_size(vec2(ui.available_width() - 12.0, 34.0), Sense::hover());
            ui.add_space(2.0);
            let response = ui.interact(
                row,
                ui.scope_id().with(("cargo-lista", *id)),
                Sense::click(),
            );
            let on = self.role.as_deref() == Some(id.as_str());
            if on || response.hovered() {
                painter.rect_filled(row, 6.0, if on { theme::SURFACE_2 } else { theme::SURFACE });
            }
            let color = theme::hex(&def.color).unwrap_or(theme::MUTED);
            role_dot(&painter, pos2(row.left() + 14.0, row.center().y), color);
            let count = data.role_count(id);
            let count_text =
                painter.layout_no_wrap(count.to_string(), theme::regular(12.5), theme::MUTED);
            let name = theme::line(
                &painter,
                &def.name,
                theme::semibold(13.5),
                theme::TEXT,
                row.width() - 30.0 - count_text.size().x - 14.0,
            );
            painter.galley(
                pos2(row.left() + 28.0, row.center().y - name.size().y / 2.0),
                name,
                theme::TEXT,
            );
            icon(
                &painter,
                pos2(
                    row.right() - 10.0 - count_text.size().x - 10.0,
                    row.center().y,
                ),
                Icon::Users,
                12.0,
                theme::MUTED,
            );
            painter.galley(
                pos2(
                    row.right() - 10.0 - count_text.size().x,
                    row.center().y - count_text.size().y / 2.0,
                ),
                count_text,
                theme::MUTED,
            );
            if response.on_hover_cursor(CursorIcon::PointingHand).clicked() && !on {
                self.role = Some((*id).clone());
                self.draft = None;
            }
        }
    }

    /// Editor do cargo selecionado, à direita da lista.
    fn role_editor(
        &mut self,
        ui: &mut Ui,
        area: Rect,
        list: Rect,
        data: &Snapshot,
        images: &mut Images,
        actions: &mut Vec<Action>,
    ) {
        ui.painter()
            .vline(list.right(), list.y_range(), (1.0, theme::BORDER));
        let editor = Rect::from_min_max(pos2(list.right() + 24.0, area.top()), area.max);
        let Some(id) = self.role.clone() else { return };
        let Some(def) = data.role_defs.get(&id).cloned() else {
            return;
        };
        if self.draft.as_ref().is_none_or(|draft| draft.id != id) {
            self.draft = Some(RoleDraft {
                id: id.clone(),
                name: def.name.clone(),
                color: def.color.clone(),
                custom: def.color.clone(),
            });
        }
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(editor)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        let ui = &mut child;
        let painter = ui.painter().clone();
        let head = theme::line(
            &painter,
            &format!("EDITAR CARGO — {}", def.name.to_uppercase()),
            theme::bold(12.0),
            theme::MUTED,
            editor.width() - 150.0,
        );
        painter.galley(pos2(editor.left(), editor.top() + 8.0), head, theme::MUTED);
        let delete = Rect::from_min_size(
            pos2(editor.right() - 132.0, editor.top()),
            vec2(132.0, 32.0),
        );
        if button_at(
            ui,
            delete,
            ui.scope_id().with("excluir-cargo"),
            "Excluir cargo",
            Some(Icon::Trash2),
            Kind::Danger,
            true,
        )
        .clicked()
        {
            let op = Op::DeleteRole {
                id: id.clone(),
                member_roles: data.member_roles.clone(),
                category_roles: data.category_roles.clone(),
                channel_roles: data.channel_roles.clone(),
            };
            actions.push(Action::Ask(Box::new(Dialog::confirm(
                "Excluir cargo?",
                &format!("{} sai de todas as pessoas, categorias e canais. O que só esse cargo via passa a ser visível para todos.", def.name),
                "Excluir",
                true,
                Then::Run(op),
            ))));
        }
        // Sub-abas.
        let mut x = editor.left();
        let tabs_y = editor.top() + 40.0;
        for (tab, label) in [
            (RoleTab::Display, "Exibição".to_owned()),
            (RoleTab::Access, "Acesso".to_owned()),
            (
                RoleTab::Members,
                format!("Membros ({})", data.role_count(&id)),
            ),
        ] {
            let text = painter.layout_no_wrap(label.clone(), theme::semibold(13.5), theme::TEXT);
            let rect = Rect::from_min_size(pos2(x, tabs_y), vec2(text.size().x + 20.0, 32.0));
            let response = ui.interact(
                rect,
                ui.scope_id().with(("sub-aba", label.as_str())),
                Sense::click(),
            );
            let on = self.role_tab == tab;
            let color = if on || response.hovered() {
                theme::TEXT
            } else {
                theme::MUTED
            };
            painter.galley_with_override_text_color(
                pos2(rect.left() + 10.0, rect.center().y - text.size().y / 2.0),
                text,
                color,
            );
            if on {
                painter.hline(
                    rect.x_range().shrink(6.0),
                    rect.bottom() - 1.0,
                    (2.0, theme::PRIMARY),
                );
            }
            if response.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                self.role_tab = tab;
            }
            x = rect.right() + 4.0;
        }
        painter.hline(editor.x_range(), tabs_y + 32.0, (1.0, theme::BORDER));
        let content = Rect::from_min_max(pos2(editor.left(), tabs_y + 32.0 + 16.0), editor.max);
        let mut inner = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(content)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        match self.role_tab {
            RoleTab::Display => self.display(&mut inner, &def, actions),
            RoleTab::Access => Self::access(&mut inner, data, &id, &def.name, actions),
            RoleTab::Members => {
                self.role_members(&mut inner, data, images, &id, &def.name, actions)
            }
        }
    }

    fn display(
        &mut self,
        ui: &mut Ui,
        def: &nimbo_core::community::RoleDef,
        actions: &mut Vec<Action>,
    ) {
        let Some(draft) = self.draft.as_mut() else {
            return;
        };
        let width = ui.available_width().min(460.0);
        ui.label(
            egui::RichText::new("NOME DO CARGO")
                .font(theme::bold(11.0))
                .color(theme::MUTED),
        );
        ui.add_space(6.0);
        text_field(ui, &mut draft.name, width, 24, "De 2 a 24 caracteres");
        ui.add_space(18.0);
        ui.label(
            egui::RichText::new("COR DO CARGO")
                .font(theme::bold(11.0))
                .color(theme::MUTED),
        );
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("É a cor do nome das pessoas com este cargo (só você vê).")
                .font(theme::regular(12.5))
                .color(theme::MUTED),
        );
        ui.add_space(8.0);
        let (row, _) = ui.allocate_exact_size(vec2(width, 40.0), Sense::hover());
        let painter = ui.painter().clone();
        for (index, color) in ROLE_COLORS.iter().enumerate() {
            let swatch = Rect::from_min_size(
                pos2(row.left() + index as f32 * 42.0, row.top()),
                vec2(36.0, 36.0),
            );
            let response = ui.interact(swatch, ui.scope_id().with(("cor", index)), Sense::click());
            let fill = theme::hex(color).unwrap_or(theme::MUTED);
            painter.rect_filled(swatch, 8.0, fill);
            let on = draft.color.eq_ignore_ascii_case(color);
            if on {
                painter.rect_stroke(
                    swatch.expand(2.0),
                    10.0,
                    (2.0, Color32::WHITE),
                    egui::StrokeKind::Outside,
                );
                icon(&painter, swatch.center(), Icon::Check, 18.0, Color32::WHITE);
            } else if response.hovered() {
                painter.rect_stroke(
                    swatch,
                    8.0,
                    (2.0, Color32::from_white_alpha(140)),
                    egui::StrokeKind::Inside,
                );
            }
            if response.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                draft.color = (*color).to_owned();
                draft.custom = (*color).to_owned();
            }
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Cor livre")
                    .font(theme::semibold(13.0))
                    .color(theme::TEXT),
            );
            ui.add_space(10.0);
            let before = draft.custom.clone();
            text_field(ui, &mut draft.custom, 120.0, 7, "#rrggbb");
            if draft.custom != before && valid_color(draft.custom.trim()) {
                draft.color = draft.custom.trim().to_lowercase();
            }
            ui.add_space(10.0);
            let (swatch, _) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::hover());
            ui.painter().rect_filled(
                swatch,
                8.0,
                theme::hex(&draft.color).unwrap_or(theme::MUTED),
            );
            if !valid_color(draft.custom.trim()) {
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new("Use o formato #rrggbb")
                        .font(theme::regular(12.5))
                        .color(theme::WARNING),
                );
            }
        });
        ui.add_space(18.0);
        ui.label(
            egui::RichText::new("PRÉVIA")
                .font(theme::bold(11.0))
                .color(theme::MUTED),
        );
        ui.add_space(6.0);
        let (preview, _) = ui.allocate_exact_size(vec2(width, 44.0), Sense::hover());
        let painter = ui.painter().clone();
        painter.rect_filled(preview, 10.0, theme::SIDEBAR);
        crate::widgets::avatar(
            &painter,
            pos2(preview.left() + 14.0 + 11.0, preview.center().y),
            22.0,
            "Fulano",
        );
        painter.text(
            pos2(preview.left() + 44.0, preview.center().y),
            Align2::LEFT_CENTER,
            "Fulano",
            theme::regular(13.0),
            theme::hex(&draft.color).unwrap_or(theme::TEXT),
        );
        let dirty = draft.name != def.name || !draft.color.eq_ignore_ascii_case(&def.color);
        if !dirty {
            return;
        }
        ui.add_space(20.0);
        let problem =
            role_name_error(&draft.name).map(|_| "O nome precisa ter de 2 a 24 caracteres.");
        let (bar, _) = ui.allocate_exact_size(vec2(width.max(460.0), 52.0), Sense::hover());
        painter.rect_filled(bar, 10.0, Color32::from_rgb(0x11, 0x17, 0x2a));
        painter.rect_stroke(bar, 10.0, (1.0, theme::BORDER), egui::StrokeKind::Inside);
        let text = problem.unwrap_or("Alterações não salvas.");
        painter.text(
            pos2(bar.left() + 14.0, bar.center().y),
            Align2::LEFT_CENTER,
            text,
            theme::regular(13.0),
            if problem.is_some() {
                theme::WARNING
            } else {
                theme::TEXT
            },
        );
        let save = Rect::from_min_size(
            pos2(bar.right() - 10.0 - 90.0, bar.center().y - 16.0),
            vec2(90.0, 32.0),
        );
        let undo = Rect::from_min_size(
            pos2(save.left() - 8.0 - 90.0, bar.center().y - 16.0),
            vec2(90.0, 32.0),
        );
        if button_at(
            ui,
            undo,
            ui.scope_id().with("desfazer-cargo"),
            "Desfazer",
            None,
            Kind::Ghost,
            true,
        )
        .clicked()
        {
            draft.name = def.name.clone();
            draft.color = def.color.clone();
            draft.custom = def.color.clone();
        }
        if button_at(
            ui,
            save,
            ui.scope_id().with("salvar-cargo"),
            "Salvar",
            None,
            Kind::Primary,
            problem.is_none(),
        )
        .clicked()
        {
            actions.push(Action::Run(Op::UpdateRole {
                id: draft.id.clone(),
                name: draft.name.trim().to_owned(),
                color: draft.color.clone(),
            }));
        }
    }

    /// Categorias e canais com a caixa "este cargo vê". Marcar deixa a área privada (só os cargos
    /// marcados veem); no máximo 5 cargos por área.
    fn access(
        ui: &mut Ui,
        data: &Snapshot,
        role: &str,
        role_name: &str,
        actions: &mut Vec<Action>,
    ) {
        Self::note(
            ui,
            "Marcar uma categoria ou canal aqui deixa ele privado: só quem tem um dos cargos marcados vê (você vê tudo). Canais seguem a categoria deles.",
        );
        let width = ui.available_width().min(620.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (category_id, category) in data.ordered_categories() {
                    Self::access_row(
                        ui,
                        data,
                        true,
                        category_id,
                        &category.name,
                        role,
                        role_name,
                        width,
                        actions,
                    );
                    for (channel_id, channel) in data
                        .ordered_channels()
                        .into_iter()
                        .filter(|(_, c)| c.category_id == *category_id)
                    {
                        Self::access_row(
                            ui,
                            data,
                            false,
                            channel_id,
                            &channel.name,
                            role,
                            role_name,
                            width,
                            actions,
                        );
                    }
                    ui.add_space(8.0);
                }
            });
    }

    #[expect(clippy::too_many_arguments)]
    fn access_row(
        ui: &mut Ui,
        data: &Snapshot,
        category: bool,
        id: &str,
        name: &str,
        role: &str,
        role_name: &str,
        width: f32,
        actions: &mut Vec<Action>,
    ) {
        let list = if category {
            data.category_roles.get(id)
        } else {
            data.channel_roles.get(id)
        }
        .cloned()
        .unwrap_or_default();
        let on = list.iter().any(|item| item == role);
        let full = !on && list.len() >= ROLE_SLOTS;
        let indent = if category { 0.0 } else { 24.0 };
        let (row, _) = ui.allocate_exact_size(vec2(width, 36.0), Sense::hover());
        let item = Rect::from_min_max(pos2(row.left() + indent, row.top()), row.max);
        let response = ui.interact(
            item,
            ui.scope_id().with(("vê", category, id)),
            if full { Sense::hover() } else { Sense::click() },
        );
        let painter = ui.painter();
        if response.hovered() && !full {
            painter.rect_filled(item, 6.0, theme::SURFACE);
        }
        let glyph = if category {
            Icon::Folder
        } else {
            Icon::MonitorPlay
        };
        icon(
            painter,
            pos2(item.left() + 14.0, item.center().y),
            glyph,
            15.0,
            theme::MUTED,
        );
        let font = if category {
            theme::bold(13.0)
        } else {
            theme::regular(14.0)
        };
        let shown = if category {
            name.to_uppercase()
        } else {
            name.to_owned()
        };
        painter.text(
            pos2(item.left() + 32.0, item.center().y),
            Align2::LEFT_CENTER,
            shown,
            font,
            theme::TEXT,
        );
        let others = list.len() - usize::from(on);
        if others > 0 {
            let text = format!("+{others} cargo{}", if others == 1 { "" } else { "s" });
            painter.text(
                pos2(item.right() - 44.0, item.center().y),
                Align2::RIGHT_CENTER,
                text,
                theme::regular(12.0),
                theme::MUTED,
            );
        }
        check_box(
            painter,
            pos2(item.right() - 18.0, item.center().y),
            on,
            !full,
        );
        let response = if full {
            response.on_hover_text(format!("No máximo {ROLE_SLOTS} cargos"))
        } else {
            response
        };
        if !full && response.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            let roles: Vec<String> = if on {
                list.iter().filter(|item| *item != role).cloned().collect()
            } else {
                [list, vec![role.to_owned()]].concat()
            };
            let message = if on {
                format!("{role_name} não vê mais {name}")
            } else {
                format!("{role_name} agora vê {name}")
            };
            actions.push(Action::Run(Op::SetRoles {
                category,
                id: id.to_owned(),
                roles,
                message,
            }));
        }
    }

    fn role_members(
        &mut self,
        ui: &mut Ui,
        data: &Snapshot,
        images: &mut Images,
        role: &str,
        role_name: &str,
        actions: &mut Vec<Action>,
    ) {
        let width = ui.available_width().min(560.0);
        let name_of = |uid: &str| {
            data.profiles
                .get(uid)
                .cloned()
                .unwrap_or_else(|| "Ainda sem nickname".to_owned())
        };
        ui.label(
            egui::RichText::new("ADICIONAR MEMBROS")
                .font(theme::bold(11.0))
                .color(theme::MUTED),
        );
        ui.add_space(6.0);
        text_field(ui, &mut self.search, width, 40, "Buscar pelo nome");
        let ctx = ui.ctx().clone();
        let query = self.search.trim().to_lowercase();
        let has = |uid: &str| {
            data.member_roles
                .get(uid)
                .is_some_and(|set| set.contains(role))
        };
        if !query.is_empty() {
            let mut found: Vec<&String> = data
                .approved
                .iter()
                .filter(|uid| {
                    data.level_of(uid) != Level::Owner
                        && !has(uid)
                        && name_of(uid).to_lowercase().contains(&query)
                })
                .collect();
            found.sort_by_key(|uid| name_of(uid).to_lowercase());
            ui.add_space(6.0);
            if found.is_empty() {
                ui.label(
                    egui::RichText::new("Ninguém com esse nome fora do cargo.")
                        .font(theme::regular(13.0))
                        .color(theme::MUTED),
                );
            }
            for uid in found.into_iter().take(8) {
                let name = name_of(uid);
                let (row, _) = ui.allocate_exact_size(vec2(width, 40.0), Sense::hover());
                let painter = ui.painter().clone();
                let texture = data
                    .avatars
                    .get(uid)
                    .and_then(|source| images.get(&ctx, source));
                photo(
                    &painter,
                    pos2(row.left() + 8.0 + 13.0, row.center().y),
                    26.0,
                    &name,
                    texture.as_ref(),
                );
                painter.text(
                    pos2(row.left() + 44.0, row.center().y),
                    Align2::LEFT_CENTER,
                    &name,
                    theme::regular(14.0),
                    theme::TEXT,
                );
                let add = Rect::from_min_size(
                    pos2(row.right() - 104.0, row.center().y - 15.0),
                    vec2(104.0, 30.0),
                );
                if button_at(
                    ui,
                    add,
                    ui.scope_id().with(("adicionar", uid)),
                    "Adicionar",
                    Some(Icon::Plus),
                    Kind::Primary,
                    true,
                )
                .clicked()
                {
                    let message = format!("{name} agora é {role_name}");
                    actions.push(Action::Run(Op::SetMemberRole {
                        uid: uid.clone(),
                        role: role.to_owned(),
                        on: true,
                        message,
                    }));
                }
            }
        }
        ui.add_space(16.0);
        let mut members: Vec<&String> = data
            .member_roles
            .iter()
            .filter(|(_, set)| set.contains(role))
            .map(|(uid, _)| uid)
            .collect();
        members.sort_by_key(|uid| name_of(uid).to_lowercase());
        ui.label(
            egui::RichText::new(format!("MEMBROS — {}", members.len()))
                .font(theme::bold(11.0))
                .color(theme::MUTED),
        );
        ui.add_space(6.0);
        if members.is_empty() {
            ui.label(
                egui::RichText::new("Ninguém tem este cargo ainda. Busque acima para adicionar.")
                    .font(theme::regular(13.0))
                    .color(theme::MUTED),
            );
        }
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for uid in members {
                    let name = name_of(uid);
                    let (row, response) = ui.allocate_exact_size(vec2(width, 40.0), Sense::hover());
                    let painter = ui.painter().clone();
                    if response.hovered() {
                        painter.rect_filled(row, 6.0, theme::SURFACE);
                    }
                    let texture = data
                        .avatars
                        .get(uid)
                        .and_then(|source| images.get(&ctx, source));
                    photo(
                        &painter,
                        pos2(row.left() + 8.0 + 13.0, row.center().y),
                        26.0,
                        &name,
                        texture.as_ref(),
                    );
                    let color = data
                        .role_color(uid)
                        .and_then(theme::hex)
                        .unwrap_or(theme::TEXT);
                    painter.text(
                        pos2(row.left() + 44.0, row.center().y),
                        Align2::LEFT_CENTER,
                        &name,
                        theme::regular(14.0),
                        color,
                    );
                    let remove = Rect::from_center_size(
                        pos2(row.right() - 18.0, row.center().y),
                        vec2(28.0, 28.0),
                    );
                    let remove_response =
                        ui.interact(remove, ui.scope_id().with(("remover", uid)), Sense::click());
                    if remove_response.hovered() {
                        painter.rect_filled(remove, 6.0, theme::DANGER.gamma_multiply(0.18));
                    }
                    icon(
                        &painter,
                        remove.center(),
                        Icon::X,
                        15.0,
                        if remove_response.hovered() {
                            theme::DANGER
                        } else {
                            theme::MUTED
                        },
                    );
                    if remove_response
                        .on_hover_cursor(CursorIcon::PointingHand)
                        .on_hover_text(format!("Tirar {name} de {role_name}"))
                        .clicked()
                    {
                        let message = format!("{role_name} removido de {name}");
                        actions.push(Action::Run(Op::SetMemberRole {
                            uid: uid.clone(),
                            role: role.to_owned(),
                            on: false,
                            message,
                        }));
                    }
                }
            });
    }

    fn channels(&mut self, ui: &mut Ui, data: &Snapshot, actions: &mut Vec<Action>) {
        if button(
            ui,
            "Nova categoria",
            Some(Icon::FolderPlus),
            Kind::Ghost,
            true,
        )
        .clicked()
        {
            actions.push(Action::CreateCategory);
        }
        ui.add_space(14.0);
        let width = ui.available_width().min(760.0);
        let uid = data.uid.as_str();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (category_id, category) in data.ordered_categories() {
                    // Administrador só vê (e só mexe n)o que enxerga.
                    if !data.owner && !data.category_allowed(category_id, uid) {
                        continue;
                    }
                    let channels: Vec<_> = data
                        .ordered_channels()
                        .into_iter()
                        .filter(|(id, c)| {
                            c.category_id == *category_id
                                && (data.owner || data.channel_allowed(id, uid))
                        })
                        .collect();
                    let (head, _) = ui.allocate_exact_size(vec2(width, 38.0), Sense::hover());
                    let painter = ui.painter().clone();
                    painter.rect_filled(head, 8.0, theme::SURFACE);
                    let title = theme::spaced(
                        &painter,
                        &category.name.to_uppercase(),
                        theme::bold(12.0),
                        theme::TEXT,
                        head.width() - 140.0,
                        0.6,
                    );
                    painter.galley(
                        pos2(head.left() + 12.0, head.center().y - title.size().y / 2.0),
                        title,
                        theme::TEXT,
                    );
                    let mut x = head.right() - 6.0;
                    let mut tool = |ui: &mut Ui, glyph: Icon, hint: &str, enabled: bool| -> bool {
                        let rect = Rect::from_center_size(
                            pos2(x - 14.0, head.center().y),
                            vec2(28.0, 28.0),
                        );
                        x -= 30.0;
                        let response = ui.interact(
                            rect,
                            ui.scope_id().with(("ferramenta", category_id, hint)),
                            if enabled {
                                Sense::click()
                            } else {
                                Sense::hover()
                            },
                        );
                        crate::widgets::icon_button_at(
                            ui,
                            rect,
                            response,
                            glyph,
                            14.0,
                            hint,
                            hint.starts_with("Excluir"),
                        )
                        .clicked()
                            && enabled
                    };
                    let deletable = category_id != DEFAULT_CATEGORY;
                    if deletable {
                        let empty = channels.is_empty()
                            && !data
                                .channels
                                .values()
                                .any(|c| c.category_id == *category_id);
                        let hint = if empty {
                            "Excluir categoria"
                        } else {
                            "Mova ou exclua os canais antes"
                        };
                        if tool(ui, Icon::Trash2, hint, empty) {
                            actions.push(Action::Ask(Box::new(Dialog::confirm(
                                "Excluir categoria?",
                                &category.name,
                                "Excluir",
                                true,
                                Then::Run(Op::DeleteCategory {
                                    id: category_id.clone(),
                                }),
                            ))));
                        }
                    }
                    if tool(ui, Icon::Settings, "Editar categoria", true) {
                        actions.push(Action::Settings {
                            category: true,
                            id: category_id.clone(),
                            tab: SettingsTab::Overview,
                        });
                    }
                    if tool(ui, Icon::Plus, "Novo canal", true) {
                        actions.push(Action::CreateChannel(category_id.clone()));
                    }
                    if channels.is_empty() {
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new("   Sem canais.")
                                .font(theme::regular(13.0))
                                .color(theme::MUTED),
                        );
                    }
                    for (id, channel) in channels {
                        let (row, response) =
                            ui.allocate_exact_size(vec2(width, 36.0), Sense::click());
                        let painter = ui.painter().clone();
                        if response.hovered() {
                            painter.rect_filled(row, 6.0, theme::SURFACE.gamma_multiply(0.6));
                        }
                        icon(
                            &painter,
                            pos2(row.left() + 24.0, row.center().y),
                            Icon::MonitorPlay,
                            15.0,
                            theme::MUTED,
                        );
                        painter.text(
                            pos2(row.left() + 42.0, row.center().y),
                            Align2::LEFT_CENTER,
                            &channel.name,
                            theme::regular(14.0),
                            theme::TEXT,
                        );
                        let lock = data.restricted_label(
                            data.channel_access.get(id).copied(),
                            data.channel_roles.get(id),
                        );
                        let mut tools_x = row.right() - 6.0;
                        let mut channel_tool = |ui: &mut Ui, glyph: Icon, hint: &str| -> bool {
                            let rect = Rect::from_center_size(
                                pos2(tools_x - 14.0, row.center().y),
                                vec2(28.0, 28.0),
                            );
                            tools_x -= 30.0;
                            let response = ui.interact(
                                rect,
                                ui.scope_id().with(("ferramenta-canal", id, hint)),
                                Sense::click(),
                            );
                            crate::widgets::icon_button_at(
                                ui,
                                rect,
                                response,
                                glyph,
                                14.0,
                                hint,
                                hint.starts_with("Excluir"),
                            )
                            .clicked()
                        };
                        if !is_system_channel(id) && channel_tool(ui, Icon::Trash2, "Excluir canal")
                        {
                            actions.push(Action::Ask(Box::new(Dialog::confirm(
                                "Excluir canal?",
                                &format!(
                                    "{} some para todos. Quem estiver nele será desconectado.",
                                    channel.name
                                ),
                                "Excluir",
                                true,
                                Then::Run(Op::DeleteChannel { id: id.clone() }),
                            ))));
                        }
                        if channel_tool(ui, Icon::Settings, "Editar canal") {
                            actions.push(Action::Settings {
                                category: false,
                                id: id.clone(),
                                tab: SettingsTab::Overview,
                            });
                        }
                        if !lock.is_empty() {
                            icon(
                                &painter,
                                pos2(tools_x - 12.0, row.center().y),
                                Icon::Lock,
                                12.0,
                                theme::MUTED,
                            );
                            ui.interact(
                                Rect::from_center_size(
                                    pos2(tools_x - 12.0, row.center().y),
                                    vec2(16.0, 16.0),
                                ),
                                ui.scope_id().with(("cadeado", id)),
                                Sense::hover(),
                            )
                            .on_hover_text(lock);
                        }
                        if response.secondary_clicked() {
                            let at = response.interact_pointer_pos().unwrap_or(row.center());
                            actions.push(Action::ChannelMenu { id: id.clone(), at });
                        }
                    }
                    ui.add_space(12.0);
                }
            });
    }
}
