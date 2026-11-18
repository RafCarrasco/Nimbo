//! Roteiro de cliques e teclas para os testes sem mão humana (`--clicks`). Os eventos entram na
//! entrada do egui como se viessem do mouse e do teclado, então passam pelo mesmo caminho dos
//! cliques de verdade (botão direito abre o menu, a caixa marca o cargo, Salvar grava no banco).
//!
//! Formato: passos separados por `;`, cada um `ms:tipo:valor`, com `ms` contado da abertura do app:
//! `1500:dir:120,173` (botão direito), `2500:esq:390,360` (esquerdo), `3000:duplo:700,400`,
//! `3500:tecla:Escape`, `4000:texto:Novo nome`, `4500:tudo` (Ctrl+A), `5000:mover:120,143` (só passa o mouse).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use egui::{Event, Key, Modifiers, PointerButton, Pos2, RawInput, pos2};

enum Step {
    Click(PointerButton, Pos2),
    Move(Pos2),
    Double(Pos2),
    Key(Key, Modifiers),
    Text(String),
}

pub struct Script {
    steps: VecDeque<(Duration, Step)>,
    /// Eventos já gerados, um lote por quadro (apertar e soltar em quadros diferentes).
    frames: VecDeque<Vec<Event>>,
    started: Instant,
}

fn point(text: &str) -> Option<Pos2> {
    let (x, y) = text.split_once(',')?;
    Some(pos2(x.trim().parse().ok()?, y.trim().parse().ok()?))
}

impl Script {
    pub fn parse(text: &str) -> Option<Self> {
        let mut steps = VecDeque::new();
        for part in text
            .split(';')
            .map(str::trim)
            .filter(|part| !part.is_empty())
        {
            let mut fields = part.splitn(3, ':');
            let at = Duration::from_millis(fields.next()?.trim().parse().ok()?);
            let kind = fields.next()?.trim();
            let value = fields.next().unwrap_or("");
            let step = match kind {
                "esq" => Step::Click(PointerButton::Primary, point(value)?),
                "dir" => Step::Click(PointerButton::Secondary, point(value)?),
                "duplo" => Step::Double(point(value)?),
                "mover" => Step::Move(point(value)?),
                "tecla" => Step::Key(Key::from_name(value.trim())?, Modifiers::NONE),
                "tudo" => Step::Key(Key::A, Modifiers::COMMAND),
                "texto" => Step::Text(value.to_owned()),
                _ => return None,
            };
            steps.push_back((at, step));
        }
        Some(Self {
            steps,
            frames: VecDeque::new(),
            started: Instant::now(),
        })
    }

    pub fn done(&self) -> bool {
        self.steps.is_empty() && self.frames.is_empty()
    }

    /// Chamado antes de cada quadro: põe na entrada o que está na hora.
    pub fn feed(&mut self, input: &mut RawInput) {
        if self.frames.is_empty()
            && let Some((at, _)) = self.steps.front()
            && self.started.elapsed() >= *at
            && let Some((_, step)) = self.steps.pop_front()
        {
            let press = |pos: Pos2, button: PointerButton, pressed: bool| Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers: Modifiers::NONE,
            };
            match step {
                Step::Click(button, pos) => {
                    self.frames.push_back(vec![Event::PointerMoved(pos)]);
                    self.frames.push_back(vec![press(pos, button, true)]);
                    self.frames.push_back(vec![press(pos, button, false)]);
                }
                Step::Double(pos) => {
                    self.frames.push_back(vec![Event::PointerMoved(pos)]);
                    for _ in 0..2 {
                        self.frames
                            .push_back(vec![press(pos, PointerButton::Primary, true)]);
                        self.frames
                            .push_back(vec![press(pos, PointerButton::Primary, false)]);
                    }
                }
                Step::Key(key, modifiers) => {
                    let event = |pressed| Event::Key {
                        key,
                        physical_key: None,
                        pressed,
                        repeat: false,
                        modifiers,
                    };
                    self.frames.push_back(vec![event(true)]);
                    self.frames.push_back(vec![event(false)]);
                }
                Step::Text(text) => self.frames.push_back(vec![Event::Text(text)]),
                Step::Move(pos) => self.frames.push_back(vec![Event::PointerMoved(pos)]),
            }
        }
        if let Some(events) = self.frames.pop_front() {
            input.events.extend(events);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_steps() {
        let script = Script::parse(
            "100:dir:10,20; 200:esq:5,5;300:tecla:Escape;400:texto:a:b;500:tudo;600:duplo:1,2",
        )
        .expect("roteiro");
        assert_eq!(script.steps.len(), 6);
        assert!(matches!(script.steps[3].1, Step::Text(ref text) if text == "a:b"));
        assert!(Script::parse("100:voar:1,1").is_none());
    }
}
