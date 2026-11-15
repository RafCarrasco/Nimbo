# Nimbo para Windows (app em Rust)

Este é o app do Nimbo a partir da versão 0.5.0. A janela é Tauri 2 e mostra o próprio site; o que é
pesado roda em Rust: captura de tela, codificação pela placa de vídeo, envio das lives, som do jogo,
lista de telas e janelas, instalação e atualização. O plano e as medições estão em
[`docs/rust-app.md`](../../docs/rust-app.md).

O app antigo, em Electron (`desktop/main.cjs`), não é mais distribuído. O instalador deste app o
remove (`installer-hooks.nsh`); o login e as preferências do antigo ficam em outra pasta e não são tocados.

| | App antigo (Electron) | Este app |
|---|---|---|
| Pasta de instalação | `%LOCALAPPDATA%\Programs\Nimbo` | `%LOCALAPPDATA%\Nimbo` |
| Executável | `Nimbo.exe` | `nimbo-app.exe` |
| Dados | `%APPDATA%\Nimbo` | `%LOCALAPPDATA%\app.nimbo.rust` |
| Atalhos | "Nimbo" | "Nimbo" (tomam o lugar dos antigos) |
| Instalador | `Nimbo-Setup.exe` (até a 0.4.1) | `Nimbo-Setup.exe` (da 0.5.0 em diante) |
| Atualização | `latest.yml` | `latest.json` do Release mais recente |

## Peças

| Pasta | O que faz |
|---|---|
| `desktop/rust-app` | Janela, ponte com o site (`window.nimboDesktop`), login, atualização, instalador |
| `desktop/rust-media` | Motor de mídia: captura, codificador da placa, envio WebRTC, som em Opus |
| `desktop/rust-audio` | Captura do som do jogo (ou de tudo menos o Nimbo) |
| `desktop/rust-native` | Telas, janelas, miniaturas, ícones, vigia do jogo |

## Rodar e testar

O Rust fica em `%USERPROFILE%\.cargo\bin`. O motor de mídia precisa do CMake no PATH (o que vem com o
Visual Studio Build Tools serve):

```powershell
$env:PATH = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin;$env:USERPROFILE\.cargo\bin;$env:PATH"
```

```powershell
cargo run --release --manifest-path desktop/rust-app/Cargo.toml
```

```powershell
cargo test --locked --release --manifest-path desktop/rust-app/Cargo.toml
```

Variáveis de ambiente, só para desenvolvimento e teste:

| Variável | Para quê |
|---|---|
| `NIMBO_URL` | Outro endereço do site (ex.: `http://localhost:5173/?devUser=owner` com o emulador). |
| `NIMBO_APP_ID` e `NIMBO_DATA_DIR` | Outro identificador e outra pasta de dados, para não misturar com o app instalado. |
| `NIMBO_LOGIN_PRINT` | Imprime o link de login em vez de abrir o navegador. |
| `NIMBO_SELFTEST=<arquivo>` | Confere a ponte com o site carregado, grava o resultado e fecha. |
| `NIMBO_SELFTEST_SCRIPT=<arquivo .js>` | Com a anterior, roda esse roteiro dentro da página no lugar da conferência. |
| `NIMBO_UPDATE_URL` | Outro endereço do `latest.json`, para testar a atualização (https, ou http só em 127.0.0.1). |

## Instalador

```powershell
powershell -ExecutionPolicy Bypass -File desktop/rust-app/scripts/pack.ps1
```

Precisa do `tauri-cli` (`cargo install tauri-cli --version "^2" --locked`) e da chave de atualização
(abaixo). Gera em `desktop/rust-app/release/`:

- `Nimbo-Setup.exe` (~5 MB): instala por usuário, sem pedir administrador, remove o app antigo e
  cria os atalhos "Nimbo". Se a máquina não tiver o WebView2, o instalador baixa.
- `Nimbo-Setup.exe.sig`: assinatura de atualização.
- `latest.json`: o que o app instalado lê para saber da versão nova.

O mesmo roda no GitHub em **Actions > Nimbo installer (Rust app) > Run workflow**. O workflow só deixa
os arquivos como artefatos; não cria Release.

## Atualização automática

O app confere `https://github.com/RafCarrasco/Nimbo/releases/latest/download/latest.json` ao abrir e
a cada 30 minutos, baixa a versão nova em segundo plano e avisa o site, que mostra "Reiniciar agora".
Fechar o app com uma atualização baixada também instala. Falhas vão só para `nimbo-update.log`, na
pasta de dados.

Duas regras para o canal não quebrar:

- **Todo Release marcado como "latest" precisa ter os três arquivos acima.** Um Release "latest" sem
  `latest.json` deixa todos os apps instalados sem atualização até o próximo.
- **Não publique mais Release do app antigo como "latest".**

## Chave de atualização

O app só aceita atualização assinada com a chave cujo par público está em `tauri.conf.json`
(`plugins.updater.pubkey`). A chave privada **não fica no repositório**:

- na máquina de quem publica: `%USERPROFILE%\.nimbo-chaves\nimbo.key` e a senha em
  `%USERPROFILE%\.nimbo-chaves\nimbo.key.senha`;
- no GitHub: segredos `TAURI_SIGNING_PRIVATE_KEY` e `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.

**Guarde uma cópia dessa pasta fora do computador.** Sem a chave não dá para publicar atualização
para quem já instalou: seria preciso todo mundo baixar o instalador de novo.

## Publicar uma versão

1. Suba `version` em `desktop/rust-app/Cargo.toml` e em `package.json`, e escreva as novidades em
   `src/lib/changelog.ts`. Mescle na `main` por PR.
2. Gere o instalador (comando acima, ou o workflow).
3. Crie o Release e suba os três arquivos:

   ```powershell
   gh release create v0.5.1 desktop/rust-app/release/Nimbo-Setup.exe desktop/rust-app/release/Nimbo-Setup.exe.sig desktop/rust-app/release/latest.json --repo RafCarrasco/Nimbo --title "Nimbo 0.5.1" --notes "O que mudou"
   ```

4. Publique o site (`firebase deploy --project golive-ad118 --only hosting,database`).

## Testar a atualização sem publicar

1. Gere duas versões com `pack.ps1 -Config <json de teste> -OutDir <pasta>`, com outro nome de
   produto e outro identificador no JSON de teste, para não mexer no Nimbo instalado.
2. Instale a primeira (`Setup.exe /S /D=<pasta de teste>`).
3. Sirva `latest.json` e `Setup.exe` da segunda em `http://127.0.0.1:8765`.
4. Abra a instalada com `NIMBO_UPDATE_URL=http://127.0.0.1:8765/latest.json`.
