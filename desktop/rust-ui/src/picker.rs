//! "O que você quer compartilhar?": lista de telas e janelas para transmitir. A lista e as regras do
//! que capturar vêm de `desktop/rust-native`, as mesmas do seletor do site (`SourcePicker.tsx` e
//! `desktop/rust-app/src/sources.rs`). Versão simples: nomes, sem miniaturas nem ícones.

use std::sync::mpsc::{Receiver, channel};

use egui::{Align2, CursorIcon, Sense, Ui, pos2, vec2};
use lucide_icons::Icon;
use nimbo_media::engine::{AudioSource, Source};
use sources::{PickerSource, SourceKind, list_monitors, picker_sources, window_info};

use crate::live::Capture;
use crate::theme;
use crate::widgets::icon;

struct Item {
    title: String,
    /// Nome do programa, para janelas.
    app: String,
    screen: bool,
    game: bool,
    capture: Capture,
    /// Janela minimizada: volta para a tela ao ser escolhida.
    restore: Option<isize>,
}

pub enum Choice {
    /// A pessoa escolheu o que transmitir.
    Start(Capture),
    Cancel,
}

pub struct Picker {
    items: Option<Vec<Item>>,
    loading: Receiver<Vec<Item>>,
}

fn primary_monitor() -> usize {
    list_monitors()
        .into_iter()
        .find(|monitor| monitor.primary)
        .map_or(1, |monitor| monitor.index)
}

/// Jogo em tela cheia (ou sem janela listável) é transmitido pelo monitor dele: capturar a janela
/// de um jogo em tela cheia costuma dar imagem preta ou congelada.
fn source_for(source: &PickerSource) -> Source {
    match source.kind {
        SourceKind::Screen => Source::Monitor(source.monitor),
        SourceKind::Game => Source::Monitor(primary_monitor()),
        SourceKind::Window if source.fullscreen && source.game => {
            let monitor = window_info(source.hwnd).ok().and_then(|geometry| {
                list_monitors()
                    .into_iter()
                    .find(|m| m.rect == geometry.monitor)
                    .map(|m| m.index)
            });
            Source::Monitor(monitor.unwrap_or_else(primary_monitor))
        }
        SourceKind::Window => Source::Window(source.hwnd),
    }
}

/// Som padrão da live no app, o mesmo do site (`src/live/audioDefault.ts`): só o som do programa
/// escolhido (com o som do computador inteiro, a voz da chamada entra na live e quem fala se ouve
/// com atraso); na tela inteira não há programa escolhido, e vai o som do computador, menos o do
/// próprio Nimbo e o dos apps de chamada de voz (como no Discord; app atual 0.5.7).
fn default_audio(source: &PickerSource) -> AudioSource {
    match source.kind {
        SourceKind::Window | SourceKind::Game if source.pid != 0 => {
            AudioSource::Include(source.pid)
        }
        _ => AudioSource::SystemExcept {
            own: std::process::id(),
        },
    }
}

impl Picker {
    /// Abre o seletor; a lista é levantada fora da thread que desenha.
    pub fn open(ctx: &egui::Context) -> Self {
        let (tx, loading) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let items = picker_sources(&[])
                .iter()
                .map(|source| Item {
                    title: source.title.clone(),
                    app: source
                        .exe
                        .strip_suffix(".exe")
                        .unwrap_or(&source.exe)
                        .to_owned(),
                    screen: source.kind == SourceKind::Screen,
                    game: source.game,
                    capture: Capture {
                        source: source_for(source),
                        audio: Some(default_audio(source)),
                        game: if source.game {
                            source.title.trim().chars().take(48).collect()
                        } else {
                            String::new()
                        },
                    },
                    restore: (source.minimized && source.kind == SourceKind::Window)
                        .then_some(source.hwnd),
                })
                .collect();
            let _ = tx.send(items);
            ctx.request_repaint();
        });
        Self {
            items: None,
            loading,
        }
    }

    /// Desenha o seletor por cima de tudo. Devolve a escolha quando a pessoa clica em algo.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<Choice> {
        if let Ok(items) = self.loading.try_recv() {
            self.items = Some(items);
        }
        let frame = egui::Frame::new()
            .fill(theme::SURFACE)
            .corner_radius(14.0)
            .inner_margin(egui::Margin::same(20));
        let mut choice = None;
        let modal = egui::Modal::new(egui::Id::unique("o-que-transmitir")).frame(frame).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.label(egui::RichText::new("O que você quer compartilhar?").font(theme::bold(17.0)));
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("A live leva junto só o som do programa escolhido; na tela inteira, o som do computador (menos o do Nimbo).")
                    .font(theme::regular(12.0))
                    .color(theme::MUTED),
            );
            ui.add_space(12.0);
            match &self.items {
                None => {
                    ui.add(egui::Spinner::new().size(24.0).color(theme::MUTED));
                }
                Some(items) if items.is_empty() => {
                    ui.label(egui::RichText::new("Nenhuma tela ou janela para compartilhar.").color(theme::MUTED));
                }
                Some(items) => {
                    egui::ScrollArea::vertical().max_height(380.0).auto_shrink([false, true]).show(ui, |ui| {
                        for (label, screens) in [("TELAS", true), ("APLICATIVOS", false)] {
                            if !items.iter().any(|item| item.screen == screens) {
                                continue;
                            }
                            ui.add_space(6.0);
                            ui.label(egui::RichText::new(label).font(theme::bold(11.0)).color(theme::MUTED));
                            ui.add_space(4.0);
                            for item in items.iter().filter(|item| item.screen == screens) {
                                if row(ui, item) {
                                    if let Some(window) = item.restore {
                                        sources::restore(window);
                                    }
                                    choice = Some(Choice::Start(item.capture.clone()));
                                }
                            }
                        }
                    });
                }
            }
            ui.add_space(14.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let text = ui.painter().layout_no_wrap("Cancelar".to_owned(), theme::semibold(14.0), theme::TEXT);
                let (rect, response) = ui.allocate_exact_size(vec2(text.size().x + 32.0, 36.0), Sense::click());
                ui.painter().rect_filled(rect, 8.0, if response.hovered() { theme::SURFACE_3 } else { theme::SURFACE_2 });
                ui.painter().text(rect.center(), Align2::CENTER_CENTER, "Cancelar", theme::semibold(14.0), theme::TEXT);
                if response.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    choice = Some(Choice::Cancel);
                }
            });
        });
        if choice.is_none() && modal.should_close() {
            choice = Some(Choice::Cancel);
        }
        choice
    }
}

/// Uma linha do seletor: ícone, nome e, nas janelas, o programa. Devolve `true` ao clicar.
fn row(ui: &mut Ui, item: &Item) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
    let painter = ui.painter();
    if response.hovered() {
        painter.rect_filled(rect, 8.0, theme::SURFACE_2);
    }
    let glyph = if item.screen {
        Icon::Monitor
    } else if item.game {
        Icon::Gamepad2
    } else {
        Icon::AppWindow
    };
    icon(
        painter,
        pos2(rect.left() + 12.0 + 10.0, rect.center().y),
        glyph,
        20.0,
        theme::MUTED,
    );
    let left = rect.left() + 44.0;
    let width = rect.right() - 12.0 - left;
    let title = theme::line(
        painter,
        &item.title,
        theme::regular(14.0),
        theme::TEXT,
        width,
    );
    if item.screen || item.app.is_empty() {
        painter.galley(
            pos2(left, rect.center().y - title.size().y / 2.0),
            title,
            theme::TEXT,
        );
    } else {
        let app = theme::line(
            painter,
            &item.app,
            theme::regular(12.0),
            theme::MUTED,
            width,
        );
        let top = rect.center().y - (title.size().y + app.size().y) / 2.0;
        let below = top + title.size().y;
        painter.galley(pos2(left, top), title, theme::TEXT);
        painter.galley(pos2(left, below), app, theme::MUTED);
    }
    response.on_hover_cursor(CursorIcon::PointingHand).clicked()
}
