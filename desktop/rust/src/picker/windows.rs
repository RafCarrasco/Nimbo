//! Dedicated WebView2 picker surface: verbatim embedded document, native attribution and private IPC.
//! This does not create a top-level window, enumerate sources, or grant WGC permission.
mod dialog;
pub use dialog::{DialogEnd, DialogOptions, NativePickerDialog};
/// Read actual NT build, not manifest-compatibility or browser/package version.
pub fn native_os_version() -> Result<String> {
    use windows::{
        Wdk::System::SystemServices::RtlGetVersion,
        Win32::System::SystemInformation::OSVERSIONINFOW,
    };
    let mut v = OSVERSIONINFOW {
        dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
        ..Default::default()
    };
    if unsafe { RtlGetVersion(&mut v) }.0 < 0 || v.dwMajorVersion == 0 || v.dwBuildNumber == 0 {
        return Err(Error::new(E_FAIL, "os-version-unavailable"));
    }
    Ok(format!(
        "{}.{}.{}",
        v.dwMajorVersion, v.dwMinorVersion, v.dwBuildNumber
    ))
}
use super::{
    PICKER_SCRIPT, PICKER_URL, PickerRequest, PickerSession, WireAction, WireMethod,
    document_resource,
};
use crate::policy::{Config, NativeRequest};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
};
use webview2_com::{
    AddScriptToExecuteOnDocumentCreatedCompletedHandler, ContentLoadingEventHandler,
    Microsoft::Web::WebView2::Win32::*, NavigationStartingEventHandler,
    NewWindowRequestedEventHandler, PermissionRequestedEventHandler,
    ScreenCaptureStartingEventHandler, WebMessageReceivedEventHandler,
    WebResourceRequestedEventHandler, take_pwstr,
};
use windows::{
    Win32::{
        Foundation::{E_ACCESSDENIED, E_FAIL, E_POINTER},
        UI::Shell::SHCreateMemStream,
    },
    core::{Error, HSTRING, Interface, PWSTR, Result, w},
};
/// Host-owned current opener context. Populate from the MAIN native controller/dispatcher only.
pub struct OwnerDocument {
    pub source: String,
    pub frame_id: u32,
    pub generation: u64,
}
impl OwnerDocument {
    pub fn request(&self) -> NativeRequest<'_> {
        NativeRequest {
            window: "main",
            source: &self.source,
            frame_id: Some(self.frame_id),
            main_frame_id: Some(self.frame_id),
            parent_frame_id: None,
            document_generation: self.generation,
        }
    }
}
pub struct NativeCall {
    pub id: u32,
    pub generation: u64,
    pub method: WireMethod,
}
pub type OwnerReader = Rc<dyn Fn() -> Option<OwnerDocument>>;
pub type CallHandler = Rc<dyn Fn(&NativePickerSurface, NativeCall) -> Result<()>>;
struct State {
    environment: ICoreWebView2Environment,
    controller: ICoreWebView2Controller,
    core: ICoreWebView2,
    config: Rc<Config>,
    model: RefCell<PickerSession>,
    owner: OwnerReader,
    on_call: CallHandler,
    on_failed: Rc<dyn Fn()>,
    closed: Cell<bool>,
}
impl State {
    fn close(&self) {
        if self.closed.replace(true) {
            return;
        }
        if let Ok(mut s) = self.model.try_borrow_mut() {
            s.closed = true;
            s.catalog = None;
            s.presented.clear();
            s.pending = None;
            s.frame = None;
        }
        if unsafe { self.controller.Close() }.is_err() {
            eprintln!("picker-controller-close-failed");
        }
    }
    fn fail(&self) {
        self.close();
        (self.on_failed)();
    }
}
impl Drop for State {
    fn drop(&mut self) {
        self.close();
    }
}
pub struct NativePickerSurface(Rc<State>);
fn pointer() -> Error {
    Error::from_hresult(E_POINTER)
}
fn denied() -> Error {
    Error::new(E_ACCESSDENIED, "picker-context-unavailable")
}
fn text(read: impl FnOnce(&mut PWSTR) -> Result<()>) -> Result<String> {
    let mut p = PWSTR::null();
    let result = read(&mut p);
    let value = take_pwstr(p);
    result?;
    Ok(value)
}
fn callback(
    weak: &Weak<State>,
    run: impl FnOnce(&NativePickerSurface) -> Result<()>,
) -> Result<()> {
    if let Some(state) = weak.upgrade()
        && !state.closed.get()
    {
        let surface = NativePickerSurface(state);
        if run(&surface).is_err() {
            surface.0.fail();
        }
    }
    Ok(())
}
impl NativePickerSurface {
    /// # Safety
    /// Caller owns a DEDICATED picker controller and its native parent HWND on this UI apartment.
    /// Keep that HWND alive until close/drop. `owner` MUST read actual main-controller source,
    /// native frame id and dispatcher generation, never web fields. The model's window cookie
    /// must be unique/non-reused. `on_call` may use only attributed actions; revalidate native
    /// source identity separately before WGC. Complete asynchronous replies with their original generation.
    pub unsafe fn attach(
        environment: ICoreWebView2Environment,
        controller: ICoreWebView2Controller,
        config: Rc<Config>,
        owner: OwnerReader,
        model: PickerSession,
        on_call: CallHandler,
        on_failed: Rc<dyn Fn()>,
    ) -> Result<Self> {
        let core = match unsafe { controller.CoreWebView2() } {
            Ok(core) => core,
            Err(error) => {
                let _ = unsafe { controller.Close() };
                return Err(error);
            }
        };
        let this = Self(Rc::new(State {
            environment,
            controller,
            core,
            config,
            model: RefCell::new(model),
            owner,
            on_call,
            on_failed,
            closed: Cell::new(false),
        }));
        if this.install().is_err() {
            this.0.close();
            return Err(Error::new(E_FAIL, "picker-guards-incomplete"));
        }
        Ok(this)
    }
    pub(super) fn resize(&self, bounds: windows::Win32::Foundation::RECT) -> Result<()> {
        unsafe { self.0.controller.SetBounds(bounds) }
    }
    pub fn core(&self) -> &ICoreWebView2 {
        &self.0.core
    }
    pub fn close(&self) {
        self.0.close();
    }
    fn parent(&self) -> Result<OwnerDocument> {
        if self.0.closed.get() {
            return Err(denied());
        }
        let owner = (self.0.owner)().ok_or_else(denied)?;
        let m = self.0.model.try_borrow().map_err(|_| denied())?;
        if !self
            .0
            .config
            .authorize(&owner.request(), m.owner_generation)
            || owner.frame_id != m.owner_frame.unwrap_or(0)
        {
            return Err(denied());
        }
        Ok(owner)
    }
    pub fn with_session<T>(
        &self,
        run: impl FnOnce(
            &mut PickerSession,
            &Config,
            &NativeRequest<'_>,
            &PickerRequest<'_>,
        ) -> std::result::Result<T, &'static str>,
    ) -> Result<T> {
        let owner = self.parent()?;
        let source = text(|p| unsafe { self.0.core.Source(p) })?;
        let mut frame = 0;
        unsafe {
            self.0
                .core
                .cast::<ICoreWebView2_20>()?
                .FrameId(&mut frame)?;
        }
        let mut m = self.0.model.try_borrow_mut().map_err(|_| denied())?;
        let attr = PickerRequest {
            window: m.window,
            source: &source,
            frame_id: frame,
            parent_frame_id: None,
            document_generation: m.generation,
        };
        if !m.authorized(&self.0.config, &owner.request(), &attr) {
            return Err(denied());
        }
        run(&mut m, &self.0.config, &owner.request(), &attr).map_err(|_| denied())
    }
    fn post(&self, generation: u64, value: Value) -> Result<()> {
        self.with_session(|m, _, _, _| {
            if m.generation == generation {
                Ok(())
            } else {
                Err("picker-stale")
            }
        })?;
        unsafe {
            self.0
                .core
                .PostWebMessageAsJson(&HSTRING::from(value.to_string()))
        }
    }
    pub fn reply(
        &self,
        id: u32,
        generation: u64,
        result: std::result::Result<Value, &'static str>,
    ) -> Result<()> {
        let value = match result {
            Ok(value) => {
                json!({"v":1,"kind":"picker-result","id":id,"gen":generation,"ok":true,"value":value})
            }
            Err(error) => {
                json!({"v":1,"kind":"picker-result","id":id,"gen":generation,"ok":false,"error":error})
            }
        };
        self.post(generation, value)
    }
    fn install(&self) -> Result<()> {
        self.parent()?;
        unsafe {
            let settings = self.0.core.Settings()?;
            settings.SetIsScriptEnabled(true)?;
            settings.SetIsWebMessageEnabled(true)?;
            settings.SetAreDevToolsEnabled(false)?;
            settings.SetAreDefaultContextMenusEnabled(false)?;
            settings.SetAreHostObjectsAllowed(false)?;
            let mut token = 0;
            let weak = Rc::downgrade(&self.0);
            self.0.core.add_NavigationStarting(
                &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                    callback(&weak, |_| {
                        let args = args.ok_or_else(pointer)?;
                        let uri = text(|p| args.Uri(p))?;
                        args.SetCancel(uri != PICKER_URL)
                    })
                })),
                &mut token,
            )?;
            let weak = Rc::downgrade(&self.0);
            self.0.core.add_NewWindowRequested(
                &NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
                    callback(&weak, |_| args.ok_or_else(pointer)?.SetHandled(true))
                })),
                &mut token,
            )?;
            let weak = Rc::downgrade(&self.0);
            self.0.core.add_PermissionRequested(
                &PermissionRequestedEventHandler::create(Box::new(move |_, args| {
                    callback(&weak, |_| {
                        args.ok_or_else(pointer)?
                            .SetState(COREWEBVIEW2_PERMISSION_STATE_DENY)
                    })
                })),
                &mut token,
            )?;
            let weak = Rc::downgrade(&self.0);
            self.0
                .core
                .cast::<ICoreWebView2_27>()?
                .add_ScreenCaptureStarting(
                    &ScreenCaptureStartingEventHandler::create(Box::new(move |_, args| {
                        callback(&weak, |_| args.ok_or_else(pointer)?.SetCancel(true))
                    })),
                    &mut token,
                )?;
            self.0.core.AddWebResourceRequestedFilter(
                &HSTRING::from("https://nimbo-picker.invalid/*"),
                COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
            )?;
            let weak = Rc::downgrade(&self.0);
            self.0.core.add_WebResourceRequested(
                &WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
                    callback(&weak, |surface| {
                        let args = args.ok_or_else(pointer)?;
                        let request = args.Request()?;
                        let uri = text(|p| request.Uri(p))?;
                        let method = text(|p| request.Method(p))?;
                        let body = document_resource(&uri, &method);
                        let stream =
                            SHCreateMemStream(Some(body.unwrap_or(b""))).ok_or_else(pointer)?;
                        let response = surface.0.environment.CreateWebResourceResponse(
                            &stream,
                            if body.is_some() { 200 } else { 403 },
                            if body.is_some() {
                                w!("OK")
                            } else {
                                w!("Forbidden")
                            },
                            w!("Content-Type: text/html; charset=utf-8\r\nCache-Control: no-store"),
                        )?;
                        args.SetResponse(&response)
                    })
                })),
                &mut token,
            )?;
            let weak = Rc::downgrade(&self.0);
            self.0.core.add_ContentLoading(
                &ContentLoadingEventHandler::create(Box::new(move |_, _| {
                    callback(&weak, |surface| {
                        surface.parent()?;
                        let mut frame = 0;
                        surface
                            .0
                            .core
                            .cast::<ICoreWebView2_20>()?
                            .FrameId(&mut frame)?;
                        surface
                            .0
                            .model
                            .try_borrow_mut()
                            .map_err(|_| denied())?
                            .bind_document(frame)
                            .map_err(|_| denied())?;
                        Ok(())
                    })
                })),
                &mut token,
            )?;
            let weak = Rc::downgrade(&self.0);
            self.0.core.add_WebMessageReceived(
                &WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
                    callback(&weak, |surface| {
                        let args = args.ok_or_else(pointer)?;
                        let source = text(|p| args.Source(p))?;
                        if source != PICKER_URL
                            || surface.with_session(|_, _, _, _| Ok(())).is_err()
                        {
                            return Ok(());
                        }
                        let raw = text(|p| args.WebMessageAsJson(p))?;
                        let action = surface.with_session(|model, cfg, main, request| {
                            model.receive(cfg, main, request, &raw)
                        });
                        match action {
                            Ok(WireAction::Ready { generation }) => surface.post(
                                generation,
                                json!({"v":1,"kind":"picker-ready","gen":generation}),
                            ),
                            Ok(WireAction::Call { id, method }) => {
                                let generation = surface
                                    .0
                                    .model
                                    .try_borrow()
                                    .map_err(|_| denied())?
                                    .generation;
                                (surface.0.on_call)(
                                    surface,
                                    NativeCall {
                                        id,
                                        generation,
                                        method,
                                    },
                                )
                            }
                            Err(_) => Ok(()),
                        }
                    })
                })),
                &mut token,
            )?;
            // Registration completion, not successful submission, is the navigation barrier.
            let weak = Rc::downgrade(&self.0);
            self.0.core.AddScriptToExecuteOnDocumentCreated(
                &HSTRING::from(PICKER_SCRIPT),
                &AddScriptToExecuteOnDocumentCreatedCompletedHandler::create(Box::new(
                    move |result, _| {
                        callback(&weak, |surface| {
                            result?;
                            surface.parent()?;
                            surface.0.core.Navigate(&HSTRING::from(PICKER_URL))
                        })
                    },
                )),
            )?;
        }
        Ok(())
    }
}
