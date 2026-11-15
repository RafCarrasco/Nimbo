; Ganchos do instalador (Tauri/NSIS).
;
; O Nimbo antigo, feito em Electron, fica em %LOCALAPPDATA%\Programs\Nimbo. Este app o substitui:
; sem remover o antigo ficariam dois "Nimbo" instalados, com atalhos de mesmo nome.
;
; A remoção é feita aqui mesmo, e não chamando o desinstalador do app antigo: em teste ele fechou o
; app mas não apagou os arquivos. O login e as preferências do antigo (%APPDATA%\Nimbo) ficam.
;
; O identificador 59fb6a3e-... é o que o instalador do app antigo deriva do appId
; "app.nimbo.desktop": é o mesmo em todas as máquinas.
!macro NSIS_HOOK_PREINSTALL
  IfFileExists "$LOCALAPPDATA\Programs\Nimbo\Nimbo.exe" 0 nimbo_antigo_fim
    ; O executável do app novo se chama nimbo-app.exe: este comando só alcança o antigo.
    nsExec::Exec 'taskkill /F /T /IM Nimbo.exe'
    Pop $0
    Sleep 1000
    RMDir /r "$LOCALAPPDATA\Programs\Nimbo"
    ; Instaladores de atualização que o app antigo já tinha baixado.
    RMDir /r "$LOCALAPPDATA\nimbo-updater"
    DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\59fb6a3e-5e80-5dc7-a1d6-78a13ec80b22"
    DeleteRegKey HKCU "Software\59fb6a3e-5e80-5dc7-a1d6-78a13ec80b22"
  nimbo_antigo_fim:
!macroend

; O instalador que o app antigo tinha baixado (100 MB) às vezes ainda está preso por um instante
; logo depois de o app fechar: tenta de novo no fim da instalação.
!macro NSIS_HOOK_POSTINSTALL
  RMDir /r "$LOCALAPPDATA\nimbo-updater"
!macroend
