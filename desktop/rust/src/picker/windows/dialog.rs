//! Owned Win32 dialog + WebView surface + bounded metadata worker. A Choice is NOT a capture grant.
use super::{NativeCall, NativePickerSurface, OwnerReader, native_os_version};
use crate::{
    picker::{Choice, PickerSession, RefreshTicket, WireMethod, initial_bounds},
    policy::Config,
    sources::{windows::Win32Metadata, worker::MetadataWorker},
};
use serde_json::Value;
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::{Rc, Weak},
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use webview2_com::{
    CreateCoreWebView2ControllerCompletedHandler, Microsoft::Web::WebView2::Win32::*,
    NavigationCompletedEventHandler,
};
use windows::{
    Win32::{
        Foundation::{E_FAIL, E_POINTER, GetLastError, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{
            BLACK_BRUSH, GetMonitorInfoW, GetStockObject, HBRUSH, MONITOR_DEFAULTTONEAREST,
            MONITORINFO, MonitorFromWindow,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::GetDpiForWindow,
            Input::KeyboardAndMouse::{EnableWindow, IsWindowEnabled},
            WindowsAndMessaging::*,
        },
    },
    core::{Error, Interface, Result, w},
};
const TIMER: usize = 17;
thread_local! {static WINDOWS:RefCell<HashMap<isize,Weak<State>>>=RefCell::new(HashMap::new());}
static COOKIE: AtomicU64 = AtomicU64::new(1);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogEnd {
    Chosen,
    CancelledFromPicker,
    ClosedNatively,
    Failed,
}
pub type CompletionHandler = Box<dyn FnOnce(Option<Choice>)>;
pub struct DialogOptions {
    pub environment: ICoreWebView2Environment,
    pub config: Rc<Config>,
    pub owner: OwnerReader,
    pub parent: HWND,
    pub audio: bool,
    pub on_done: CompletionHandler,
}
struct Pending {
    ticket: RefreshTicket,
    id: u32,
    generation: u64,
    choice: Option<(String, bool)>,
    started: Instant,
}
struct State {
    hwnd: HWND,
    parent: HWND,
    environment: ICoreWebView2Environment,
    config: Rc<Config>,
    owner: OwnerReader,
    owner_generation: u64,
    owner_frame: u32,
    surface: RefCell<Option<NativePickerSurface>>,
    worker: RefCell<MetadataWorker>,
    pending: RefCell<Option<Pending>>,
    done: RefCell<Option<CompletionHandler>>,
    closed: Cell<bool>,
    ready: Cell<bool>,
    end: Cell<Option<DialogEnd>>,
    clean: Cell<Option<bool>>,
    started: Instant,
}
pub struct NativePickerDialog(Rc<State>);
fn failed() -> Error {
    Error::new(E_FAIL, "native-picker-unavailable")
}
fn pointer() -> Error {
    Error::from_hresult(E_POINTER)
}
impl NativePickerDialog {
    /// # Safety
    /// UI apartment only; parent is the live, owned MAIN window. Keep parent alive until this
    /// dialog closes. `owner` reads native main controller/dispatcher fields, never JS fields.
    /// Caller must close on application shutdown; on_done receives metadata, not WGC permission.
    pub unsafe fn open(o: DialogOptions) -> Result<Self> {
        let owner = (o.owner)().ok_or_else(failed)?;
        let mut pid = 0;
        unsafe {
            GetWindowThreadProcessId(o.parent, Some(&mut pid));
        }
        if pid != std::process::id() || !unsafe { IsWindowEnabled(o.parent) }.as_bool() {
            return Err(failed());
        }
        let cookie = COOKIE
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
            .map_err(|_| failed())?;
        let model = PickerSession::new(&o.config, &owner.request(), cookie, o.audio)
            .map_err(|_| failed())?;
        register()?;
        let bounds = work_bounds(o.parent)?;
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_DLGMODALFRAME,
                w!("NimboRustPrivatePicker"),
                w!("Nimbo · Escolher o que transmitir"),
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_THICKFRAME,
                bounds.left,
                bounds.top,
                bounds.right - bounds.left,
                bounds.bottom - bounds.top,
                Some(o.parent),
                None,
                Some(GetModuleHandleW(None)?.into()),
                None,
            )?
        };
        let worker = match MetadataWorker::spawn(Win32Metadata) {
            Ok(worker) => worker,
            Err(_) => {
                let _ = unsafe { DestroyWindow(hwnd) };
                return Err(failed());
            }
        };
        let state = Rc::new(State {
            hwnd,
            parent: o.parent,
            environment: o.environment,
            config: o.config,
            owner: o.owner,
            owner_generation: owner.generation,
            owner_frame: owner.frame_id,
            surface: RefCell::new(None),
            worker: RefCell::new(worker),
            pending: RefCell::new(None),
            done: RefCell::new(Some(o.on_done)),
            closed: Cell::new(false),
            ready: Cell::new(false),
            end: Cell::new(None),
            clean: Cell::new(None),
            started: Instant::now(),
        });
        WINDOWS.with(|m| {
            m.borrow_mut()
                .insert(hwnd.0 as isize, Rc::downgrade(&state))
        });
        unsafe {
            let _ = EnableWindow(state.parent, false);
        }
        if unsafe { SetTimer(Some(hwnd), TIMER, 16, None) } == 0 {
            state.finish(None, DialogEnd::Failed);
            return Err(failed());
        }
        // Borrow icons from our own parent; do not destroy shared icon handles.
        unsafe {
            for kind in [ICON_BIG, ICON_SMALL] {
                let icon = SendMessageW(
                    state.parent,
                    WM_GETICON,
                    Some(WPARAM(kind as usize)),
                    Some(LPARAM(0)),
                );
                let _ = SendMessageW(
                    hwnd,
                    WM_SETICON,
                    Some(WPARAM(kind as usize)),
                    Some(LPARAM(icon.0)),
                );
            }
        }
        let weak = Rc::downgrade(&state);
        let handler = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(
            move |result, controller| {
                let Some(s) = weak.upgrade() else {
                    if let Some(c) = controller {
                        let _ = unsafe { c.Close() };
                    }
                    return Ok(());
                };
                if s.closed.get() {
                    if let Some(c) = controller {
                        let _ = unsafe { c.Close() };
                    }
                    return Ok(());
                }
                let close_on_error = controller.clone();
                let setup = (|| -> Result<()> {
                    result?;
                    let controller = controller.ok_or_else(pointer)?;
                    let core = unsafe { controller.CoreWebView2()? };
                    let nav = Rc::downgrade(&s);
                    let mut token = 0;
                    let navigation_handler =
                        NavigationCompletedEventHandler::create(Box::new(move |_, args| {
                            if let Some(s) = nav.upgrade()
                                && !s.closed.get()
                            {
                                let mut success = windows::core::BOOL(0);
                                let valid = args
                                    .ok_or_else(pointer)
                                    .and_then(|a| unsafe { a.IsSuccess(&mut success) })
                                    .is_ok()
                                    && success.as_bool();
                                if valid {
                                    s.ready.set(true);
                                    unsafe {
                                        let _ = ShowWindow(s.hwnd, SW_SHOW);
                                    }
                                } else {
                                    s.finish(None, DialogEnd::Failed);
                                }
                            }
                            Ok(())
                        }));
                    unsafe {
                        core.add_NavigationCompleted(&navigation_handler, &mut token)?;
                    }
                    let calls = Rc::downgrade(&s);
                    let errors = Rc::downgrade(&s);
                    let surface = unsafe {
                        NativePickerSurface::attach(
                            s.environment.clone(),
                            controller,
                            s.config.clone(),
                            s.owner.clone(),
                            model,
                            Rc::new(move |surface, call| {
                                if let Some(s) = calls.upgrade() {
                                    s.call(surface, call)
                                } else {
                                    Err(failed())
                                }
                            }),
                            Rc::new(move || {
                                if let Some(s) = errors.upgrade() {
                                    s.finish(None, DialogEnd::Failed);
                                }
                            }),
                        )?
                    };
                    *s.surface.borrow_mut() = Some(surface);
                    s.resize();
                    Ok(())
                })();
                if setup.is_err() {
                    if let Some(controller) = close_on_error {
                        let _ = unsafe { controller.Close() };
                    }
                    s.finish(None, DialogEnd::Failed);
                }
                Ok(())
            },
        ));
        let start = unsafe {
            state
                .environment
                .CreateCoreWebView2Controller(hwnd, &handler)
        };
        if start.is_err() {
            state.finish(None, DialogEnd::Failed);
            return Err(failed());
        }
        Ok(Self(state))
    }
    pub fn hwnd(&self) -> HWND {
        self.0.hwnd
    }
    pub fn is_open(&self) -> bool {
        !self.0.closed.get()
    }
    pub fn is_ready(&self) -> bool {
        self.is_open() && self.0.ready.get()
    }
    pub fn core(&self) -> Option<ICoreWebView2> {
        self.0.surface.borrow().as_ref().map(|s| s.core().clone())
    }
    pub fn close(&self) {
        self.0.finish(None, DialogEnd::ClosedNatively);
    }
    pub fn metadata_shutdown_succeeded(&self) -> Option<bool> {
        self.0.clean.get()
    }
    pub fn completion(&self) -> Option<DialogEnd> {
        self.0.end.get()
    }
    pub fn has_catalog(&self) -> bool {
        self.0.surface.borrow().as_ref().is_some_and(|s| {
            s.with_session(|m, _, _, _| Ok(m.catalog.is_some()))
                .unwrap_or(false)
        })
    }
}
impl Drop for NativePickerDialog {
    fn drop(&mut self) {
        self.close();
    }
}
impl State {
    fn finish(&self, mut choice: Option<Choice>, mut reason: DialogEnd) {
        if self.closed.replace(true) {
            return;
        }
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER);
        }
        if let Ok(surface) = self.surface.try_borrow()
            && let Some(s) = surface.as_ref()
        {
            s.close();
        }
        let clean = self
            .worker
            .try_borrow_mut()
            .is_ok_and(|mut w| w.shutdown(Duration::from_secs(2)).is_ok());
        self.clean.set(Some(clean));
        if !clean {
            eprintln!("picker-metadata-shutdown-failed");
            choice = None;
            reason = DialogEnd::Failed;
        }
        self.end.set(Some(reason));
        self.pending.borrow_mut().take();
        WINDOWS.with(|m| m.borrow_mut().remove(&(self.hwnd.0 as isize)));
        unsafe {
            if IsWindow(Some(self.parent)).as_bool() {
                let _ = EnableWindow(self.parent, true);
            }
            if IsWindow(Some(self.hwnd)).as_bool() {
                let _ = DestroyWindow(self.hwnd);
            }
        }
        // Closing the controller/window can reenter the owner's native event loop.
        // Never deliver a selection to a different main document after modal teardown.
        if choice.as_ref().is_some_and(|c| {
            !(self.owner)()
                .as_ref()
                .is_some_and(|o| c.authorized(&self.config, &o.request()))
        }) {
            choice = None;
            self.end.set(Some(DialogEnd::Failed));
        }
        let done = self.done.borrow_mut().take();
        if let Some(done) = done {
            done(choice);
        }
    }
    fn own_processes(&self) -> Result<Vec<u32>> {
        unsafe {
            let collection = self
                .environment
                .cast::<ICoreWebView2Environment8>()?
                .GetProcessInfos()?;
            let mut count = 0;
            collection.Count(&mut count)?;
            let mut own = vec![std::process::id()];
            for i in 0..count {
                let mut pid = 0;
                collection.GetValueAtIndex(i)?.ProcessId(&mut pid)?;
                own.push(
                    u32::try_from(pid)
                        .ok()
                        .filter(|p| *p != 0)
                        .ok_or_else(failed)?,
                );
            }
            own.sort_unstable();
            own.dedup();
            Ok(own)
        }
    }
    fn call(&self, surface: &NativePickerSurface, call: NativeCall) -> Result<()> {
        match call.method {
            WireMethod::Cancel => {
                surface.with_session(|m, c, p, a| m.cancel(c, p, a))?;
                self.finish(None, DialogEnd::CancelledFromPicker);
                Ok(())
            }
            WireMethod::Diagnostics => {
                let version = native_os_version()?;
                let data = surface.with_session(|m, _, _, _| {
                    Ok(Value::String(
                        m.catalog
                            .as_ref()
                            .ok_or("picker-catalog-unavailable")?
                            .diagnostics(&version),
                    ))
                })?;
                surface.reply(call.id, call.generation, Ok(data))
            }
            WireMethod::List => self.submit(surface, call.id, call.generation, None),
            WireMethod::Choose { id, audio } => {
                self.submit(surface, call.id, call.generation, Some((id, audio)))
            }
        }
    }
    fn submit(
        &self,
        surface: &NativePickerSurface,
        id: u32,
        generation: u64,
        choice: Option<(String, bool)>,
    ) -> Result<()> {
        if self.pending.borrow().is_some() {
            return surface.reply(
                id,
                generation,
                Err("Aguarde a atualização atual e tente novamente."),
            );
        }
        let own = self.own_processes()?;
        let ticket = surface.with_session(|m, c, p, a| m.begin_refresh(c, p, a))?;
        if self.worker.borrow().submit(ticket, own).is_err() {
            surface
                .with_session(|m, _, _, _| {
                    m.finish_refresh(ticket, Err("metadata-worker-busy"))
                        .map(|_| ())
                })
                .ok();
            return surface.reply(id, generation, Err("Não foi possível atualizar as fontes."));
        }
        *self.pending.borrow_mut() = Some(Pending {
            ticket,
            id,
            generation,
            choice,
            started: Instant::now(),
        });
        Ok(())
    }
    fn tick(&self) {
        if self.closed.get() {
            return;
        }
        let owner = (self.owner)();
        if !owner.as_ref().is_some_and(|o| {
            o.frame_id == self.owner_frame
                && self.config.authorize(&o.request(), self.owner_generation)
        }) {
            self.finish(None, DialogEnd::Failed);
            return;
        }
        if (!self.ready.get() && self.started.elapsed() > Duration::from_secs(15))
            || self
                .pending
                .borrow()
                .as_ref()
                .is_some_and(|p| p.started.elapsed() > Duration::from_secs(15))
        {
            self.finish(None, DialogEnd::Failed);
            return;
        }
        let output = self.worker.borrow().take();
        let output = match output {
            Ok(Some(o)) => o,
            Ok(None) => return,
            Err(_) => {
                self.finish(None, DialogEnd::Failed);
                return;
            }
        };
        let pending = {
            let mut p = self.pending.borrow_mut();
            if p.as_ref().is_some_and(|p| p.ticket == output.0) {
                p.take()
            } else {
                None
            }
        };
        let Some(pending) = pending else {
            return;
        };
        let surface = self.surface.borrow();
        let Some(surface) = surface.as_ref() else {
            return;
        };
        let result = match (pending.choice, output.1) {
            (Some((id, audio)), Ok(fresh)) => {
                match surface.with_session(|m, c, p, a| m.choose(c, p, a, &id, audio, &fresh)) {
                    Ok(choice) => {
                        self.finish(Some(choice), DialogEnd::Chosen);
                        return;
                    }
                    Err(_) => surface.reply(
                        pending.id,
                        pending.generation,
                        Err("A fonte mudou ou deixou de estar disponível."),
                    ),
                }
            }
            (None, result) => {
                let value = surface.with_session(|m, c, p, a| {
                    if !m.finish_refresh(output.0, result)? {
                        return Err("picker-stale");
                    }
                    m.list(c, p, a)
                });
                match value {
                    Ok(value) => surface.reply(pending.id, pending.generation, Ok(value)),
                    Err(_) => surface.reply(
                        pending.id,
                        pending.generation,
                        Err("Não foi possível listar as fontes."),
                    ),
                }
            }
            (Some(_), Err(_)) => surface.reply(
                pending.id,
                pending.generation,
                Err("Não foi possível revalidar a fonte."),
            ),
        };
        if result.is_err() {
            self.finish(None, DialogEnd::Failed);
        }
    }
    fn resize(&self) {
        if self.closed.get() {
            return;
        }
        let surface = self.surface.borrow();
        if let Some(s) = surface.as_ref() {
            let mut r = RECT::default();
            if unsafe { GetClientRect(self.hwnd, &mut r) }
                .and_then(|_| s.resize(r))
                .is_err()
            {
                self.finish(None, DialogEnd::Failed);
            }
        }
    }
}
impl Drop for State {
    fn drop(&mut self) {
        self.finish(None, DialogEnd::ClosedNatively);
    }
}
fn register() -> Result<()> {
    static REGISTERED: OnceLock<std::result::Result<(), u32>> = OnceLock::new();
    let value = REGISTERED.get_or_init(|| unsafe {
        let instance = GetModuleHandleW(None).map_err(|e| e.code().0 as u32)?;
        let c = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: instance.into(),
            lpszClassName: w!("NimboRustPrivatePicker"),
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            hCursor: LoadCursorW(None, IDC_ARROW).map_err(|e| e.code().0 as u32)?,
            ..Default::default()
        };
        if RegisterClassW(&c) == 0 {
            Err(GetLastError().0)
        } else {
            Ok(())
        }
    });
    value.as_ref().map(|_| ()).map_err(|_| failed())
}
fn monitor_work(parent: HWND) -> Result<(crate::shell::Rect, u32)> {
    unsafe {
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(
            MonitorFromWindow(parent, MONITOR_DEFAULTTONEAREST),
            &mut info,
        )
        .as_bool()
        {
            return Err(failed());
        }
        let dpi = GetDpiForWindow(parent);
        if dpi == 0 {
            return Err(failed());
        }
        Ok((
            crate::shell::Rect {
                left: info.rcWork.left,
                top: info.rcWork.top,
                right: info.rcWork.right,
                bottom: info.rcWork.bottom,
            },
            dpi,
        ))
    }
}
fn work_bounds(parent: HWND) -> Result<crate::shell::Rect> {
    let (work, dpi) = monitor_work(parent)?;
    initial_bounds(work, dpi).map_err(|_| failed())
}
unsafe extern "system" fn proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let state = WINDOWS.with(|m| m.borrow().get(&(hwnd.0 as isize)).and_then(Weak::upgrade));
    if let Some(s) = state {
        match msg {
            WM_TIMER if w.0 == TIMER => {
                s.tick();
                return LRESULT(0);
            }
            WM_CLOSE => {
                s.finish(None, DialogEnd::ClosedNatively);
                return LRESULT(0);
            }
            WM_DESTROY => {
                s.finish(None, DialogEnd::ClosedNatively);
                return LRESULT(0);
            }
            WM_GETMINMAXINFO if l.0 != 0 => {
                if let Ok((work, dpi)) = monitor_work(hwnd)
                    && let Ok((width, height)) = crate::picker::minimum_size(work, dpi)
                {
                    // Win32 supplies the mutable MINMAXINFO for this system message.
                    let limits = unsafe { &mut *(l.0 as *mut MINMAXINFO) };
                    limits.ptMinTrackSize.x = width;
                    limits.ptMinTrackSize.y = height;
                    return LRESULT(0);
                }
            }
            WM_DPICHANGED if l.0 != 0 => {
                // Win32 supplies the suggested physical rectangle for our owned HWND.
                let r = unsafe { *(l.0 as *const RECT) };
                if let (Some(width), Some(height)) =
                    (r.right.checked_sub(r.left), r.bottom.checked_sub(r.top))
                    && width > 0
                    && height > 0
                    && unsafe {
                        SetWindowPos(
                            hwnd,
                            None,
                            r.left,
                            r.top,
                            width,
                            height,
                            SWP_NOZORDER | SWP_NOACTIVATE,
                        )
                    }
                    .is_err()
                {
                    s.finish(None, DialogEnd::Failed);
                }
                return LRESULT(0);
            }
            WM_SIZE => {
                if w.0 == SIZE_MINIMIZED as usize {
                    unsafe {
                        let _ = ShowWindow(hwnd, SW_RESTORE);
                    }
                } else {
                    s.resize();
                }
                return LRESULT(0);
            }
            _ => {}
        }
    }
    unsafe { DefWindowProcW(hwnd, msg, w, l) }
}
