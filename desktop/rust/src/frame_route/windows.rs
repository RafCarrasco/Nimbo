//! Windows Graphics Capture + D3D11 CPU readback and main-frame shared-buffer sink.
//! Feasibility is deliberately restricted to a native-owned window in THIS process.
use super::{FrameAck, FrameLayout, FrameRoute};
use crate::policy::{Config, NativeRequest};
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat};
use windows::Win32::Foundation::{E_ACCESSDENIED, E_FAIL, E_INVALIDARG, E_POINTER, HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::{Common::DXGI_FORMAT_B8G8R8A8_UNORM, IDXGIDevice};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::UI::WindowsAndMessaging::{GetWindowThreadProcessId, IsWindow};
use windows::core::{Error, Interface, Result};

fn invalid(message: &str) -> Error {
    Error::new(E_INVALIDARG, message)
}
fn own_window(hwnd: HWND) -> bool {
    // SAFETY: query-only APIs accept an invalid HWND. Never trust web-provided HWND/PID.
    unsafe {
        let mut pid = 0;
        IsWindow(Some(hwnd)).as_bool()
            && GetWindowThreadProcessId(hwnd, Some(&mut pid)) != 0
            && pid == std::process::id()
    }
}
struct Frame(Direct3D11CaptureFrame);
impl Drop for Frame {
    fn drop(&mut self) {
        if let Err(e) = self.0.Close() {
            eprintln!("WGC frame close failed: {e}");
        }
    }
}

pub struct WgcCapture {
    hwnd: HWND,
    item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    session: Option<GraphicsCaptureSession>,
    closed_token: i64,
    closed: Arc<AtomicBool>,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    layout: FrameLayout,
    stopped: bool,
    // Immediate D3D context and native window stay on one owner thread; no unsafe Send escape.
    _thread: PhantomData<Rc<()>>,
}
impl WgcCapture {
    /// # Safety
    /// Initialize WinRT on this thread first. Caller must OWN and keep HWND alive until stop/drop,
    /// without destroying/reusing it meanwhile. Never pass HWNDs from script. Only this process's
    /// windows are accepted; this is NOT a general custom-picker implementation.
    pub unsafe fn for_owned_window(hwnd: HWND) -> Result<Self> {
        if !own_window(hwnd) {
            return Err(Error::from_hresult(E_ACCESSDENIED));
        }
        if !GraphicsCaptureSession::IsSupported()? {
            return Err(Error::new(E_FAIL, "WGC unavailable"));
        }
        // SAFETY: checked live owned HWND; all device/interface HRESULTs are propagated.
        unsafe {
            let interop =
                windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
            let item: GraphicsCaptureItem = interop.CreateForWindow(hwnd)?;
            let size = item.Size()?;
            let layout = FrameLayout::new(
                size.Width.try_into().map_err(|_| invalid("width"))?,
                size.Height.try_into().map_err(|_| invalid("height"))?,
            )
            .map_err(invalid)?;
            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
            let device = device.ok_or_else(|| Error::from_hresult(E_POINTER))?;
            let context = context.ok_or_else(|| Error::from_hresult(E_POINTER))?;
            let dxgi: IDXGIDevice = device.cast()?;
            let runtime: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi)?.cast()?;
            let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
                &runtime,
                DirectXPixelFormat::B8G8R8A8UIntNormalized,
                2,
                size,
            )?;
            let closed = Arc::new(AtomicBool::new(false));
            let flag = closed.clone();
            let token = match item.Closed(&TypedEventHandler::new(move |_, _| {
                flag.store(true, Ordering::Release);
                Ok(())
            })) {
                Ok(token) => token,
                Err(e) => {
                    let _ = pool.Close();
                    return Err(e);
                }
            };
            // From here Drop covers partial session creation/StartCapture failure.
            let mut capture = Self {
                hwnd,
                item,
                pool,
                session: None,
                closed_token: token,
                closed,
                device,
                context,
                layout,
                stopped: false,
                _thread: PhantomData,
            };
            capture.session = Some(capture.pool.CreateCaptureSession(&capture.item)?);
            capture
                .session
                .as_ref()
                .ok_or_else(|| Error::from_hresult(E_POINTER))?
                .StartCapture()?;
            Ok(capture)
        }
    }
    pub fn layout(&self) -> FrameLayout {
        self.layout
    }
    pub fn next_rgba(&mut self) -> Result<Option<Vec<u8>>> {
        if self.stopped {
            return Ok(None);
        }
        if self.closed.load(Ordering::Acquire) || !own_window(self.hwnd) {
            self.stop()?;
            return Err(Error::new(
                E_ACCESSDENIED,
                "capture source closed or identity lost",
            ));
        }
        let outcome = self.read_frame();
        if outcome.is_err() {
            self.stop()?;
        }
        outcome
    }
    fn read_frame(&self) -> Result<Option<Vec<u8>>> {
        // SAFETY: same documented method/ABI as the generated projection. Inspect S_OK/null
        // explicitly: the projection maps that to E_POINTER, hiding a genuine failing HRESULT.
        // The non-null success output owns one COM reference, adopted exactly once by from_raw.
        let frame = unsafe {
            let mut raw = std::ptr::null_mut();
            (Interface::vtable(&self.pool).TryGetNextFrame)(
                Interface::as_raw(&self.pool),
                &mut raw,
            )
            .ok()?;
            if raw.is_null() {
                return Ok(None);
            }
            Frame(Direct3D11CaptureFrame::from_raw(raw))
        };
        let size = frame.0.ContentSize()?;
        if size.Width != self.layout.width as i32 || size.Height != self.layout.height as i32 {
            return Err(invalid(
                "source resize closes feasibility capture; no stale/padded pixels",
            ));
        }
        // SAFETY: frame retains surface while copying. Device/context are confined to this thread.
        unsafe {
            let access: IDirect3DDxgiInterfaceAccess = frame.0.Surface()?.cast()?;
            let texture: ID3D11Texture2D = access.GetInterface()?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            texture.GetDesc(&mut desc);
            if desc.Width < self.layout.width
                || desc.Height < self.layout.height
                || desc.Height > 2160
                || desc.Width > 3840
                || desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM
                || desc.SampleDesc.Count != 1
                || desc.MipLevels != 1
                || desc.ArraySize != 1
            {
                return Err(invalid("unexpected WGC texture format/extent"));
            }
            desc.Usage = D3D11_USAGE_STAGING;
            desc.BindFlags = 0;
            desc.MiscFlags = 0;
            desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            let mut staging = None;
            self.device
                .CreateTexture2D(&desc, None, Some(&mut staging))?;
            let staging = staging.ok_or_else(|| Error::from_hresult(E_POINTER))?;
            self.context.CopyResource(&staging, &texture);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            // Unmap unconditionally after all fallible layout checks/conversion; no pointer escapes.
            let pixels = (|| {
                if mapped.pData.is_null() || mapped.RowPitch < self.layout.width * 4 {
                    return Err(invalid("invalid D3D mapping"));
                }
                let length = (mapped.RowPitch as usize)
                    .checked_mul(self.layout.height as usize)
                    .filter(|n| *n <= isize::MAX as usize)
                    .ok_or_else(|| invalid("mapped extent overflow"))?;
                let bytes = std::slice::from_raw_parts(mapped.pData.cast::<u8>(), length);
                self.layout
                    .rgba(bytes, mapped.RowPitch as usize)
                    .map_err(invalid)
            })();
            self.context.Unmap(&staging, 0);
            pixels.map(Some)
        }
    }
    pub fn stop(&mut self) -> Result<()> {
        if self.stopped {
            return Ok(());
        }
        self.stopped = true;
        let session = self.session.take().map(|s| s.Close()).transpose();
        let pool = self.pool.Close();
        let closed = self.item.RemoveClosed(self.closed_token);
        session?;
        pool?;
        closed?;
        Ok(())
    }
}
impl Drop for WgcCapture {
    fn drop(&mut self) {
        if let Err(e) = self.stop() {
            eprintln!("WGC shutdown failed: {e}");
        }
    }
}

use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_SHARED_BUFFER_ACCESS_READ_ONLY, ICoreWebView2_17, ICoreWebView2Environment12,
    ICoreWebView2SharedBuffer,
};

struct Shared(ICoreWebView2SharedBuffer);
impl Drop for Shared {
    fn drop(&mut self) {
        if let Err(e) = unsafe { self.0.Close() } {
            eprintln!("Shared buffer close failed: {e}");
        }
    }
}

/// Actual shared-buffer pixel posting, not JSON bytes. NO IPC handler or remote authorization grant.
/// All methods run on the owning WebView UI thread. Each frame uses FRESH memory: even an early or
/// forged ACK cannot expose later source pixels through a retained old JS ArrayBuffer.
pub struct SharedFrameSink {
    capture: WgcCapture,
    environment: ICoreWebView2Environment12,
    core: ICoreWebView2_17,
    route: FrameRoute,
    buffers: [Option<Shared>; 2],
}
impl SharedFrameSink {
    /// # Safety
    /// Host must obtain interfaces from the SAME controller, populate request from native frame
    /// ownership, and stop/drop BEFORE any navigation/reload/controller close. Unknown identity
    /// must never call this constructor. No page-provided frame/source fields are trusted.
    pub unsafe fn new(
        environment: ICoreWebView2Environment12,
        core: ICoreWebView2_17,
        route: FrameRoute,
        capture: WgcCapture,
    ) -> Self {
        Self {
            capture,
            environment,
            core,
            route,
            buffers: [None, None],
        }
    }
    /// One bounded capture-to-WebView step. Call even under backpressure to enforce ACK timeout.
    /// Returns false when no frame/credit exists, not a claim that a script rendered a frame.
    /// `now` is monotonic elapsed time from the native owner, not a script clock.
    pub fn pump(
        &mut self,
        config: &Config,
        request: &NativeRequest<'_>,
        now: Duration,
    ) -> Result<bool> {
        let outcome = self.pump_inner(config, request, now);
        if outcome.is_err() {
            self.stop()?;
        }
        outcome
    }
    fn pump_inner(
        &mut self,
        config: &Config,
        request: &NativeRequest<'_>,
        now: Duration,
    ) -> Result<bool> {
        if self.capture.layout() != self.route.layout() {
            return Err(invalid("source/route size mismatch"));
        }
        let Some(ticket) = self
            .route
            .reserve_due(config, request, now)
            .map_err(invalid)?
        else {
            return Ok(false);
        };

        let Some(pixels) = self.capture.next_rgba()? else {
            // Unposted reservation is local only; no page ACK is required.
            // Use a separate cancellation path rather than accepting a synthetic page response.
            self.route.cancel_unposted(ticket);
            return Ok(false);
        };
        // SAFETY: constructor establishes owning controller/thread and trusted document. The slot
        // is reserved before allocation, pointers are checked, and bytes never change after Post.
        unsafe {
            let shared = Shared(self.environment.CreateSharedBuffer(pixels.len() as u64)?);
            let mut pointer = std::ptr::null_mut();
            shared.0.Buffer(&mut pointer)?;
            if pointer.is_null() {
                return Err(Error::from_hresult(E_POINTER));
            }
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), pointer, pixels.len());
            let metadata = windows::core::HSTRING::from(self.route.metadata(ticket));
            self.core.PostSharedBufferToScript(
                &shared.0,
                COREWEBVIEW2_SHARED_BUFFER_ACCESS_READ_ONLY,
                &metadata,
            )?;
            self.buffers[ticket.slot()] = Some(shared);
            self.route.mark_published(ticket, now).map_err(invalid)?;
        }
        Ok(true)
    }
    pub fn acknowledge(
        &mut self,
        config: &Config,
        request: &NativeRequest<'_>,
        ack: FrameAck,
        now_ms: u64,
    ) -> Result<bool> {
        if !self.route.acknowledge_at(config, request, ack, now_ms) {
            if self.route.stopped {
                self.stop()?;
            }
            return Ok(false);
        }
        self.buffers[ack.slot] = None;
        Ok(true)
    }
    pub fn stop(&mut self) -> Result<()> {
        self.route.stop();
        self.buffers = [None, None];
        self.capture.stop()
    }
}
impl Drop for SharedFrameSink {
    fn drop(&mut self) {
        if let Err(e) = self.stop() {
            eprintln!("Capture route shutdown failed: {e}");
        }
    }
}
