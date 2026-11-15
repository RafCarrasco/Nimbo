//! `nimbo-media watch <http://127.0.0.1:porta> [opções]`: assiste, com o player em Rust, à live do
//! servidor de teste (`nimbo-media serve`). Abre uma janela simples, desenha a live e escreve os
//! números a cada 2 s. Se a live cai, tenta de novo sozinho.
//!
//!   --seconds N   termina depois de N segundos (sem isso, fica até fechar a janela)
//!   --gpu         desenha direto da textura do decodificador, sem trazer o quadro para a memória
//!   --software    decodifica por software (a reserva)
//!   --no-audio    não pede o som
//!   --volume V    volume de 0 a 1. O padrão é 0: o teste nunca toca som sem pedirem
//!   --offscreen   abre a janela fora da tela (não aparece para quem usa o computador)
//!   --dump ARQ    grava em ARQ (.bmp) um quadro decodificado, depois de ~3 s de live, e escreve a
//!                 média de Y, U e V em seis faixas verticais (teste de cor)
//!   --no-draw     recebe e decodifica, mas não desenha (mede o player sem a janela)

use std::io::{Read, Write};
use std::mem::ManuallyDrop;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use nimbo_media::viewer::{Frame, Options, State, Stats, Viewer};
use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_WRITE,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_MAP_WRITE,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
    D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_COLOR_SPACE,
    D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
    D3D11_VIDEO_USAGE_OPTIMAL_SPEED, D3D11_VPIV_DIMENSION_TEXTURE2D,
    D3D11_VPOV_DIMENSION_TEXTURE2D, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext,
    ID3D11Multithread, ID3D11Texture2D, ID3D11VideoContext, ID3D11VideoDevice,
    ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorInputView,
    ID3D11VideoProcessorOutputView,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_FORMAT_UNKNOWN,
    DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_PRESENT, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG,
    DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIDevice, IDXGIFactory2,
    IDXGISwapChain1,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRect, CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DispatchMessageW,
    GetClientRect, IDC_ARROW, LoadCursorW, MSG, PM_REMOVE, PeekMessageW, PostQuitMessage,
    RegisterClassW, SW_SHOWNOACTIVATE, ShowWindow, TranslateMessage, WM_DESTROY, WM_QUIT,
    WNDCLASSW, WS_OVERLAPPEDWINDOW,
};
use windows::core::{Interface, w};

use crate::AnyError;

/// De quantos em quantos segundos os números são escritos.
const STATS_EVERY: Duration = Duration::from_secs(2);
/// Depois que a live cai, de quanto em quanto tempo tentar de novo.
const RETRY_EVERY: Duration = Duration::from_secs(2);

struct Args {
    url: String,
    seconds: Option<u64>,
    gpu: bool,
    offscreen: bool,
    /// Só recebe e decodifica: a janela fica vazia.
    no_draw: bool,
    /// Onde gravar um quadro decodificado (conferência da imagem sem passar pelo desenho).
    dump: Option<String>,
    options: Options,
}

fn parse(args: &[String]) -> Result<Args, AnyError> {
    let mut parsed = Args {
        url: String::new(),
        seconds: None,
        gpu: false,
        offscreen: false,
        no_draw: false,
        dump: None,
        // Volume zero: o teste decodifica e entrega o som à placa, mas ninguém ouve.
        options: Options {
            volume: 0.0,
            ..Options::default()
        },
    };
    let mut rest = args.iter().skip(1);
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--gpu" => parsed.gpu = true,
            "--software" => parsed.options.software = true,
            "--no-audio" => parsed.options.audio = false,
            "--offscreen" => parsed.offscreen = true,
            "--no-draw" => parsed.no_draw = true,
            "--seconds" => parsed.seconds = rest.next().and_then(|text| text.parse().ok()),
            "--dump" => parsed.dump = rest.next().cloned(),
            "--volume" => {
                parsed.options.volume = rest
                    .next()
                    .and_then(|text| text.parse().ok())
                    .ok_or("--volume pede um número de 0 a 1")?;
            }
            url if url.starts_with("http://") => parsed.url = url.trim_end_matches('/').into(),
            other => return Err(format!("opção desconhecida: {other}").into()),
        }
    }
    if parsed.url.is_empty() {
        return Err("uso: nimbo-media watch http://127.0.0.1:3070 [--seconds N] [--gpu] [--software] [--no-audio] [--volume V] [--offscreen] [--no-draw] [--dump quadro.bmp]".into());
    }
    // Quem desenha da textura não precisa do quadro na memória.
    parsed.options.memory = !parsed.gpu;
    Ok(parsed)
}

/// `POST /offer` do servidor de teste: manda a oferta, devolve a resposta.
fn post_offer(url: &str, offer: &[u8]) -> Result<Vec<u8>, AnyError> {
    let host = url.trim_start_matches("http://");
    let address = host
        .to_socket_addrs()?
        .next()
        .ok_or("endereço do servidor inválido")?;
    let mut last: AnyError = "sem tentativa".into();
    // Nesta máquina um filtro de rede corta conexões HTTP locais ao acaso: tenta de novo.
    for _ in 0..5 {
        let attempt = || -> Result<Vec<u8>, AnyError> {
            let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(3))?;
            stream.set_read_timeout(Some(Duration::from_secs(8)))?;
            write!(
                stream,
                "POST /offer HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                offer.len()
            )?;
            stream.write_all(offer)?;
            let mut response = Vec::new();
            stream.read_to_end(&mut response)?;
            let split = response
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .ok_or("resposta sem cabeçalho")?;
            if !response.starts_with(b"HTTP/1.1 200") {
                return Err(String::from_utf8_lossy(&response[..split.min(60)])
                    .into_owned()
                    .into());
            }
            Ok(response[split + 4..].to_vec())
        };
        match attempt() {
            Ok(answer) if !answer.is_empty() => return Ok(answer),
            Ok(_) => last = "resposta vazia".into(),
            Err(e) => last = e,
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    Err(last)
}

fn connect(args: &Args) -> Result<Viewer, AnyError> {
    let viewer = Viewer::new(args.options.clone())?;
    let answer = post_offer(&args.url, viewer.offer())?;
    viewer.answer(&answer)?;
    Ok(viewer)
}

extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: repassa os mesmos argumentos que o Windows entregou.
    unsafe {
        if message == WM_DESTROY {
            PostQuitMessage(0);
            return LRESULT(0);
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
}

fn open_window(offscreen: bool) -> Result<HWND, AnyError> {
    // SAFETY: chamadas Win32 com ponteiros para valores locais vivos durante a chamada; o nome da
    // classe e o título são constantes do programa.
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: w!("NimboWatch"),
            ..Default::default()
        };
        RegisterClassW(&class);
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 1280,
            bottom: 720,
        };
        let _ = AdjustWindowRect(&mut rect, WS_OVERLAPPEDWINDOW, false);
        let (x, y) = if offscreen { (-4000, 1200) } else { (80, 80) };
        let hwnd = CreateWindowExW(
            Default::default(),
            w!("NimboWatch"),
            w!("Nimbo watch (player em Rust)"),
            WS_OVERLAPPEDWINDOW,
            x,
            y,
            rect.right - rect.left,
            rect.bottom - rect.top,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        // Sem tirar o foco de quem está usando o computador.
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        Ok(hwnd)
    }
}

/// O processador de vídeo e os tamanhos para os quais ele foi montado.
struct Processor {
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    picture: (u32, u32),
    window: (u32, u32),
}

/// Textura de escrita, textura de entrada do processador e a vista dela, para um tamanho de quadro.
struct Upload {
    staging: ID3D11Texture2D,
    shown: ID3D11Texture2D,
    view: ID3D11VideoProcessorInputView,
    picture: (u32, u32),
}

/// Desenha os quadros na janela com o processador de vídeo do Direct3D (converte NV12 para a cor
/// da tela e redimensiona na placa).
struct Renderer {
    hwnd: HWND,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    swapchain: IDXGISwapChain1,
    /// Processador montado para um tamanho de quadro e um tamanho de janela.
    processor: Option<Processor>,
    target: Option<ID3D11VideoProcessorOutputView>,
    /// Caminho pela memória: por onde o quadro sobe para a placa.
    upload: Option<Upload>,
}

impl Renderer {
    /// `device`: o do decodificador, para desenhar sem cópia; sem ele, um dispositivo próprio.
    fn new(hwnd: HWND, device: Option<ID3D11Device>) -> Result<Self, AnyError> {
        // SAFETY: chamadas COM com ponteiros para variáveis locais vivas durante a chamada.
        unsafe {
            let device = match device {
                Some(device) => device,
                None => {
                    let mut created = None;
                    D3D11CreateDevice(
                        None,
                        D3D_DRIVER_TYPE_HARDWARE,
                        HMODULE::default(),
                        D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                        None,
                        D3D11_SDK_VERSION,
                        Some(&mut created),
                        None,
                        None,
                    )?;
                    created.ok_or("sem dispositivo Direct3D")?
                }
            };
            let context = device.GetImmediateContext()?;
            let _ = context
                .cast::<ID3D11Multithread>()?
                .SetMultithreadProtected(true);
            let factory: IDXGIFactory2 = device.cast::<IDXGIDevice>()?.GetAdapter()?.GetParent()?;
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: 0,
                Height: 0,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                Stereo: false.into(),
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                Flags: 0,
            };
            let swapchain = factory.CreateSwapChainForHwnd(&device, hwnd, &desc, None, None)?;
            Ok(Self {
                hwnd,
                video_device: device.cast()?,
                video_context: context.cast()?,
                device,
                context,
                swapchain,
                processor: None,
                target: None,
                upload: None,
            })
        }
    }

    fn client(&self) -> (u32, u32) {
        let mut rect = RECT::default();
        // SAFETY: `rect` é local e vive durante a chamada.
        let _ = unsafe { GetClientRect(self.hwnd, &mut rect) };
        (
            (rect.right - rect.left).max(1) as u32,
            (rect.bottom - rect.top).max(1) as u32,
        )
    }

    fn draw(&mut self, frame: &Frame) -> Result<(), AnyError> {
        let window = self.client();
        let picture = (frame.width, frame.height);
        // SAFETY: chamadas COM com ponteiros para valores locais vivos durante a chamada; texturas e
        // vistas pertencem ao mesmo dispositivo; o ponteiro do Map vale até o Unmap.
        unsafe {
            if !matches!(&self.processor, Some(p) if p.picture == picture && p.window == window) {
                // Janela ou live mudou de tamanho: refaz a superfície da janela e o processador.
                self.target = None;
                self.processor = None;
                self.swapchain.ResizeBuffers(
                    0,
                    window.0,
                    window.1,
                    DXGI_FORMAT_UNKNOWN,
                    DXGI_SWAP_CHAIN_FLAG(0),
                )?;
                let rate = DXGI_RATIONAL {
                    Numerator: 60,
                    Denominator: 1,
                };
                let enumerator = self.video_device.CreateVideoProcessorEnumerator(
                    &D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                        InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                        InputFrameRate: rate,
                        InputWidth: picture.0,
                        InputHeight: picture.1,
                        OutputFrameRate: rate,
                        OutputWidth: window.0,
                        OutputHeight: window.1,
                        // OPTIMAL_SPEED e não PLAYBACK_NORMAL: neste último o processador da NVIDIA
                        // mistura quadros vizinhos e cada objeto em movimento ganha um fantasma
                        // (visto na foto da janela em 15/11/2026; o quadro decodificado estava limpo).
                        Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
                    },
                )?;
                let processor = self.video_device.CreateVideoProcessor(&enumerator, 0)?;
                self.video_context.VideoProcessorSetStreamFrameFormat(
                    &processor,
                    0,
                    D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                );
                self.video_context
                    .VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
                // A live chega em BT.709 de faixa completa (0 a 255): bit 2 = matriz 709, bits 4 e 5 =
                // faixa (2 = completa). Sem dizer isso, o processador supõe a faixa de TV e escurece.
                self.video_context.VideoProcessorSetStreamColorSpace(
                    &processor,
                    0,
                    &D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
                        _bitfield: (1 << 2) | (2 << 4),
                    },
                );
                // Só a parte visível do quadro (a textura do decodificador tem linhas a mais).
                let source = RECT {
                    left: 0,
                    top: 0,
                    right: picture.0 as i32,
                    bottom: picture.1 as i32,
                };
                self.video_context.VideoProcessorSetStreamSourceRect(
                    &processor,
                    0,
                    true,
                    Some(&source),
                );
                // Cabe inteiro na janela, sem deformar: sobra preta dos lados ou em cima e embaixo.
                let scale = (f64::from(window.0) / f64::from(picture.0))
                    .min(f64::from(window.1) / f64::from(picture.1));
                let (w, h) = (
                    (f64::from(picture.0) * scale) as i32,
                    (f64::from(picture.1) * scale) as i32,
                );
                let (left, top) = ((window.0 as i32 - w) / 2, (window.1 as i32 - h) / 2);
                let dest = RECT {
                    left,
                    top,
                    right: left + w,
                    bottom: top + h,
                };
                self.video_context.VideoProcessorSetStreamDestRect(
                    &processor,
                    0,
                    true,
                    Some(&dest),
                );
                let full = RECT {
                    left: 0,
                    top: 0,
                    right: window.0 as i32,
                    bottom: window.1 as i32,
                };
                self.video_context
                    .VideoProcessorSetOutputTargetRect(&processor, true, Some(&full));

                let back: ID3D11Texture2D = self.swapchain.GetBuffer(0)?;
                let mut view = None;
                self.video_device.CreateVideoProcessorOutputView(
                    &back,
                    &enumerator,
                    &D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                        ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                        Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                            Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                        },
                    },
                    Some(&mut view),
                )?;
                self.target = view;
                self.processor = Some(Processor {
                    enumerator,
                    processor,
                    picture,
                    window,
                });
                // A vista de entrada depende do enumerador: refaz junto.
                self.upload = None;
            }
            let Some(Processor {
                enumerator,
                processor,
                ..
            }) = &self.processor
            else {
                return Err("sem processador de vídeo".into());
            };
            let Some(target) = &self.target else {
                return Err("sem superfície de desenho".into());
            };

            let input = if let Some(gpu) = &frame.gpu
                && frame.data.is_empty()
            {
                // Sem cópia: a própria fatia da textura do decodificador é a entrada.
                self.input_view(enumerator, &gpu.texture, gpu.index)?
            } else {
                if frame.data.is_empty() {
                    return Err("quadro sem imagem na memória nem na placa".into());
                }
                if !matches!(&self.upload, Some(upload) if upload.picture == picture) {
                    let texture =
                        |usage, bind: u32, access: u32| -> Result<ID3D11Texture2D, AnyError> {
                            let desc = D3D11_TEXTURE2D_DESC {
                                Width: picture.0,
                                Height: picture.1,
                                MipLevels: 1,
                                ArraySize: 1,
                                Format: DXGI_FORMAT_NV12,
                                SampleDesc: DXGI_SAMPLE_DESC {
                                    Count: 1,
                                    Quality: 0,
                                },
                                Usage: usage,
                                BindFlags: bind,
                                CPUAccessFlags: access,
                                MiscFlags: 0,
                            };
                            let mut out = None;
                            self.device.CreateTexture2D(&desc, None, Some(&mut out))?;
                            out.ok_or_else(|| "CreateTexture2D não devolveu textura".into())
                        };
                    let staging = texture(D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_WRITE.0 as u32)?;
                    let shown = texture(
                        D3D11_USAGE_DEFAULT,
                        (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                        0,
                    )?;
                    let view = self.input_view(enumerator, &shown, 0)?;
                    self.upload = Some(Upload {
                        staging,
                        shown,
                        view,
                        picture,
                    });
                }
                let Some(Upload {
                    staging,
                    shown,
                    view,
                    ..
                }) = &self.upload
                else {
                    return Err("sem textura de envio".into());
                };
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                self.context
                    .Map(staging, 0, D3D11_MAP_WRITE, 0, Some(&mut mapped))?;
                let (pitch, rows) = (mapped.RowPitch as usize, frame.height as usize);
                let width = frame.width as usize;
                let start = mapped.pData.cast::<u8>();
                for (row, line) in frame.y().chunks_exact(frame.stride).enumerate() {
                    std::ptr::copy_nonoverlapping(line.as_ptr(), start.add(row * pitch), width);
                }
                let chroma = start.add(pitch * rows);
                for (row, line) in frame.uv().chunks_exact(frame.stride).enumerate() {
                    std::ptr::copy_nonoverlapping(line.as_ptr(), chroma.add(row * pitch), width);
                }
                self.context.Unmap(staging, 0);
                self.context.CopyResource(shown, staging);
                view.clone()
            };

            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: ManuallyDrop::new(Some(input)),
                ..Default::default()
            };
            let blt = self.video_context.VideoProcessorBlt(
                processor,
                target,
                0,
                std::slice::from_ref(&stream),
            );
            drop(ManuallyDrop::into_inner(stream.pInputSurface));
            blt?;
            // Sem esperar a tela: o teste mede quantos quadros o player consegue entregar.
            self.swapchain.Present(0, DXGI_PRESENT(0)).ok()?;
        }
        Ok(())
    }

    fn input_view(
        &self,
        enumerator: &ID3D11VideoProcessorEnumerator,
        texture: &ID3D11Texture2D,
        slice: u32,
    ) -> Result<ID3D11VideoProcessorInputView, AnyError> {
        let mut view = None;
        // SAFETY: a textura e o enumerador são do mesmo dispositivo; `view` vive durante a chamada.
        unsafe {
            self.video_device.CreateVideoProcessorInputView(
                texture,
                enumerator,
                &D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                    FourCC: 0,
                    ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                        Texture2D: D3D11_TEX2D_VPIV {
                            MipSlice: 0,
                            ArraySlice: slice,
                        },
                    },
                },
                Some(&mut view),
            )?;
        }
        view.ok_or_else(|| "sem vista de entrada do processador de vídeo".into())
    }
}

/// Média de Y, U e V no miolo de cada uma de seis faixas verticais do quadro. Com uma janela de
/// teste de cores conhecidas, diz com que matriz e faixa de cor o vídeo chega.
fn bands(frame: &Frame) -> String {
    let (width, height) = (frame.width as usize, frame.height as usize);
    let (luma, chroma) = (frame.y(), frame.uv());
    let mut out = String::new();
    for band in 0..6 {
        let (left, right) = ((band * 4 + 1) * width / 24, (band * 4 + 3) * width / 24);
        let (top, bottom) = (height * 2 / 5, height * 3 / 5);
        let (mut y, mut u, mut v, mut pixels, mut pairs) = (0u64, 0u64, 0u64, 0u64, 0u64);
        for row in top..bottom {
            for x in left..right {
                y += u64::from(luma[row * frame.stride + x]);
                pixels += 1;
            }
            for x in (left & !1..right).step_by(2) {
                let pair = (row / 2) * frame.stride + x;
                u += u64::from(chroma[pair]);
                v += u64::from(chroma[pair + 1]);
                pairs += 1;
            }
        }
        out.push_str(&format!(
            " [{:.1} {:.1} {:.1}]",
            y as f64 / pixels.max(1) as f64,
            u as f64 / pairs.max(1) as f64,
            v as f64 / pairs.max(1) as f64
        ));
    }
    out
}

/// Grava um quadro NV12 como imagem .bmp. Cores pela matriz BT.709 de faixa completa, que é como o
/// motor manda (medido com faixas de cor conhecida: cinza 64/128/192 chega com Y 64/128/192).
fn dump(frame: &Frame, path: &str) -> std::io::Result<()> {
    let (width, height) = (frame.width as usize, frame.height as usize);
    let row = (width * 3).div_ceil(4) * 4;
    let mut file = Vec::with_capacity(54 + row * height);
    file.extend_from_slice(b"BM");
    file.extend_from_slice(&((54 + row * height) as u32).to_le_bytes());
    file.extend_from_slice(&[0; 4]);
    file.extend_from_slice(&54u32.to_le_bytes());
    file.extend_from_slice(&40u32.to_le_bytes());
    file.extend_from_slice(&(width as i32).to_le_bytes());
    file.extend_from_slice(&(height as i32).to_le_bytes());
    file.extend_from_slice(&1u16.to_le_bytes());
    file.extend_from_slice(&24u16.to_le_bytes());
    file.extend_from_slice(&[0; 24]);
    let (luma, chroma) = (frame.y(), frame.uv());
    let clamp = |value: f32| value.clamp(0.0, 255.0) as u8;
    // O .bmp guarda as linhas de baixo para cima.
    for y in (0..height).rev() {
        let start = file.len();
        for x in 0..width {
            let light = f32::from(luma[y * frame.stride + x]);
            let pair = (y / 2) * frame.stride + (x & !1);
            let (u, v) = (
                f32::from(chroma[pair]) - 128.0,
                f32::from(chroma[pair + 1]) - 128.0,
            );
            file.push(clamp(light + 1.8556 * u));
            file.push(clamp(light - 0.1873 * u - 0.4681 * v));
            file.push(clamp(light + 1.5748 * v));
        }
        file.resize(start + row, 0);
    }
    std::fs::write(path, file)
}

/// Memória do processo em MB: (em uso agora, reservada só para ele).
fn memory_mb() -> (f64, f64) {
    let mut counters = PROCESS_MEMORY_COUNTERS::default();
    let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    // SAFETY: `counters` é local e tem o tamanho informado.
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size) };
    if ok.is_err() {
        return (0.0, 0.0);
    }
    (
        counters.WorkingSetSize as f64 / 1048576.0,
        counters.PagefileUsage as f64 / 1048576.0,
    )
}

/// O que o teste conta além do que o player informa.
#[derive(Clone, Copy, Default)]
struct Drawn {
    frames: u64,
    /// Quadros decodificados que não chegaram a ser desenhados (o desenho estava ocupado).
    skipped: u64,
    time: Duration,
    /// Conexões abertas desde o começo (a primeira e cada nova tentativa).
    connections: u64,
}

fn print_stats(
    stats: Option<&Stats>,
    before: Option<&Stats>,
    drawn: Drawn,
    drawn_before: Drawn,
    seconds: f64,
    cpu: Duration,
    cpu_draw: Duration,
) {
    let (memory, private) = memory_mb();
    let Some(stats) = stats else {
        println!(
            "{{\"estado\":\"sem live\",\"conexoes\":{},\"cpu_um_nucleo_pct\":{:.1},\"memoria_mb\":{memory:.0}}}",
            drawn.connections,
            cpu.as_secs_f64() / seconds * 100.0,
        );
        return;
    };
    // Conexão nova: os contadores recomeçaram, não há diferença a tirar.
    let before = before.filter(|old| old.frames_received <= stats.frames_received);
    let delta = |now: u64, pick: fn(&Stats) -> u64| now - before.map_or(0, pick).min(now);
    let received = delta(stats.frames_received, |s| s.frames_received);
    let decoded = delta(stats.frames_decoded, |s| s.frames_decoded);
    let decode_time = stats.decode_time
        - before
            .map_or(Duration::ZERO, |s| s.decode_time)
            .min(stats.decode_time);
    let latency = stats.latency
        - before
            .map_or(Duration::ZERO, |s| s.latency)
            .min(stats.latency);
    let frames = drawn.frames - drawn_before.frames;
    // Parte do tempo de CPU de uma thread do player neste intervalo, em % de um núcleo.
    let part = |now: Duration, pick: fn(&Stats) -> Duration| {
        (now - before.map_or(Duration::ZERO, pick).min(now)).as_secs_f64() / seconds * 100.0
    };
    let per_frame =
        |total: Duration, count: u64| total.as_secs_f64() * 1000.0 / count.max(1) as f64;
    println!(
        "{{\"estado\":\"{}\",\"rota\":\"{}\",\"resolucao\":\"{}x{}\",\"decodificador\":\"{}\",\"recebidos_por_s\":{:.1},\"decodificados_por_s\":{:.1},\"desenhados_por_s\":{:.1},\"pulados_no_desenho\":{},\"descartados\":{},\"ms_decodificar\":{:.2},\"ms_do_pacote_ao_quadro\":{:.2},\"ms_desenhar\":{:.2},\"fila\":{},\"mbps\":{:.2},\"perda_pct\":{:.1},\"pacotes_faltando\":{},\"reenvios_pedidos\":{},\"quadros_completos_pedidos\":{},\"rtt_ms\":{},\"audio_pacotes_por_s\":{:.1},\"audio_decodificado_ms_por_s\":{:.0},\"audio_entregue_ms_por_s\":{:.0},\"audio_secas\":{},\"saida_de_som\":{},\"conexoes\":{},\"cpu_um_nucleo_pct\":{:.1},\"cpu_rede_pct\":{:.1},\"cpu_decodificador_pct\":{:.1},\"cpu_som_pct\":{:.1},\"cpu_desenho_pct\":{:.1},\"memoria_mb\":{:.0},\"memoria_privada_mb\":{:.0}}}",
        stats.state.as_str(),
        stats.route,
        stats.width,
        stats.height,
        stats.decoder,
        received as f64 / seconds,
        decoded as f64 / seconds,
        frames as f64 / seconds,
        drawn.skipped - drawn_before.skipped,
        delta(stats.frames_dropped, |s| s.frames_dropped),
        per_frame(decode_time, decoded),
        per_frame(latency, decoded),
        per_frame(drawn.time - drawn_before.time, frames),
        stats.backlog,
        f64::from(stats.bitrate) / 1e6,
        stats.loss * 100.0,
        delta(stats.packets_missing, |s| s.packets_missing),
        delta(stats.nacks, |s| s.nacks),
        delta(stats.keyframes_requested, |s| s.keyframes_requested),
        stats.rtt.map_or("null".into(), |rtt| format!(
            "{:.1}",
            rtt.as_secs_f64() * 1000.0
        )),
        delta(stats.audio_packets, |s| s.audio_packets) as f64 / seconds,
        delta(stats.audio_frames, |s| s.audio_frames) as f64 / 48.0 / seconds,
        delta(stats.audio_played, |s| s.audio_played) as f64 / 48.0 / seconds,
        delta(stats.audio_underruns, |s| s.audio_underruns),
        stats.audio_output,
        drawn.connections,
        cpu.as_secs_f64() / seconds * 100.0,
        part(stats.cpu_network, |s| s.cpu_network),
        part(stats.cpu_decoder, |s| s.cpu_decoder),
        part(stats.cpu_audio, |s| s.cpu_audio),
        cpu_draw.as_secs_f64() / seconds * 100.0,
        memory,
        private,
    );
}

pub fn run(args: &[String]) -> Result<(), AnyError> {
    let args = parse(args)?;
    let hwnd = open_window(args.offscreen)?;
    println!("JANELA {}", hwnd.0 as isize);

    let started = Instant::now();
    let mut viewer: Option<Viewer> = None;
    let mut renderer: Option<Renderer> = None;
    let mut last_try: Option<Instant> = None;
    let mut serial = 0u64;
    let mut drawn = Drawn::default();
    // O quadro que está na janela: guardado só para a textura dele não ser reaproveitada.
    let mut _held: Option<Arc<Frame>> = None;
    let mut dumped = false;
    let (mut tick, mut cpu_before) = (Instant::now(), crate::process_cpu());
    let mut draw_before = nimbo_media::thread_cpu();
    let (mut stats_before, mut drawn_before): (Option<Stats>, Drawn) = (None, drawn);
    loop {
        let mut message = MSG::default();
        // SAFETY: `message` é local e vive durante as chamadas.
        unsafe {
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                if message.message == WM_QUIT {
                    return Ok(());
                }
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        if args
            .seconds
            .is_some_and(|limit| started.elapsed().as_secs() >= limit)
        {
            return Ok(());
        }

        // Sem live (ainda não conectou, ou caiu): tenta de novo de tempos em tempos.
        if viewer.as_ref().is_none_or(|v| v.state() == State::Closed)
            && last_try.is_none_or(|at| at.elapsed() >= RETRY_EVERY)
        {
            last_try = Some(Instant::now());
            // Solta primeiro o que desenha da textura do decodificador antigo.
            _held = None;
            if args.gpu {
                renderer = None;
            }
            viewer = None;
            serial = 0;
            match connect(&args) {
                Ok(connected) => {
                    drawn.connections += 1;
                    viewer = Some(connected);
                }
                Err(e) => eprintln!("sem live: {e}"),
            }
        }

        match viewer
            .as_ref()
            .and_then(|v| v.next_frame(serial, Duration::from_millis(20)))
        {
            Some(frame) => {
                drawn.skipped += frame.serial - serial - 1;
                serial = frame.serial;
                let begun = Instant::now();
                if renderer.is_none() && !args.no_draw {
                    // No caminho sem cópia a janela precisa usar o dispositivo do decodificador.
                    let device = if args.gpu {
                        viewer.as_ref().and_then(Viewer::device)
                    } else {
                        None
                    };
                    renderer = Some(Renderer::new(hwnd, device)?);
                }
                if let Some(renderer) = renderer.as_mut() {
                    match renderer.draw(&frame) {
                        Ok(()) => {
                            drawn.frames += 1;
                            drawn.time += begun.elapsed();
                        }
                        Err(e) => eprintln!("desenho: {e}"),
                    }
                }
                if let Some(path) = &args.dump
                    && !dumped
                    && (frame.serial >= 150 || started.elapsed() >= Duration::from_secs(6))
                    && !frame.data.is_empty()
                {
                    dumped = true;
                    match dump(&frame, path) {
                        Ok(()) => eprintln!(
                            "quadro {} gravado em {path}; faixas YUV:{}",
                            frame.serial,
                            bands(&frame)
                        ),
                        Err(e) => eprintln!("quadro não gravado: {e}"),
                    }
                }
                // Segura o quadro desenhado: no caminho sem cópia, a textura dele está na tela.
                _held = Some(frame);
            }
            None if viewer.is_none() => std::thread::sleep(Duration::from_millis(20)),
            None => {}
        }

        if tick.elapsed() >= STATS_EVERY {
            let seconds = tick.elapsed().as_secs_f64();
            let cpu = crate::process_cpu();
            let draw = nimbo_media::thread_cpu();
            let stats = viewer.as_ref().map(Viewer::stats);
            print_stats(
                stats.as_ref(),
                stats_before.as_ref(),
                drawn,
                drawn_before,
                seconds,
                cpu.saturating_sub(cpu_before),
                draw.saturating_sub(draw_before),
            );
            (tick, cpu_before, stats_before, drawn_before) = (Instant::now(), cpu, stats, drawn);
            draw_before = draw;
        }
    }
}
