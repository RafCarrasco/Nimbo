//! Caixas de confirmação e de texto (`dialogs.confirm`/`dialogs.prompt` do site) e os avisos que
//! aparecem no canto e somem sozinhos (`Toast.tsx`), com um botão opcional ("Assistir").

use std::time::{Duration, Instant};

use egui::{Align2, Color32, CursorIcon, Rect, Sense, pos2, vec2};
use lucide_icons::Icon;

use crate::ops::Op;
use crate::theme;
use crate::widgets::{Kind, button, icon, text_field};

/// O que fazer quando a pessoa confirma.
pub enum Then {
    Run(Op),
    /// Criar canal nesta categoria, com o nome digitado.
    CreateChannel(String),
    CreateCategory,
    /// Fechar a janela do canal sem salvar.
    DiscardSettings,
}

pub struct Input {
    pub label: String,
    pub value: String,
    pub limit: usize,
    pub rule: fn(&str) -> Option<&'static str>,
    error: String,
}

pub struct Dialog {
    pub title: String,
    pub message: String,
    pub confirm: String,
    pub danger: bool,
    pub input: Option<Input>,
    pub then: Option<Then>,
}

impl Dialog {
    pub fn confirm(title: &str, message: &str, confirm: &str, danger: bool, then: Then) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
            confirm: confirm.into(),
            danger,
            input: None,
            then: Some(then),
        }
    }

    pub fn prompt(
        title: &str,
        label: &str,
        confirm: &str,
        rule: fn(&str) -> Option<&'static str>,
        then: Then,
    ) -> Self {
        Self {
            title: title.into(),
            message: String::new(),
            confirm: confirm.into(),
            danger: false,
            input: Some(Input {
                label: label.into(),
                value: String::new(),
                limit: 32,
                rule,
                error: String::new(),
            }),
            then: Some(then),
        }
    }

    /// Desenha a caixa. Devolve `Some` quando ela fecha: com o que fazer e o texto digitado, ou
    /// `None` dentro quando a pessoa desistiu.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<Option<(Then, String)>> {
        let frame = egui::Frame::new()
            .fill(theme::SURFACE)
            .stroke((1.0, theme::BORDER))
            .corner_radius(14.0)
            .inner_margin(egui::Margin::same(22));
        let mut outcome = None;
        let modal =
            egui::Modal::new(egui::Id::unique(("caixa", &self.title)))
                .frame(frame)
                .show(ctx, |ui| {
                    ui.set_width(380.0);
                    ui.label(
                        egui::RichText::new(&self.title)
                            .font(theme::bold(17.0))
                            .color(theme::TEXT),
                    );
                    if !self.message.is_empty() {
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new(&self.message)
                                .font(theme::regular(14.0))
                                .color(theme::MUTED),
                        );
                    }
                    let mut submit = false;
                    if let Some(input) = &mut self.input {
                        ui.add_space(12.0);
                        ui.label(
                            egui::RichText::new(&input.label)
                                .font(theme::semibold(12.5))
                                .color(theme::MUTED),
                        );
                        ui.add_space(6.0);
                        let response = text_field(ui, &mut input.value, 380.0, input.limit, "");
                        // O campo fica com o foco enquanto a caixa está aberta; Enter confirma (o
                        // aviso de que o campo perdeu o foco só chega no quadro seguinte, por isso a
                        // tecla é lida direto).
                        response.request_focus();
                        submit = ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if !input.error.is_empty() {
                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new(&input.error)
                                    .font(theme::regular(12.5))
                                    .color(theme::DANGER),
                            );
                        }
                    }
                    ui.add_space(18.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let kind = if self.danger {
                            Kind::Danger
                        } else {
                            Kind::Primary
                        };
                        if button(ui, &self.confirm, None, kind, true).clicked() {
                            submit = true;
                        }
                        ui.add_space(8.0);
                        if button(ui, "Cancelar", None, Kind::Ghost, true).clicked() {
                            outcome = Some(None);
                        }
                    });
                    if submit {
                        let value = self
                            .input
                            .as_ref()
                            .map(|input| input.value.trim().to_owned())
                            .unwrap_or_default();
                        match self.input.as_mut().and_then(|input| {
                            (input.rule)(&input.value).map(|error| (input, error))
                        }) {
                            Some((input, error)) => input.error = error.to_owned(),
                            None => outcome = Some(self.then.take().map(|then| (then, value))),
                        }
                    }
                });
        if outcome.is_none() && modal.should_close() {
            outcome = Some(None);
        }
        outcome
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Success,
    Error,
    Info,
}

struct Toast<A> {
    text: String,
    tone: Tone,
    action: Option<(String, A)>,
    until: Instant,
}

/// Avisos no canto de baixo, à direita. Repetidos substituem o anterior; no máximo três.
pub struct Toasts<A> {
    list: Vec<Toast<A>>,
}

impl<A: Clone> Toasts<A> {
    pub fn new() -> Self {
        Self { list: Vec::new() }
    }

    pub fn show_text(&mut self, text: impl Into<String>, tone: Tone) {
        self.push(text.into(), tone, None);
    }

    /// Aviso com botão: fica mais tempo, dá para ler e decidir.
    pub fn with_action(&mut self, text: impl Into<String>, label: &str, action: A) {
        self.push(text.into(), Tone::Info, Some((label.to_owned(), action)));
    }

    fn push(&mut self, text: String, tone: Tone, action: Option<(String, A)>) {
        let life = if action.is_some() {
            9000
        } else if tone == Tone::Error {
            6000
        } else {
            3500
        };
        self.list.retain(|toast| toast.text != text);
        if self.list.len() >= 3 {
            self.list.remove(0);
        }
        self.list.push(Toast {
            text,
            tone,
            action,
            until: Instant::now() + Duration::from_millis(life),
        });
    }

    /// Desenha os avisos; devolve a ação do botão clicado.
    pub fn draw(&mut self, ctx: &egui::Context) -> Option<A> {
        let now = Instant::now();
        self.list.retain(|toast| toast.until > now);
        if self.list.is_empty() {
            return None;
        }
        ctx.request_repaint_after(Duration::from_millis(250));
        let screen = ctx.content_rect();
        let mut picked = None;
        let mut close = None;
        egui::Area::new(egui::Id::unique("avisos"))
            .order(egui::Order::Tooltip)
            .anchor(Align2::RIGHT_BOTTOM, vec2(-16.0, -16.0))
            .interactable(true)
            .show(ctx, |ui| {
                ui.set_width(360.0_f32.min(screen.width() - 32.0));
                for (index, toast) in self.list.iter().enumerate() {
                    if index > 0 {
                        ui.add_space(8.0);
                    }
                    let width = ui.available_width();
                    let action_width = toast.action.as_ref().map_or(0.0, |(label, _)| {
                        ui.painter()
                            .layout_no_wrap(label.clone(), theme::semibold(12.5), Color32::WHITE)
                            .size()
                            .x
                            + 24.0
                            + 8.0
                    });
                    let text_width = width - 42.0 - 44.0 - action_width;
                    let text = ui.painter().layout(
                        toast.text.clone(),
                        theme::regular(13.5),
                        theme::TEXT,
                        text_width,
                    );
                    let height = (text.size().y + 20.0).max(46.0);
                    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
                    let painter = ui.painter();
                    let (accent, glyph) = match toast.tone {
                        Tone::Success => (theme::SUCCESS, Icon::CircleCheck),
                        Tone::Error => (theme::DANGER, Icon::CircleAlert),
                        Tone::Info => (theme::PRIMARY, Icon::Info),
                    };
                    painter.rect_filled(
                        rect.translate(vec2(0.0, 6.0)),
                        10.0,
                        Color32::from_black_alpha(60),
                    );
                    painter.rect_filled(rect, 10.0, theme::SURFACE_2);
                    painter.rect_stroke(rect, 10.0, (1.0, theme::BORDER), egui::StrokeKind::Inside);
                    painter.rect_filled(
                        Rect::from_min_size(rect.min, vec2(3.0, rect.height())),
                        2.0,
                        accent,
                    );
                    icon(
                        painter,
                        pos2(rect.left() + 14.0 + 9.0, rect.center().y),
                        glyph,
                        18.0,
                        accent,
                    );
                    painter.galley(
                        pos2(rect.left() + 42.0, rect.center().y - text.size().y / 2.0),
                        text,
                        theme::TEXT,
                    );
                    let close_rect = Rect::from_center_size(
                        pos2(rect.right() - 8.0 - 14.0, rect.center().y),
                        vec2(28.0, 28.0),
                    );
                    let response = ui.interact(
                        close_rect,
                        ui.scope_id().with(("fechar-aviso", index)),
                        Sense::click(),
                    );
                    if response.hovered() {
                        ui.painter().rect_filled(close_rect, 6.0, theme::SURFACE_3);
                    }
                    icon(
                        ui.painter(),
                        close_rect.center(),
                        Icon::X,
                        16.0,
                        theme::MUTED,
                    );
                    if response.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                        close = Some(index);
                    }
                    if let Some((label, action)) = &toast.action {
                        let galley = ui.painter().layout_no_wrap(
                            label.clone(),
                            theme::semibold(12.5),
                            Color32::WHITE,
                        );
                        let button = Rect::from_min_size(
                            pos2(
                                close_rect.left() - 8.0 - galley.size().x - 24.0,
                                rect.center().y - 14.0,
                            ),
                            vec2(galley.size().x + 24.0, 28.0),
                        );
                        let response = ui.interact(
                            button,
                            ui.scope_id().with(("acao-aviso", index)),
                            Sense::click(),
                        );
                        let fill = if response.hovered() {
                            theme::PRIMARY_STRONG
                        } else {
                            theme::PRIMARY
                        };
                        ui.painter().rect_filled(button, 8.0, fill);
                        ui.painter().galley(
                            button.center() - galley.size() / 2.0,
                            galley,
                            Color32::WHITE,
                        );
                        if response.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                            picked = Some(action.clone());
                            close = Some(index);
                        }
                    }
                }
            });
        if let Some(index) = close {
            self.list.remove(index);
        }
        picked
    }
}
