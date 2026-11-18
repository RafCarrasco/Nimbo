//! Fotos de perfil e prévias das lives como texturas: data URL (WebP, JPEG ou PNG, como o banco
//! guarda) ou a foto da conta Google (`https://lh3.googleusercontent.com/...`, a única origem que as
//! regras do banco aceitam). A decodificação (e o download, no caso do Google) sai da thread que
//! desenha; até ficar pronta, quem desenha mostra a inicial.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use base64::Engine as _;
use egui::{ColorImage, TextureHandle, TextureOptions};

/// Maior lado de uma foto guardada como textura (as fotos do banco têm 128 px; as do Google, 96).
const MAX_SIDE: u32 = 512;
/// Fotos e prévias que ninguém desenha há tanto tempo saem da memória da placa.
const IDLE: Duration = Duration::from_secs(120);

enum Slot {
    Loading,
    Ready(TextureHandle, Instant),
    Failed,
}

/// Imagem decodificada (ou `None` quando não deu), pela chave da origem.
type Decoded = (u64, Option<ColorImage>);

pub struct Images {
    slots: HashMap<u64, Slot>,
    done: (Sender<Decoded>, Receiver<Decoded>),
    sweep: Instant,
}

fn key_of(source: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    hasher.finish()
}

/// Bytes da imagem a partir do que está no banco (data URL ou endereço do Google).
fn bytes_of(source: &str) -> Option<Vec<u8>> {
    if let Some(rest) = source.strip_prefix("data:image/") {
        let (kind, data) = rest.split_once(";base64,")?;
        if !matches!(kind, "webp" | "jpeg" | "png") {
            return None;
        }
        return base64::engine::general_purpose::STANDARD
            .decode(data.trim())
            .ok();
    }
    if source.starts_with("https://lh3.googleusercontent.com/") && source.len() <= 400 {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .build()
            .into();
        let mut response = agent.get(source).call().ok()?;
        return response
            .body_mut()
            .with_config()
            .limit(2_000_000)
            .read_to_vec()
            .ok();
    }
    None
}

fn decode(bytes: &[u8]) -> Option<ColorImage> {
    let image = image::load_from_memory(bytes).ok()?;
    let image = if image.width().max(image.height()) > MAX_SIDE {
        image.thumbnail(MAX_SIDE, MAX_SIDE)
    } else {
        image
    };
    let rgba = image.to_rgba8();
    Some(ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        rgba.as_raw(),
    ))
}

impl Images {
    pub fn new() -> Self {
        Self {
            slots: HashMap::new(),
            done: channel(),
            sweep: Instant::now(),
        }
    }

    /// A textura da imagem, se já estiver pronta. Pede a decodificação na primeira vez.
    pub fn get(&mut self, ctx: &egui::Context, source: &str) -> Option<TextureHandle> {
        if source.is_empty() {
            return None;
        }
        let key = key_of(source);
        match self.slots.get_mut(&key) {
            Some(Slot::Ready(texture, used)) => {
                *used = Instant::now();
                return Some(texture.clone());
            }
            Some(_) => return None,
            None => {}
        }
        self.slots.insert(key, Slot::Loading);
        let (tx, repaint, source) = (self.done.0.clone(), ctx.clone(), source.to_owned());
        std::thread::spawn(move || {
            let image = bytes_of(&source).and_then(|bytes| decode(&bytes));
            let _ = tx.send((key, image));
            repaint.request_repaint();
        });
        None
    }

    /// A cada quadro: guarda o que ficou pronto e solta o que ninguém usa mais.
    pub fn update(&mut self, ctx: &egui::Context) {
        while let Ok((key, image)) = self.done.1.try_recv() {
            let slot = match image {
                Some(image) => Slot::Ready(
                    ctx.load_texture(format!("foto-{key:x}"), image, TextureOptions::LINEAR),
                    Instant::now(),
                ),
                None => Slot::Failed,
            };
            self.slots.insert(key, slot);
        }
        if self.sweep.elapsed() > Duration::from_secs(10) {
            self.sweep = Instant::now();
            self.slots
                .retain(|_, slot| !matches!(slot, Slot::Ready(_, used) if used.elapsed() > IDLE));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_urls_decode_and_strangers_do_not() {
        // PNG de 1x1 vermelho.
        let png = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";
        let image = bytes_of(png).and_then(|bytes| decode(&bytes)).expect("png");
        assert_eq!(image.size, [1, 1]);
        assert!(bytes_of("data:image/svg+xml;base64,PHN2Zz4=").is_none());
        assert!(bytes_of("https://example.com/foto.png").is_none());
        assert!(bytes_of("").is_none());
    }
}
