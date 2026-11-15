//! Caminhos de rede das lives que passam pelo navegador embutido (assistir, e a webcam).
//!
//! Por padrão o WebView2 se comporta como o Edge: o WebRTC só oferece a rota padrão e troca o
//! endereço local por um nome `.local`, que só funciona dentro da mesma casa. O app antigo
//! (Electron) oferecia todas as placas de rede com o endereço de verdade: IPv6, rede virtual de
//! jogo (Radmin, Hamachi), segunda placa. Medido em 14/11/2026 na mesma máquina: 2 endereços no
//! WebView2 contra 9 no Electron, e duas pessoas que se viam no app antigo deixaram de se ver.
//!
//! O Chromium libera a lista completa para a página que tem permissão de câmera (é a regra dele,
//! não há opção separada). O Nimbo já usa a câmera nas lives de webcam, então o app concede essa
//! permissão à própria página, e só a ela. O microfone não é concedido.

use tauri::WebviewWindow;
use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_PERMISSION_KIND_CAMERA, COREWEBVIEW2_PERMISSION_STATE_ALLOW, ICoreWebView2_13,
    ICoreWebView2Profile4,
};
use webview2_com::SetPermissionStateCompletedHandler;
use windows::core::{HSTRING, Interface};

/// Concede a câmera à origem do Nimbo no perfil do navegador embutido. Falhar aqui só mantém o
/// comportamento padrão (menos caminhos de rede); o app segue funcionando.
pub fn open_paths(window: &WebviewWindow, origin: String) {
    let granted = window.with_webview(move |webview| {
        // SAFETY: chamadas COM no navegador embutido, feitas na thread da interface (é onde o
        // Tauri roda este bloco); as interfaces são contadas por referência pelos próprios tipos.
        let result = unsafe {
            (|| -> windows::core::Result<()> {
                let profile = webview
                    .controller()
                    .CoreWebView2()?
                    .cast::<ICoreWebView2_13>()?
                    .Profile()?
                    .cast::<ICoreWebView2Profile4>()?;
                let done = SetPermissionStateCompletedHandler::create(Box::new(|result| {
                    if let Err(e) = result {
                        eprintln!("[nimbo] permissão de rede não aplicada: {e}");
                    }
                    Ok(())
                }));
                profile.SetPermissionState(
                    COREWEBVIEW2_PERMISSION_KIND_CAMERA,
                    &HSTRING::from(origin),
                    COREWEBVIEW2_PERMISSION_STATE_ALLOW,
                    &done,
                )
            })()
        };
        if let Err(e) = result {
            eprintln!("[nimbo] caminhos de rede restritos (WebView2 antigo?): {e}");
        }
    });
    if let Err(e) = granted {
        eprintln!("[nimbo] caminhos de rede restritos: {e}");
    }
}
