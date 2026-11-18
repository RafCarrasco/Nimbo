//! Desenho do vídeo no palco. O player entrega cada quadro em NV12 (um plano de luz e um de cor
//! com metade da resolução); aqui ele vira imagem na tela.
//!
//! Caminho normal: os dois planos sobem para a placa como estão (3 MB por quadro em 1080p) e a
//! conversão para RGB acontece no desenho, num shader. Caminho de reserva (`Videos::cpu`, e quando a
//! janela não está no wgpu): a conversão é feita na CPU e o quadro sobe como uma textura comum do
//! egui (8 MB por quadro). Os dois foram medidos; os números estão em `docs/rust-app.md`.
//!
//! Cor: a live do motor chega em BT.709 de faixa completa (ver `nimbo_media::viewer::Frame`).

use std::collections::HashMap;

use eframe::egui_wgpu::{self, CallbackResources, CallbackTrait, RenderState};
use eframe::wgpu;
use egui::{Color32, ColorImage, Rect, TextureHandle, TextureOptions, Ui, pos2};
use nimbo_media::viewer::Frame;

const SHADER: &str = r"
struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) at: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) index: u32) -> Out {
    let x = f32(index & 1u);
    let y = f32((index >> 1u) & 1u);
    var out: Out;
    out.position = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    out.at = vec2<f32>(x, y);
    return out;
}

@group(0) @binding(0) var light: texture_2d<f32>;
@group(0) @binding(1) var color: texture_2d<f32>;
@group(0) @binding(2) var smooth_sampler: sampler;

// BT.709 de faixa completa: Y de 0 a 255, U e V em torno de 128.
fn rgb(at: vec2<f32>) -> vec3<f32> {
    let y = textureSample(light, smooth_sampler, at).r;
    let c = textureSample(color, smooth_sampler, at).rg - vec2<f32>(0.5019608, 0.5019608);
    let value = vec3<f32>(y + 1.5748 * c.y, y - 0.1873 * c.x - 0.4681 * c.y, y + 1.8556 * c.x);
    return clamp(value, vec3<f32>(0.0), vec3<f32>(1.0));
}

// Janela que guarda a cor como ela vai para o monitor (o caso normal do egui).
@fragment
fn fs_gamma(in: Out) -> @location(0) vec4<f32> {
    return vec4<f32>(rgb(in.at), 1.0);
}

// Janela que guarda luz linear: desfaz a curva do sRGB antes de gravar.
@fragment
fn fs_linear(in: Out) -> @location(0) vec4<f32> {
    let c = rgb(in.at);
    let low = c / 12.92;
    let high = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return vec4<f32>(select(high, low, c <= vec3<f32>(0.04045)), 1.0);
}
";

/// Os dois planos de uma live na placa.
struct Planes {
    light: wgpu::Texture,
    color: wgpu::Texture,
    bind: wgpu::BindGroup,
    size: (u32, u32),
}

/// O que o desenho precisa, guardado junto do renderizador do egui.
struct Gpu {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    videos: HashMap<String, Planes>,
}

/// Pedido de desenho de uma live, executado dentro do passe do egui.
struct Draw {
    key: String,
}

impl CallbackTrait for Draw {
    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &CallbackResources,
    ) {
        let Some(gpu) = resources.get::<Gpu>() else {
            return;
        };
        let Some(planes) = gpu.videos.get(&self.key) else {
            return;
        };
        pass.set_pipeline(&gpu.pipeline);
        pass.set_bind_group(0, &planes.bind, &[]);
        pass.draw(0..4, 0..1);
    }
}

/// Quadros das lives prontos para desenhar, por chave ("uid:screen").
pub struct Videos {
    state: Option<RenderState>,
    /// Força a conversão na CPU (para medir os dois caminhos).
    pub cpu: bool,
    textures: HashMap<String, (TextureHandle, u64)>,
    /// Último quadro já enviado à placa, por live.
    serials: HashMap<String, u64>,
    sizes: HashMap<String, (u32, u32)>,
    scratch: Vec<Color32>,
}

impl Videos {
    pub fn new(state: Option<&RenderState>, cpu: bool) -> Self {
        if let Some(state) = state {
            let device = &state.device;
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("nimbo_video"),
                source: wgpu::ShaderSource::Wgsl(SHADER.into()),
            });
            let texture = |binding| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            };
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("nimbo_video"),
                entries: &[
                    texture(0),
                    texture(1),
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("nimbo_video"),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("nimbo_video"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..wgpu::PrimitiveState::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(if state.target_format.is_srgb() {
                        "fs_linear"
                    } else {
                        "fs_gamma"
                    }),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: state.target_format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                multiview_mask: None,
                cache: None,
            });
            let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("nimbo_video"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..wgpu::SamplerDescriptor::default()
            });
            state.renderer.write().callback_resources.insert(Gpu {
                pipeline,
                layout,
                sampler,
                videos: HashMap::new(),
            });
        }
        Self {
            state: state.cloned(),
            cpu,
            textures: HashMap::new(),
            serials: HashMap::new(),
            sizes: HashMap::new(),
            scratch: Vec::new(),
        }
    }

    /// Por onde os quadros estão indo para a tela, para o relatório de medidas.
    pub fn path(&self) -> &'static str {
        if self.state.is_some() && !self.cpu {
            "planos na placa"
        } else {
            "conversão na CPU"
        }
    }

    /// Entrega o quadro mais recente de uma live. Devolve `true` quando ele era novo (foi enviado
    /// à placa agora).
    pub fn upload(&mut self, ctx: &egui::Context, key: &str, frame: &Frame) -> bool {
        if self.serials.get(key) == Some(&frame.serial) {
            return false;
        }
        let (width, height) = (frame.width, frame.height);
        if width == 0
            || height == 0
            || frame.stride < width as usize
            || frame.y().len() < frame.stride * height as usize
        {
            return false;
        }
        match self.state.clone().filter(|_| !self.cpu) {
            Some(state) => self.upload_planes(&state, key, frame),
            None => self.upload_rgba(ctx, key, frame),
        }
        self.serials.insert(key.to_owned(), frame.serial);
        self.sizes.insert(key.to_owned(), (width, height));
        true
    }

    fn upload_planes(&mut self, state: &RenderState, key: &str, frame: &Frame) {
        let (width, height) = (frame.width, frame.height);
        let (half_width, half_height) = (width.div_ceil(2), height.div_ceil(2));
        if frame.uv().len() < frame.stride * half_height as usize {
            return;
        }
        let mut renderer = state.renderer.write();
        let Some(gpu) = renderer.callback_resources.get_mut::<Gpu>() else {
            return;
        };
        if gpu
            .videos
            .get(key)
            .is_none_or(|planes| planes.size != (width, height))
        {
            let plane = |label, format, width, height| {
                state.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                })
            };
            let light = plane(
                "nimbo_video_luz",
                wgpu::TextureFormat::R8Unorm,
                width,
                height,
            );
            let color = plane(
                "nimbo_video_cor",
                wgpu::TextureFormat::Rg8Unorm,
                half_width,
                half_height,
            );
            let view = |texture: &wgpu::Texture| {
                texture.create_view(&wgpu::TextureViewDescriptor::default())
            };
            let bind = state.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("nimbo_video"),
                layout: &gpu.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view(&light)),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view(&color)),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&gpu.sampler),
                    },
                ],
            });
            gpu.videos.insert(
                key.to_owned(),
                Planes {
                    light,
                    color,
                    bind,
                    size: (width, height),
                },
            );
        }
        let Some(planes) = gpu.videos.get(key) else {
            return;
        };
        let write = |texture: &wgpu::Texture, data: &[u8], width, height| {
            state.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(frame.stride as u32),
                    rows_per_image: Some(height),
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
        };
        write(&planes.light, frame.y(), width, height);
        write(&planes.color, frame.uv(), half_width, half_height);
    }

    /// Reserva: converte para RGBA na CPU e entrega ao egui como uma imagem comum.
    fn upload_rgba(&mut self, ctx: &egui::Context, key: &str, frame: &Frame) {
        let (width, height, stride) = (frame.width as usize, frame.height as usize, frame.stride);
        let (light, color) = (frame.y(), frame.uv());
        self.scratch.clear();
        self.scratch.reserve(width * height);
        for row in 0..height {
            let line = &light[row * stride..row * stride + width];
            let chroma_at = (row / 2) * stride;
            let chroma = color.get(chroma_at..chroma_at + stride).unwrap_or(&[]);
            for (x, &y) in line.iter().enumerate() {
                let pair = x & !1;
                let (u, v) = match chroma.get(pair..pair + 2) {
                    Some(uv) => (i32::from(uv[0]) - 128, i32::from(uv[1]) - 128),
                    None => (0, 0),
                };
                // Os mesmos números do shader, em ponto fixo de 16 bits.
                let y = i32::from(y) << 16;
                let channel = |value: i32| ((value + 32768) >> 16).clamp(0, 255) as u8;
                self.scratch.push(Color32::from_rgb(
                    channel(y + 103_206 * v),
                    channel(y - 12_275 * u - 30_677 * v),
                    channel(y + 121_609 * u),
                ));
            }
        }
        let image = ColorImage::new([width, height], std::mem::take(&mut self.scratch));
        match self.textures.get_mut(key) {
            Some((texture, _)) => texture.set(image, TextureOptions::LINEAR),
            None => {
                let texture =
                    ctx.load_texture(format!("live-{key}"), image, TextureOptions::LINEAR);
                self.textures.insert(key.to_owned(), (texture, 0));
            }
        }
    }

    /// Tamanho do último quadro de uma live, se já chegou algum.
    pub fn size(&self, key: &str) -> Option<(u32, u32)> {
        self.sizes.get(key).copied()
    }

    /// Desenha a live no retângulo dado (quem chama já acertou a proporção).
    pub fn paint(&self, ui: &Ui, rect: Rect, key: &str) {
        if !self.sizes.contains_key(key) {
            return;
        }
        if self.state.is_some() && !self.cpu {
            ui.painter().add(egui_wgpu::Callback::new_paint_callback(
                rect,
                Draw {
                    key: key.to_owned(),
                },
            ));
        } else if let Some((texture, _)) = self.textures.get(key) {
            ui.painter().image(
                texture.id(),
                rect,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
    }

    /// A live saiu do palco: solta as texturas dela.
    pub fn forget(&mut self, key: &str) {
        self.textures.remove(key);
        self.serials.remove(key);
        self.sizes.remove(key);
        if let Some(state) = &self.state
            && let Some(gpu) = state.renderer.write().callback_resources.get_mut::<Gpu>()
        {
            gpu.videos.remove(key);
        }
    }

    /// Solta tudo o que não está na lista (lives que saíram do palco).
    pub fn keep_only(&mut self, keys: &[String]) {
        let gone: Vec<String> = self
            .sizes
            .keys()
            .filter(|key| !keys.contains(key))
            .cloned()
            .collect();
        for key in gone {
            self.forget(&key);
        }
    }
}
