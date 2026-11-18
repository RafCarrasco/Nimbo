//! Janela de configurações de um canal ou categoria, no estilo do Discord: navegação à esquerda
//! ("Visão geral", "Permissões" e, embaixo, "Excluir"), barra de "alterações não salvas" com Desfazer
//! e Salvar, e pedido de confirmação ao fechar com alteração pendente.
//!
//! Quem pode o quê segue as regras do banco (`scripts/build-rules.mjs`): canal padrão e categoria
//! padrão não mudam de nome nem são excluídos; nível "Só o proprietário" e cargos, só o proprietário.
//! Cargos são segredo do proprietário: para administrador a aba Permissões tem só o nível mínimo.

use egui::{Color32, CursorIcon, Rect, Sense, Ui, pos2, vec2};
use lucide_icons::Icon;
use nimbo_core::admin::{self, ROLE_SLOTS, is_system_channel};
use nimbo_core::community::{DEFAULT_CATEGORY, Level, Snapshot};

use crate::images::Images;
use crate::ops::Op;
use crate::theme;
use crate::widgets::{Kind, button_at, check_box, icon, photo, role_dot, switch, text_field};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Permissions,
}

#[derive(Clone, PartialEq)]
struct Draft {
    name: String,
    description: String,
    category_id: String,
    private: bool,
    roles: Vec<String>,
    level: Level,
}

/// O que a janela pediu neste quadro.
pub enum Event {
    /// Fechar (sem nada pendente).
    Close,
    /// Fechar com alteração pendente: perguntar antes.
    AskClose,
    Save(Op),
    /// "Excluir" na navegação: perguntar antes.
    AskDelete,
}

pub struct Settings {
    pub category: bool,
    pub id: String,
    tab: Tab,
    original: Draft,
    draft: Draft,
    error: String,
    /// Acabou de salvar: até o banco devolver o novo valor (ou 3 s), a janela não volta ao antigo.
    saved: Option<std::time::Instant>,
}

impl Settings {
    pub fn open(data: &Snapshot, category: bool, id: &str, tab: Tab) -> Option<Self> {
        let draft = Self::read(data, category, id)?;
        Some(Self {
            category,
            id: id.to_owned(),
            tab,
            original: draft.clone(),
            draft,
            error: String::new(),
            saved: None,
        })
    }

    fn read(data: &Snapshot, category: bool, id: &str) -> Option<Draft> {
        let (name, description, category_id, level, roles) = if category {
            let item = data.categories.get(id)?;
            (
                item.name.clone(),
                String::new(),
                String::new(),
                data.category_access.get(id),
                data.category_roles.get(id),
            )
        } else {
            let item = data.channels.get(id)?;
            (
                item.name.clone(),
                item.description.clone(),
                item.category_id.clone(),
                data.channel_access.get(id),
                data.channel_roles.get(id),
            )
        };
        let roles = roles.cloned().unwrap_or_default();
        Some(Draft {
            name,
            description,
            category_id,
            private: !roles.is_empty(),
            roles,
            level: level.copied().unwrap_or_default(),
        })
    }

    fn dirty(&self) -> bool {
        self.draft != self.original
    }

    /// Nome fixo: canal padrão e categoria padrão.
    fn fixed(&self) -> bool {
        if self.category {
            self.id == DEFAULT_CATEGORY
        } else {
            is_system_channel(&self.id)
        }
    }

    pub fn title(&self) -> &str {
        &self.original.name
    }

    /// Pedido de fechar (X, Esc, clique fora).
    fn close(&self) -> Event {
        if self.dirty() {
            Event::AskClose
        } else {
            Event::Close
        }
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        data: &Snapshot,
        images: &mut Images,
    ) -> Option<Event> {
        // Sem alteração pendente, a janela acompanha o banco (outra pessoa mudou, ou acabou de salvar).
        match Self::read(data, self.category, &self.id) {
            Some(fresh) if fresh == self.draft => {
                self.saved = None;
                self.original = fresh;
            }
            Some(_) if !self.dirty() && self.saved.is_some_and(|at| at.elapsed().as_secs() < 3) => {
            }
            Some(fresh) if !self.dirty() => {
                self.original = fresh.clone();
                self.draft = fresh;
            }
            Some(_) => {}
            // Excluída (aqui ou em outro lugar): a janela fecha.
            None => return Some(Event::Close),
        }
        let screen = ctx.content_rect();
        let size = vec2(
            820.0_f32.min(screen.width() - 40.0),
            580.0_f32.min(screen.height() - 40.0),
        );
        let frame = egui::Frame::new()
            .fill(theme::BG)
            .stroke((1.0, theme::BORDER))
            .corner_radius(14.0);
        let mut event = None;
        let modal = egui::Modal::new(egui::Id::unique(("configuracoes", self.category, &self.id)))
            .frame(frame)
            .show(ctx, |ui| {
                let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
                let nav = Rect::from_min_max(rect.min, pos2(rect.left() + 210.0, rect.bottom()));
                ui.painter().rect_filled(
                    nav,
                    egui::CornerRadius {
                        nw: 14,
                        sw: 14,
                        ne: 0,
                        se: 0,
                    },
                    theme::SIDEBAR,
                );
                self.nav(ui, nav, data, &mut event);
                let body = Rect::from_min_max(pos2(nav.right(), rect.top()), rect.max);
                self.body(ui, body, data, images, &mut event);
            });
        if event.is_none() && modal.should_close() {
            event = Some(self.close());
        }
        event
    }

    fn nav(&mut self, ui: &mut Ui, nav: Rect, data: &Snapshot, event: &mut Option<Event>) {
        let painter = ui.painter().clone();
        let kicker = if self.category { "CATEGORIA" } else { "CANAL" };
        painter.text(
            pos2(nav.left() + 18.0, nav.top() + 26.0),
            egui::Align2::LEFT_CENTER,
            kicker,
            theme::bold(10.5),
            theme::MUTED,
        );
        let name = theme::line(
            &painter,
            &self.original.name,
            theme::bold(15.0),
            theme::TEXT,
            nav.width() - 36.0,
        );
        painter.galley(pos2(nav.left() + 18.0, nav.top() + 38.0), name, theme::TEXT);
        let mut y = nav.top() + 74.0;
        for (tab, label) in [
            (Tab::Overview, "Visão geral"),
            (Tab::Permissions, "Permissões"),
        ] {
            let row =
                Rect::from_min_size(pos2(nav.left() + 10.0, y), vec2(nav.width() - 20.0, 34.0));
            let response = ui.interact(row, ui.scope_id().with(("aba", label)), Sense::click());
            let on = self.tab == tab;
            if on || response.hovered() {
                painter.rect_filled(row, 6.0, if on { theme::SURFACE_2 } else { theme::SURFACE });
            }
            let color = if on { theme::TEXT } else { theme::MUTED };
            painter.text(
                pos2(row.left() + 10.0, row.center().y),
                egui::Align2::LEFT_CENTER,
                label,
                theme::semibold(13.5),
                color,
            );
            if response.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                self.tab = tab;
            }
            y += 38.0;
        }
        if self.fixed() {
            return;
        }
        let has_channels = self.category
            && data
                .channels
                .values()
                .any(|channel| channel.category_id == self.id);
        let row = Rect::from_min_size(
            pos2(nav.left() + 10.0, nav.bottom() - 16.0 - 34.0),
            vec2(nav.width() - 20.0, 34.0),
        );
        painter.hline(row.x_range(), row.top() - 10.0, (1.0, theme::BORDER));
        let response = ui.interact(
            row,
            ui.scope_id().with("excluir"),
            if has_channels {
                Sense::hover()
            } else {
                Sense::click()
            },
        );
        if response.hovered() && !has_channels {
            painter.rect_filled(row, 6.0, theme::DANGER.gamma_multiply(0.18));
        }
        let color = if has_channels {
            theme::DANGER.gamma_multiply(0.45)
        } else {
            theme::DANGER
        };
        icon(
            &painter,
            pos2(row.left() + 10.0 + 8.0, row.center().y),
            Icon::Trash2,
            15.0,
            color,
        );
        let label = if self.category {
            "Excluir categoria"
        } else {
            "Excluir canal"
        };
        painter.text(
            pos2(row.left() + 34.0, row.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            theme::semibold(13.5),
            color,
        );
        if has_channels {
            response.on_hover_text("Mova ou exclua os canais antes");
        } else if response.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            *event = Some(Event::AskDelete);
        }
    }

    fn body(
        &mut self,
        ui: &mut Ui,
        body: Rect,
        data: &Snapshot,
        images: &mut Images,
        event: &mut Option<Event>,
    ) {
        let painter = ui.painter().clone();
        let title = match self.tab {
            Tab::Overview => "Visão geral",
            Tab::Permissions => "Permissões",
        };
        painter.text(
            pos2(body.left() + 28.0, body.top() + 34.0),
            egui::Align2::LEFT_CENTER,
            title,
            theme::bold(19.0),
            theme::TEXT,
        );
        let close = Rect::from_center_size(
            pos2(body.right() - 26.0, body.top() + 30.0),
            vec2(32.0, 32.0),
        );
        let response = ui.interact(close, ui.scope_id().with("fechar"), Sense::click());
        if response.hovered() {
            painter.rect_filled(close, 8.0, theme::SURFACE_2);
        }
        icon(
            &painter,
            close.center(),
            Icon::X,
            18.0,
            if response.hovered() {
                theme::TEXT
            } else {
                theme::MUTED
            },
        );
        if response
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text("Fechar (Esc)")
            .clicked()
        {
            *event = Some(self.close());
        }
        let bar = if self.dirty() { 64.0 } else { 0.0 };
        let content = Rect::from_min_max(
            pos2(body.left() + 28.0, body.top() + 64.0),
            pos2(body.right() - 28.0, body.bottom() - 12.0 - bar),
        );
        let mut inner = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(content)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(&mut inner, |ui| match self.tab {
                Tab::Overview => self.overview(ui, data),
                Tab::Permissions => self.permissions(ui, data, images),
            });
        if self.dirty() {
            let bar = Rect::from_min_max(
                pos2(body.left() + 16.0, body.bottom() - 12.0 - 52.0),
                pos2(body.right() - 16.0, body.bottom() - 12.0),
            );
            self.unsaved(ui, bar, data, event);
        }
    }

    fn field_label(ui: &mut Ui, text: &str) {
        ui.label(
            egui::RichText::new(text)
                .font(theme::bold(11.0))
                .color(theme::MUTED),
        );
        ui.add_space(6.0);
    }

    fn overview(&mut self, ui: &mut Ui, data: &Snapshot) {
        let width = ui.available_width().min(520.0);
        let fixed = self.fixed();
        Self::field_label(
            ui,
            if self.category {
                "NOME DA CATEGORIA"
            } else {
                "NOME DO CANAL"
            },
        );
        ui.add_enabled_ui(!fixed, |ui| {
            text_field(ui, &mut self.draft.name, width, 32, "De 2 a 32 caracteres")
        });
        if fixed {
            ui.add_space(6.0);
            let what = if self.category {
                "Categoria padrão"
            } else {
                "Canal padrão"
            };
            ui.label(
                egui::RichText::new(format!("{what}: o nome não muda."))
                    .font(theme::regular(12.5))
                    .color(theme::MUTED),
            );
        }
        if self.category {
            return;
        }
        ui.add_space(18.0);
        Self::field_label(ui, "DESCRIÇÃO");
        ui.add_enabled_ui(!fixed, |ui| {
            egui::Frame::new()
                .fill(theme::PANEL)
                .stroke((1.0, theme::BORDER))
                .corner_radius(8.0)
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.draft.description)
                            .frame(egui::Frame::NONE)
                            .desired_width(width - 22.0)
                            .desired_rows(3)
                            .char_limit(160)
                            .font(theme::regular(14.0))
                            .text_color(theme::TEXT)
                            .hint_text(
                                egui::RichText::new("Do que se trata este canal")
                                    .color(theme::MUTED),
                            ),
                    );
                });
        });
        ui.add_space(4.0);
        let count = self.draft.description.chars().count();
        ui.label(
            egui::RichText::new(format!("{count}/160"))
                .font(theme::regular(11.5))
                .color(theme::MUTED),
        );
        if fixed {
            return;
        }
        ui.add_space(14.0);
        Self::field_label(ui, "CATEGORIA");
        let categories: Vec<(String, String)> = data
            .ordered_categories()
            .into_iter()
            .filter(|(id, _)| data.owner || data.category_allowed(id, &data.uid))
            .map(|(id, category)| (id.clone(), category.name.clone()))
            .collect();
        let current = categories
            .iter()
            .find(|(id, _)| *id == self.draft.category_id)
            .map_or("—".to_owned(), |(_, name)| name.clone());
        egui::ComboBox::from_id_salt(("categoria-do-canal", &self.id))
            .width(width.min(320.0))
            .selected_text(egui::RichText::new(current).font(theme::regular(14.0)))
            .show_ui(ui, |ui| {
                for (id, name) in &categories {
                    ui.selectable_value(
                        &mut self.draft.category_id,
                        id.clone(),
                        egui::RichText::new(name).font(theme::regular(14.0)),
                    );
                }
            });
    }

    fn permissions(&mut self, ui: &mut Ui, data: &Snapshot, images: &mut Images) {
        let width = ui.available_width().min(560.0);
        let owner = data.owner;
        let what = if self.category { "categoria" } else { "canal" };
        if owner {
            // Chave "Canal privado": ligada, só os cargos marcados veem.
            let hint = ui.painter().layout(
                format!("Só quem tem um dos cargos marcados vê est{} {what}. Os demais nem sabem que existe.", if self.category { "a" } else { "e" }),
                theme::regular(12.5),
                theme::MUTED,
                width - 90.0,
            );
            let (row, _) =
                ui.allocate_exact_size(vec2(width, 32.0 + hint.size().y), Sense::hover());
            let painter = ui.painter().clone();
            icon(
                &painter,
                pos2(row.left() + 10.0, row.top() + 14.0),
                Icon::Lock,
                16.0,
                theme::TEXT,
            );
            let title = if self.category {
                "Categoria privada"
            } else {
                "Canal privado"
            };
            painter.text(
                pos2(row.left() + 28.0, row.top() + 14.0),
                egui::Align2::LEFT_CENTER,
                title,
                theme::semibold(15.0),
                theme::TEXT,
            );
            painter.galley(
                pos2(row.left() + 28.0, row.top() + 26.0),
                hint,
                theme::MUTED,
            );
            let toggle =
                Rect::from_min_size(pos2(row.right() - 46.0, row.top() + 4.0), vec2(40.0, 22.0));
            let response = ui.interact(toggle, ui.scope_id().with("privado"), Sense::click());
            switch(&painter, toggle, self.draft.private, true);
            if response.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                self.draft.private = !self.draft.private;
                if !self.draft.private {
                    self.draft.roles.clear();
                }
            }
            if self.draft.private {
                ui.add_space(8.0);
                Self::field_label(
                    ui,
                    &format!("CARGOS QUE VEEM ({}/{ROLE_SLOTS})", self.draft.roles.len()),
                );
                let roles = data.ordered_roles();
                if roles.is_empty() {
                    ui.label(
                        egui::RichText::new(
                            "Nenhum cargo criado. Crie em Gerenciar comunidade → Cargos.",
                        )
                        .font(theme::regular(13.0))
                        .color(theme::MUTED),
                    );
                }
                for (id, def) in roles {
                    let on = self.draft.roles.contains(id);
                    let full = !on && self.draft.roles.len() >= ROLE_SLOTS;
                    let (row, _) = ui.allocate_exact_size(vec2(width, 36.0), Sense::hover());
                    let response = ui.interact(
                        row,
                        ui.scope_id().with(("cargo", id)),
                        if full { Sense::hover() } else { Sense::click() },
                    );
                    let painter = ui.painter();
                    if response.hovered() && !full {
                        painter.rect_filled(row, 6.0, theme::SURFACE);
                    }
                    role_dot(
                        painter,
                        pos2(row.left() + 16.0, row.center().y),
                        theme::hex(&def.color).unwrap_or(theme::MUTED),
                    );
                    let color = if full { theme::MUTED } else { theme::TEXT };
                    painter.text(
                        pos2(row.left() + 32.0, row.center().y),
                        egui::Align2::LEFT_CENTER,
                        &def.name,
                        theme::regular(14.0),
                        color,
                    );
                    check_box(painter, pos2(row.right() - 18.0, row.center().y), on, !full);
                    let response = if full {
                        response.on_hover_text(format!("No máximo {ROLE_SLOTS} cargos"))
                    } else {
                        response
                    };
                    if !full && response.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                        if on {
                            self.draft.roles.retain(|role| role != id);
                        } else {
                            self.draft.roles.push(id.clone());
                        }
                    }
                }
                if self.draft.roles.is_empty() {
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "Marque pelo menos um cargo, ou desligue \"{}\".",
                            if self.category {
                                "Categoria privada"
                            } else {
                                "Canal privado"
                            }
                        ))
                        .font(theme::regular(12.5))
                        .color(theme::WARNING),
                    );
                }
            }
            ui.add_space(18.0);
            self.who_sees(ui, data, images, width);
        }
        ui.add_space(18.0);
        Self::field_label(ui, "NÍVEL MÍNIMO");
        let mut levels = vec![
            (Level::Member, "Todos", "Todos os membros"),
            (
                Level::Admin,
                "Administradores",
                "Administradores e proprietário",
            ),
        ];
        if owner {
            levels.push((Level::Owner, "Só o proprietário", "Somente você"));
        }
        for (level, label, hint) in levels {
            let (row, _) = ui.allocate_exact_size(vec2(width, 40.0), Sense::hover());
            let response = ui.interact(row, ui.scope_id().with(("nivel", label)), Sense::click());
            let on = self.draft.level == level;
            let painter = ui.painter();
            painter.rect_filled(
                row,
                8.0,
                if on {
                    theme::SURFACE_2
                } else if response.hovered() {
                    theme::SURFACE
                } else {
                    theme::PANEL
                },
            );
            let center = pos2(row.left() + 18.0, row.center().y);
            painter.circle_stroke(
                center,
                8.0,
                (2.0, if on { theme::PRIMARY } else { theme::MUTED }),
            );
            if on {
                painter.circle_filled(center, 4.0, theme::PRIMARY);
            }
            painter.text(
                pos2(row.left() + 36.0, row.center().y),
                egui::Align2::LEFT_CENTER,
                label,
                theme::semibold(14.0),
                theme::TEXT,
            );
            painter.text(
                pos2(row.right() - 12.0, row.center().y),
                egui::Align2::RIGHT_CENTER,
                hint,
                theme::regular(12.5),
                theme::MUTED,
            );
            if response.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                self.draft.level = level;
            }
            ui.add_space(6.0);
        }
        ui.add_space(8.0);
        let note = if self.category {
            "Canais seguem a categoria e podem ser ainda mais restritos. O acesso é verificado pelo servidor."
        } else {
            "O canal também segue a categoria dele. O acesso é verificado pelo servidor."
        };
        ui.label(
            egui::RichText::new(note)
                .font(theme::regular(12.5))
                .color(theme::MUTED),
        );
    }

    /// "Quem vê agora": quem enxerga com o rascunho aplicado (só o proprietário).
    fn who_sees(&self, ui: &mut Ui, data: &Snapshot, images: &mut Images, width: f32) {
        let mut preview = data.clone();
        let roles = if self.draft.private {
            self.draft.roles.clone()
        } else {
            Vec::new()
        };
        if self.category {
            preview
                .category_access
                .insert(self.id.clone(), self.draft.level);
            preview.category_roles.insert(self.id.clone(), roles);
        } else {
            preview
                .channel_access
                .insert(self.id.clone(), self.draft.level);
            preview.channel_roles.insert(self.id.clone(), roles);
            if let Some(channel) = preview.channels.get_mut(&self.id) {
                channel.category_id = self.draft.category_id.clone();
            }
        }
        let mut people: Vec<(String, String)> = data
            .approved
            .iter()
            .chain(std::iter::once(&data.uid))
            .filter(|uid| {
                if self.category {
                    preview.category_allowed(&self.id, uid)
                } else {
                    preview.channel_allowed(&self.id, uid)
                }
            })
            .map(|uid| {
                (
                    uid.clone(),
                    data.profiles
                        .get(uid)
                        .cloned()
                        .unwrap_or_else(|| "Ainda sem nickname".to_owned()),
                )
            })
            .collect();
        people.sort_by_key(|(_, name)| name.to_lowercase());
        people.dedup_by(|a, b| a.0 == b.0);
        Self::field_label(ui, &format!("QUEM VÊ AGORA — {}", people.len()));
        let ctx = ui.ctx().clone();
        let painter = ui.painter().clone();
        let mut x = 0.0;
        let mut row: Option<Rect> = None;
        for (uid, name) in &people {
            let label = theme::line(&painter, name, theme::regular(13.0), theme::TEXT, 160.0);
            let chip_width = 4.0 + 22.0 + 6.0 + label.size().x + 10.0;
            if row.is_none() || x + chip_width > width {
                let (rect, _) = ui.allocate_exact_size(vec2(width, 32.0), Sense::hover());
                row = Some(rect);
                x = 0.0;
            }
            let line = row.expect("linha");
            let chip = Rect::from_min_size(
                pos2(line.left() + x, line.top() + 2.0),
                vec2(chip_width, 28.0),
            );
            painter.rect_filled(chip, 14.0, theme::SURFACE_2);
            let texture = data
                .avatars
                .get(uid)
                .and_then(|source| images.get(&ctx, source));
            photo(
                &painter,
                pos2(chip.left() + 4.0 + 11.0, chip.center().y),
                22.0,
                name,
                texture.as_ref(),
            );
            painter.galley(
                pos2(chip.left() + 32.0, chip.center().y - label.size().y / 2.0),
                label,
                theme::TEXT,
            );
            x += chip_width + 6.0;
        }
        if people.is_empty() {
            ui.label(
                egui::RichText::new("Ninguém.")
                    .font(theme::regular(13.0))
                    .color(theme::MUTED),
            );
        }
    }

    fn unsaved(&mut self, ui: &mut Ui, bar: Rect, data: &Snapshot, event: &mut Option<Event>) {
        let painter = ui.painter().clone();
        painter.rect_filled(
            bar.translate(vec2(0.0, 4.0)),
            10.0,
            Color32::from_black_alpha(70),
        );
        painter.rect_filled(bar, 10.0, Color32::from_rgb(0x11, 0x17, 0x2a));
        painter.rect_stroke(bar, 10.0, (1.0, theme::BORDER), egui::StrokeKind::Inside);
        let problem = self.problem();
        let text = problem.unwrap_or("Você tem alterações não salvas.");
        let color = if problem.is_some() {
            theme::WARNING
        } else {
            theme::TEXT
        };
        let save = Rect::from_min_size(
            pos2(bar.right() - 12.0 - 150.0, bar.center().y - 17.0),
            vec2(150.0, 34.0),
        );
        let undo = Rect::from_min_size(
            pos2(save.left() - 8.0 - 92.0, bar.center().y - 17.0),
            vec2(92.0, 34.0),
        );
        let label = theme::line(
            &painter,
            text,
            theme::regular(13.5),
            color,
            undo.left() - bar.left() - 28.0,
        );
        painter.galley(
            pos2(bar.left() + 16.0, bar.center().y - label.size().y / 2.0),
            label,
            color,
        );
        if button_at(
            ui,
            undo,
            ui.scope_id().with("desfazer"),
            "Desfazer",
            None,
            Kind::Ghost,
            true,
        )
        .clicked()
        {
            self.draft = self.original.clone();
            self.error.clear();
        }
        let ok = problem.is_none();
        let response = button_at(
            ui,
            save,
            ui.scope_id().with("salvar"),
            "Salvar alterações",
            None,
            Kind::Primary,
            ok,
        );
        if ok
            && (response.clicked() || ui.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::S)))
        {
            *event = Some(Event::Save(self.save_op(data)));
            // Fica como salvo já: a resposta do banco chega em seguida e confirma (ou o aviso diz que não).
            self.original = self.draft.clone();
            self.saved = Some(std::time::Instant::now());
        }
    }

    /// Motivo para não deixar salvar, se houver.
    fn problem(&self) -> Option<&'static str> {
        if !self.fixed() && admin::name_error(&self.draft.name).is_some() {
            return Some("O nome precisa ter de 2 a 32 caracteres.");
        }
        // Privado sem cargo marcado: ninguém além do proprietário veria.
        if self.draft.private && self.draft.roles.is_empty() {
            return Some("Marque pelo menos um cargo para salvar.");
        }
        None
    }

    fn save_op(&self, data: &Snapshot) -> Op {
        let (old, new) = (&self.original, &self.draft);
        let mut ops = Vec::new();
        if !self.fixed()
            && (old.name != new.name
                || old.description != new.description
                || old.category_id != new.category_id)
        {
            ops.push(if self.category {
                Op::RenameCategory {
                    id: self.id.clone(),
                    name: new.name.trim().to_owned(),
                }
            } else {
                Op::EditChannel {
                    id: self.id.clone(),
                    name: new.name.trim().to_owned(),
                    description: new.description.trim().to_owned(),
                    category: new.category_id.clone(),
                }
            });
        }
        // Cargos antes do nível: ao trancar, ninguém fica vendo no meio do caminho.
        let roles = if new.private {
            new.roles.clone()
        } else {
            Vec::new()
        };
        if data.owner && roles != old.roles {
            ops.push(Op::SetRoles {
                category: self.category,
                id: self.id.clone(),
                roles,
                message: String::new(),
            });
        }
        if new.level != old.level {
            ops.push(Op::SetAccess {
                category: self.category,
                id: self.id.clone(),
                level: new.level,
            });
        }
        Op::Batch {
            ops,
            message: "Alterações salvas".to_owned(),
        }
    }

    /// O que excluir, para a caixa de confirmação.
    pub fn delete_op(&self) -> Op {
        if self.category {
            Op::DeleteCategory {
                id: self.id.clone(),
            }
        } else {
            Op::DeleteChannel {
                id: self.id.clone(),
            }
        }
    }
}
