//! Decodificação do vídeo de quem assiste: H.264 (Annex B) → NV12.
//!
//! Caminho normal: decodificador H.264 do Windows (Media Foundation) ligado a um dispositivo
//! Direct3D 11, que decodifica na placa de vídeo (DXVA). O quadro sai numa textura NV12 da placa;
//! para quem desenha a partir da memória, ele é copiado para uma textura de leitura e de lá para um
//! buffer NV12 compacto. Sem placa ou sem Media Foundation, a reserva é o OpenH264, por software.

use std::mem::ManuallyDrop;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use openh264::formats::YUVSource;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_PREVENT_INTERNAL_THREADING_OPTIMIZATIONS,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_MAP_FLAG_DO_NOT_WAIT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::DXGI_ERROR_WAS_STILL_DRAWING;
use windows::Win32::Media::MediaFoundation::{
    CLSID_MSH264DecoderMFT, CODECAPI_AVLowLatencyMode, ICodecAPI, IMF2DBuffer, IMFDXGIBuffer,
    IMFDXGIDeviceManager, IMFMediaBuffer, IMFSample, IMFTransform, MF_E_NOTACCEPTING,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_LOW_LATENCY, MF_MT_FRAME_SIZE,
    MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_SUBTYPE,
    MF_SA_D3D11_AWARE, MF_VERSION, MFCreateDXGIDeviceManager, MFCreateMediaType,
    MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video, MFStartup,
    MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
    MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES,
    MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFVideoFormat_H264, MFVideoFormat_NV12,
    MFVideoInterlace_MixedInterlaceOrProgressive,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::System::Variant::{VARIANT, VT_BOOL};
use windows::core::Interface;

use crate::AnyError;

/// Quantos buffers de quadro ficam guardados para reaproveitar (cada um tem ~3 MB em 1080p).
const SPARE: usize = 4;
/// Intervalo entre duas perguntas "a cópia do quadro para a memória já acabou?" (leva ~1 ms).
const READ_POLL: Duration = Duration::from_micros(300);
/// Prazo das perguntas; depois disso a leitura espera o driver.
const READ_WAIT: Duration = Duration::from_millis(30);

/// Buffers de quadro já usados, para o próximo quadro não pedir memória nova ao sistema.
pub(crate) type Pool = Arc<Mutex<Vec<Vec<u8>>>>;

/// O quadro decodificado ainda na placa de vídeo (caminho sem cópia).
///
/// A textura é do decodificador: é uma fatia (`index`) de um conjunto que ele reaproveita. Enquanto
/// este valor existir, a fatia fica reservada; por isso guarde só o quadro mais recente.
pub struct GpuFrame {
    pub texture: ID3D11Texture2D,
    /// Qual fatia do conjunto de texturas é este quadro.
    pub index: u32,
    /// Segura a amostra do decodificador: é ela que mantém a fatia reservada.
    _sample: IMFSample,
}

// SAFETY: a textura vem de um dispositivo Direct3D com proteção multithread ligada, e a amostra do
// Media Foundation é de uso livre entre threads (o próprio decodificador a entrega assim).
unsafe impl Send for GpuFrame {}
// SAFETY: idem; o valor só expõe leitura.
unsafe impl Sync for GpuFrame {}

/// Um quadro que saiu do decodificador.
pub(crate) struct Picture {
    pub width: u32,
    pub height: u32,
    /// NV12 compacto: `width * height` bytes de Y e depois `width * height / 2` de UV intercalado.
    /// Vazio quando a cópia para a memória está desligada.
    pub data: Vec<u8>,
    pub gpu: Option<GpuFrame>,
}

pub(crate) enum Decoder {
    Windows(Box<MediaFoundation>),
    Software(Box<openh264::decoder::Decoder>),
}

impl Decoder {
    /// `software` força a reserva (testes). `memory` diz se cada quadro é copiado para a memória.
    /// Devolve também o nome do que ficou em uso, para as estatísticas.
    pub fn new(software: bool, memory: bool, pool: &Pool) -> Result<(Self, String), AnyError> {
        if !software {
            match MediaFoundation::new(memory, pool.clone()) {
                Ok(decoder) => {
                    let name = if decoder.device.is_some() {
                        "placa de vídeo (Media Foundation + Direct3D 11)"
                    } else {
                        "software (Media Foundation)"
                    };
                    return Ok((Self::Windows(Box::new(decoder)), name.into()));
                }
                Err(e) => eprintln!("decodificador do Windows indisponível: {e}"),
            }
        }
        let decoder = openh264::decoder::Decoder::new()?;
        Ok((
            Self::Software(Box::new(decoder)),
            "software (OpenH264)".into(),
        ))
    }

    /// Dispositivo Direct3D em que os quadros são decodificados, para quem desenha sem cópia.
    pub fn device(&self) -> Option<ID3D11Device> {
        match self {
            Self::Windows(decoder) => decoder.device.clone(),
            Self::Software(_) => None,
        }
    }

    /// Entrega um quadro H.264 inteiro (Annex B) e recolhe em `out` o que o decodificador liberou.
    pub fn decode(
        &mut self,
        data: &[u8],
        pool: &Pool,
        out: &mut Vec<Picture>,
    ) -> Result<(), AnyError> {
        match self {
            Self::Windows(decoder) => decoder.decode(data, out),
            Self::Software(decoder) => {
                if let Some(yuv) = decoder.decode(data)? {
                    out.push(from_i420(&yuv, pool));
                }
                Ok(())
            }
        }
    }
}

/// Um buffer do tamanho pedido, de preferência um já usado.
fn take(pool: &Pool, len: usize) -> Vec<u8> {
    let mut buffer = pool.lock().unwrap().pop().unwrap_or_default();
    buffer.clear();
    buffer.resize(len, 0);
    buffer
}

/// Devolve o buffer de um quadro que ninguém mais usa.
pub(crate) fn give_back(pool: &Pool, buffer: Vec<u8>) {
    let mut spare = pool.lock().unwrap();
    if spare.len() < SPARE && buffer.capacity() > 0 {
        spare.push(buffer);
    }
}

/// I420 (três planos) do OpenH264 → NV12 compacto.
fn from_i420(yuv: &impl YUVSource, pool: &Pool) -> Picture {
    let (width, height) = yuv.dimensions();
    let (width, height) = (width & !1, height & !1);
    let (y_stride, u_stride, v_stride) = yuv.strides();
    let (y, u, v) = (yuv.y(), yuv.u(), yuv.v());
    let mut data = take(pool, width * height * 3 / 2);
    for row in 0..height {
        data[row * width..(row + 1) * width]
            .copy_from_slice(&y[row * y_stride..row * y_stride + width]);
    }
    let chroma = &mut data[width * height..];
    for row in 0..height / 2 {
        let (u_row, v_row) = (&u[row * u_stride..], &v[row * v_stride..]);
        let out = &mut chroma[row * width..(row + 1) * width];
        for (x, pair) in out.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            *pair = [u_row[x], v_row[x]];
        }
    }
    Picture {
        width: width as u32,
        height: height as u32,
        data,
        gpu: None,
    }
}

/// Copia um NV12 com linhas de `pitch` bytes e `coded` linhas de Y para um NV12 compacto.
///
/// # Safety
/// `source` precisa apontar para pelo menos `pitch * coded * 3 / 2` bytes legíveis, com
/// `pitch >= width` e `coded >= height`.
unsafe fn copy_nv12(
    source: *const u8,
    pitch: usize,
    coded: usize,
    width: usize,
    height: usize,
    data: &mut [u8],
) {
    // SAFETY: garantido por quem chama; cada linha lida fica dentro do plano de origem.
    unsafe {
        for row in 0..height {
            let line = std::slice::from_raw_parts(source.add(row * pitch), width);
            data[row * width..(row + 1) * width].copy_from_slice(line);
        }
        let chroma = source.add(pitch * coded);
        let out = &mut data[width * height..];
        for row in 0..height / 2 {
            let line = std::slice::from_raw_parts(chroma.add(row * pitch), width);
            out[row * width..(row + 1) * width].copy_from_slice(line);
        }
    }
}

pub(crate) struct MediaFoundation {
    transform: IMFTransform,
    /// Dispositivo da decodificação pela placa; `None` = o decodificador do Windows em software.
    pub device: Option<ID3D11Device>,
    context: Option<ID3D11DeviceContext>,
    /// Textura de leitura, do tamanho da textura do decodificador, para trazer o quadro à memória.
    staging: Option<(ID3D11Texture2D, u32, u32)>,
    /// Tamanho da imagem visível (a textura costuma ser maior: 1080 linhas viram 1088).
    width: u32,
    height: u32,
    /// Tamanho codificado: onde começa o plano de cor num buffer de memória.
    coded_height: u32,
    memory: bool,
    pool: Pool,
    /// Tempo da próxima amostra, em unidades de 100 ns (só precisa crescer).
    time: i64,
    _manager: Option<IMFDXGIDeviceManager>,
}

// SAFETY: os objetos COM daqui são usados por uma thread só de cada vez (a do decodificador), e o
// dispositivo Direct3D tem a proteção multithread ligada.
unsafe impl Send for MediaFoundation {}

fn variant_true() -> VARIANT {
    let mut v = VARIANT::default();
    // SAFETY: VARIANT recém-zerada; escrevemos o tipo e o campo que corresponde a ele.
    unsafe {
        let inner = &mut *v.Anonymous.Anonymous;
        inner.vt = VT_BOOL;
        inner.Anonymous.boolVal = windows::Win32::Foundation::VARIANT_BOOL(-1);
    }
    v
}

impl MediaFoundation {
    fn new(memory: bool, pool: Pool) -> Result<Self, AnyError> {
        // SAFETY: chamadas COM/Media Foundation com ponteiros para variáveis locais vivas durante a
        // chamada; cada interface retornada é contada por referência pelo próprio tipo.
        unsafe {
            // Já iniciado nesta thread (ou em outro modo) não é erro para o que fazemos aqui.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, 0)?;

            let transform: IMFTransform =
                CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER)?;
            let attributes = transform.GetAttributes()?;
            // Sem isto o decodificador segura vários quadros antes de soltar o primeiro (ele espera
            // quadros fora de ordem, que uma live não tem).
            let _ = attributes.SetUINT32(&CODECAPI_AVLowLatencyMode, 1);
            let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);
            if let Ok(codec) = transform.cast::<ICodecAPI>() {
                let _ = codec.SetValue(&CODECAPI_AVLowLatencyMode, &variant_true());
            }

            // Decodificação pela placa: opcional. Falhou em qualquer passo, segue em software.
            let mut device = None;
            let mut context = None;
            let mut manager = None;
            if attributes.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) != 0
                && let Ok((d3d, ctx, mgr)) = Self::graphics(&transform)
            {
                (device, context, manager) = (Some(d3d), Some(ctx), Some(mgr));
            }

            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            input.SetUINT32(
                &MF_MT_INTERLACE_MODE,
                MFVideoInterlace_MixedInterlaceOrProgressive.0 as u32,
            )?;
            transform.SetInputType(0, &input, 0)?;

            let mut decoder = Self {
                transform,
                device,
                context,
                staging: None,
                width: 0,
                height: 0,
                coded_height: 0,
                memory,
                pool,
                time: 0,
                _manager: manager,
            };
            decoder.choose_output()?;
            decoder
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            decoder
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(decoder)
        }
    }

    /// Cria o dispositivo Direct3D e o entrega ao decodificador.
    ///
    /// # Safety
    /// `transform` precisa ser um decodificador que aceita Direct3D 11 (`MF_SA_D3D11_AWARE`).
    unsafe fn graphics(
        transform: &IMFTransform,
    ) -> Result<(ID3D11Device, ID3D11DeviceContext, IMFDXGIDeviceManager), AnyError> {
        // SAFETY: ponteiros para variáveis locais vivas durante cada chamada.
        unsafe {
            let (mut device, mut context) = (None, None);
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                // Sem as threads internas do driver: medido com dois players lado a lado por 60 s
                // (15/11/2026, NVIDIA), 7,8% de um núcleo contra 9,5% com elas.
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT
                    | D3D11_CREATE_DEVICE_BGRA_SUPPORT
                    | D3D11_CREATE_DEVICE_PREVENT_INTERNAL_THREADING_OPTIMIZATIONS,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
            let device = device.ok_or("sem dispositivo Direct3D")?;
            let context = context.ok_or("sem contexto Direct3D")?;
            // O decodificador usa o dispositivo em threads dele: o Direct3D precisa se proteger.
            let _ = context
                .cast::<ID3D11Multithread>()?
                .SetMultithreadProtected(true);
            let mut token = 0u32;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager = manager.ok_or("sem gerenciador de dispositivo DXGI")?;
            manager.ResetDevice(&device, token)?;
            transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)?;
            Ok((device, context, manager))
        }
    }

    /// Escolhe NV12 entre as saídas que o decodificador oferece e lê o tamanho da imagem.
    fn choose_output(&mut self) -> Result<(), AnyError> {
        // SAFETY: chamadas COM simples; o buffer da área visível é local e tem o tamanho informado.
        unsafe {
            let mut index = 0;
            let chosen = loop {
                let Ok(kind) = self.transform.GetOutputAvailableType(0, index) else {
                    return Err("o decodificador não oferece saída em NV12".into());
                };
                if kind.GetGUID(&MF_MT_SUBTYPE)? == MFVideoFormat_NV12 {
                    break kind;
                }
                index += 1;
            };
            self.transform.SetOutputType(0, &chosen, 0)?;
            let size = chosen.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0);
            let (coded_width, coded_height) = ((size >> 32) as u32, size as u32);
            (self.width, self.height, self.coded_height) =
                (coded_width, coded_height, coded_height);
            // MFVideoArea: deslocamento X (4 bytes), deslocamento Y (4 bytes), largura e altura (i32).
            let mut area = [0u8; 16];
            if chosen
                .GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, &mut area, None)
                .is_ok()
            {
                let visible_width = u32::from_le_bytes([area[8], area[9], area[10], area[11]]);
                let visible_height = u32::from_le_bytes([area[12], area[13], area[14], area[15]]);
                if (1..=coded_width).contains(&visible_width)
                    && (1..=coded_height).contains(&visible_height)
                {
                    (self.width, self.height) = (visible_width, visible_height);
                }
            }
            (self.width, self.height) = (self.width & !1, self.height & !1);
        }
        Ok(())
    }

    fn decode(&mut self, data: &[u8], out: &mut Vec<Picture>) -> Result<(), AnyError> {
        // SAFETY: Lock devolve um ponteiro válido pelo tamanho pedido até o Unlock; copiamos antes dele.
        let sample = unsafe {
            let buffer = MFCreateMemoryBuffer(data.len() as u32)?;
            let mut ptr = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(data.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(self.time)?;
            sample
        };
        self.time += 166_667;
        // SAFETY: chamada COM simples.
        match unsafe { self.transform.ProcessInput(0, &sample, 0) } {
            Ok(()) => {}
            // Cheio: recolhe o que já está pronto e entrega de novo.
            Err(e) if e.code() == MF_E_NOTACCEPTING => {
                self.drain(out)?;
                // SAFETY: idem.
                unsafe { self.transform.ProcessInput(0, &sample, 0)? };
            }
            Err(e) => return Err(e.into()),
        }
        self.drain(out)
    }

    fn drain(&mut self, out: &mut Vec<Picture>) -> Result<(), AnyError> {
        // Teto de voltas: uma mudança de formato que nunca assenta não pode prender a thread.
        for _ in 0..32 {
            // SAFETY: chamada COM simples.
            let info = unsafe { self.transform.GetOutputStreamInfo(0)? };
            let provides = info.dwFlags
                & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0)
                    as u32
                != 0;
            let mut buffers = [MFT_OUTPUT_DATA_BUFFER::default()];
            if !provides {
                // Em software é quem chama que fornece a amostra de saída.
                // SAFETY: chamadas COM simples.
                unsafe {
                    let sample = MFCreateSample()?;
                    sample.AddBuffer(&MFCreateMemoryBuffer(info.cbSize.max(1))?)?;
                    buffers[0].pSample = ManuallyDrop::new(Some(sample));
                }
            }
            let mut status = 0u32;
            // SAFETY: `buffers` e `status` vivem durante a chamada.
            let result = unsafe { self.transform.ProcessOutput(0, &mut buffers, &mut status) };
            // SAFETY: os campos são ManuallyDrop escritos acima ou pela chamada; tomamos posse uma vez.
            let sample = unsafe { ManuallyDrop::take(&mut buffers[0].pSample) };
            unsafe { ManuallyDrop::drop(&mut buffers[0].pEvents) };
            match result {
                Ok(()) => {
                    if let Some(sample) = sample {
                        out.push(self.picture(sample)?);
                    }
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                // Primeiro quadro, ou a live mudou de resolução: o decodificador redefine a saída.
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => self.choose_output()?,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    fn picture(&mut self, sample: IMFSample) -> Result<Picture, AnyError> {
        let (width, height) = (self.width as usize, self.height as usize);
        if width == 0 || height == 0 {
            return Err("o decodificador entregou um quadro sem dizer o tamanho".into());
        }
        // SAFETY: chamada COM simples.
        let buffer: IMFMediaBuffer = unsafe { sample.GetBufferByIndex(0)? };
        if let Ok(dxgi) = buffer.cast::<IMFDXGIBuffer>() {
            // SAFETY: GetResource devolve uma referência nova à textura; `from_raw` assume a posse dela.
            let (texture, index) = unsafe {
                let mut raw = std::ptr::null_mut();
                dxgi.GetResource(&ID3D11Texture2D::IID, &mut raw)?;
                (ID3D11Texture2D::from_raw(raw), dxgi.GetSubresourceIndex()?)
            };
            let data = if self.memory {
                self.read_back(&texture, index)?
            } else {
                Vec::new()
            };
            return Ok(Picture {
                width: self.width,
                height: self.height,
                data,
                gpu: Some(GpuFrame {
                    texture,
                    index,
                    _sample: sample,
                }),
            });
        }
        // Quadro em memória (decodificador do Windows em software).
        let mut data = take(&self.pool, width * height * 3 / 2);
        let coded = self.coded_height.max(self.height) as usize;
        // SAFETY: os ponteiros valem entre Lock e Unlock; `copy_nv12` lê só dentro do quadro (o buffer
        // tem `coded` linhas de Y e metade disso de cor, com `pitch` bytes cada).
        unsafe {
            if let Ok(plane) = buffer.cast::<IMF2DBuffer>() {
                let mut start = std::ptr::null_mut();
                let mut pitch = 0i32;
                plane.Lock2D(&mut start, &mut pitch)?;
                if pitch >= width as i32 {
                    copy_nv12(start, pitch as usize, coded, width, height, &mut data);
                }
                plane.Unlock2D()?;
            } else {
                let mut start = std::ptr::null_mut();
                let mut len = 0u32;
                buffer.Lock(&mut start, None, Some(&mut len))?;
                let pitch = (len as usize * 2 / 3).checked_div(coded).unwrap_or(0);
                if pitch >= width {
                    copy_nv12(start, pitch, coded, width, height, &mut data);
                }
                buffer.Unlock()?;
            }
        }
        Ok(Picture {
            width: self.width,
            height: self.height,
            data,
            gpu: None,
        })
    }

    /// Traz um quadro da placa para a memória: copia a fatia para a textura de leitura e lê de lá.
    fn read_back(&mut self, texture: &ID3D11Texture2D, index: u32) -> Result<Vec<u8>, AnyError> {
        let (Some(device), Some(context)) = (&self.device, &self.context) else {
            return Err("quadro na placa sem dispositivo Direct3D".into());
        };
        let (width, height) = (self.width as usize, self.height as usize);
        // SAFETY: as duas texturas são do mesmo dispositivo e formato; o ponteiro do Map vale até o
        // Unmap e cobre `RowPitch` bytes por linha, com as linhas de cor depois das `coded` de Y.
        unsafe {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            texture.GetDesc(&mut desc);
            if (desc.Width as usize) < width || (desc.Height as usize) < height {
                return Err("a textura do decodificador é menor que a imagem".into());
            }
            if !matches!(&self.staging, Some((_, w, h)) if (*w, *h) == (desc.Width, desc.Height)) {
                let staging = D3D11_TEXTURE2D_DESC {
                    Width: desc.Width,
                    Height: desc.Height,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_NV12,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Usage: D3D11_USAGE_STAGING,
                    BindFlags: 0,
                    CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                    MiscFlags: 0,
                };
                let mut created = None;
                device.CreateTexture2D(&staging, None, Some(&mut created))?;
                let created = created.ok_or("CreateTexture2D não devolveu a textura de leitura")?;
                self.staging = Some((created, desc.Width, desc.Height));
            }
            let Some((staging, _, coded)) = &self.staging else {
                return Err("sem textura de leitura".into());
            };
            context.CopySubresourceRegion(staging, 0, 0, 0, 0, texture, index, None);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            // Pergunta se a cópia acabou e dorme entre as perguntas, em vez de pedir a leitura e deixar
            // o driver esperar do jeito dele (ele espera girando). Medido com dois players lado a lado
            // por 90 s: 4,4% de um núcleo assim contra 8,4% esperando no driver, ao custo de 0,3 ms a
            // mais por quadro.
            context.Flush();
            let deadline = Instant::now() + READ_WAIT;
            loop {
                match context.Map(
                    staging,
                    0,
                    D3D11_MAP_READ,
                    D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32,
                    Some(&mut mapped),
                ) {
                    Ok(()) => break,
                    Err(e)
                        if e.code() == DXGI_ERROR_WAS_STILL_DRAWING
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(READ_POLL);
                    }
                    // Passou do prazo, ou o driver não aceita a pergunta: espera do jeito antigo.
                    Err(_) => {
                        context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
                        break;
                    }
                }
            }
            let mut data = take(&self.pool, width * height * 3 / 2);
            if mapped.RowPitch as usize >= width {
                copy_nv12(
                    mapped.pData.cast::<u8>(),
                    mapped.RowPitch as usize,
                    *coded as usize,
                    width,
                    height,
                    &mut data,
                );
            }
            context.Unmap(staging, 0);
            Ok(data)
        }
    }
}

impl Drop for MediaFoundation {
    fn drop(&mut self) {
        use windows::Win32::Media::MediaFoundation::IMFShutdown;
        // SAFETY: chamada COM simples num objeto ainda vivo. Mesmo motivo do codificador (hw.rs):
        // sem o Shutdown, soltar um transformador do Media Foundation às vezes bloqueia.
        unsafe {
            if let Ok(shutdown) = self.transform.cast::<IMFShutdown>() {
                let _ = shutdown.Shutdown();
            }
        }
    }
}

/// O quadro traz uma imagem completa (IDR)? Percorre as unidades do Annex B.
pub(crate) fn has_idr(data: &[u8]) -> bool {
    units(data).any(|unit| unit.first().is_some_and(|byte| byte & 0x1f == 5))
}

/// As unidades (NAL) de um quadro em Annex B, sem o código de início.
pub(crate) fn units(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let ends: Vec<usize> = starts
        .iter()
        .skip(1)
        .map(|next| {
            // O código de início seguinte pode ter 3 ou 4 bytes (um zero a mais antes).
            let end = next - 3;
            if end > 0 && data[end - 1] == 0 {
                end - 1
            } else {
                end
            }
        })
        .chain(std::iter::once(data.len()))
        .collect();
    starts
        .into_iter()
        .zip(ends)
        .filter(|(start, end)| start < end)
        .map(move |(start, end)| &data[start..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_units_and_the_complete_picture() {
        // SPS (7), PPS (8) e IDR (5) com códigos de início de 4 e de 3 bytes.
        let key = [
            0, 0, 0, 1, 0x67, 1, 2, 0, 0, 0, 1, 0x68, 3, 0, 0, 1, 0x65, 9, 9,
        ];
        let found: Vec<&[u8]> = units(&key).collect();
        assert_eq!(
            found,
            vec![&[0x67, 1, 2][..], &[0x68, 3][..], &[0x65, 9, 9][..]]
        );
        assert!(has_idr(&key));
        // Quadro que depende do anterior (tipo 1).
        assert!(!has_idr(&[0, 0, 0, 1, 0x41, 7, 7]));
        assert!(!has_idr(&[]));
        assert!(!has_idr(&[0, 0, 1]));
    }

    #[test]
    fn copies_only_the_visible_part_of_a_padded_picture() {
        // 4x2 visível dentro de um quadro codificado 4x4 com linhas de 6 bytes.
        let (pitch, coded, width, height) = (6usize, 4usize, 4usize, 2usize);
        let mut source = vec![0u8; pitch * coded * 3 / 2];
        for (i, byte) in source.iter_mut().enumerate() {
            *byte = i as u8;
        }
        let mut data = vec![0u8; width * height * 3 / 2];
        // SAFETY: `source` tem `pitch * coded * 3 / 2` bytes.
        unsafe { copy_nv12(source.as_ptr(), pitch, coded, width, height, &mut data) };
        assert_eq!(&data[..4], &[0, 1, 2, 3]);
        assert_eq!(&data[4..8], &[6, 7, 8, 9]);
        // A cor começa depois das 4 linhas codificadas de Y.
        assert_eq!(&data[8..12], &[24, 25, 26, 27]);
    }
}
