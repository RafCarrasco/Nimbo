# Nimbo em Rust (`desktop/rust-app`)

A casca do app de Windows em Rust (Tauri 2): uma janela que mostra o site do Nimbo no WebView2 e
entrega a ele a ponte `window.nimboDesktop` (login, motor de mídia, atualização). O plano geral
está em `docs/rust-app.md`; o contrato da ponte, em `src/desktop.ts`.

Ela é distribuída como **Nimbo Beta**, ao lado do Nimbo atual (Electron). Os dois podem ficar
instalados juntos:

| | Nimbo (Electron) | Nimbo Beta (Rust) |
|---|---|---|
| Identificador | `app.nimbo.desktop` | `app.nimbo.rust` |
| Pasta de instalação | `%LOCALAPPDATA%\Programs\nimbo` | `%LOCALAPPDATA%\Nimbo Beta` |
| Executável | `Nimbo.exe` | `nimbo-beta.exe` |
| Dados (login, cookies) | `%APPDATA%\Nimbo` | `%LOCALAPPDATA%\app.nimbo.rust` |
| Atalhos | "Nimbo" | "Nimbo Beta" |
| Instalador | `Nimbo-Setup.exe` | `Nimbo-Beta-Setup.exe` |
| Atualização | `latest.yml` do Release "latest" | `latest-beta.json` do Release `rust-beta` |

## Rodar

O Rust não fica no PATH nesta máquina; use o caminho inteiro. O motor de mídia compila o libopus
com o **CMake**, que precisa estar no PATH (o que vem com o Visual Studio Build Tools serve):

```powershell
$env:PATH = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin;$env:PATH"
cd desktop\rust-app
& "$env:USERPROFILE\.cargo\bin\cargo.exe" run --release
```

Variáveis de ambiente que a casca lê (todas opcionais, só para teste):

| Variável | Para quê |
|---|---|
| `NIMBO_URL` | Outro endereço no lugar do site publicado (ex.: `http://localhost:5173`). |
| `NIMBO_APP_ID` | Outro identificador: não divide trava de instância única nem dados com o app instalado. |
| `NIMBO_DATA_DIR` | Pasta dos dados do WebView2 (login, cookies) e do `nimbo-update.log`. |
| `NIMBO_LOGIN_PRINT` | Escreve o link de login no stdout em vez de abrir o navegador. |
| `NIMBO_SELFTEST=<arquivo>` | Confere a ponte com o site carregado, grava o relatório no arquivo e fecha. |
| `NIMBO_SELFTEST_SCRIPT=<arquivo .js>` | Com a anterior, roda esse roteiro no lugar da conferência padrão. |
| `NIMBO_UPDATE_URL` | Outro endereço do `latest-beta.json`, para testar a atualização (ver abaixo). |

Só para gerar o instalador (lidas pelo `tauri-cli`, nunca pelo app):
`TAURI_SIGNING_PRIVATE_KEY` e `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.

## Testar

```powershell
cd desktop\rust-app
$cargo = "$env:USERPROFILE\.cargo\bin\cargo.exe"
& $cargo fmt -- --check
& $cargo clippy --locked --release --all-targets -- -D warnings
& $cargo test --locked --release
```

Com o site de verdade carregado (abre a janela, confere a ponte e fecha sozinho):

```powershell
$env:NIMBO_APP_ID = 'app.nimbo.rusttest'
$env:NIMBO_DATA_DIR = "$env:TEMP\nimbo-rust-selftest"
$env:NIMBO_SELFTEST = "$env:TEMP\nimbo-rust-selftest.txt"
& $cargo run --release
Get-Content $env:NIMBO_SELFTEST
```

## Versão

A versão do app vem de **um lugar só: `version` no `Cargo.toml`**. O `tauri.conf.json` não tem
`version` de propósito; sem ela o Tauri usa a do Cargo no executável, no instalador, na assinatura
e no que o app informa à página (`nimboDesktop.appVersion`). Para lançar, suba a versão ali.

## Instalador

Precisa do `tauri-cli` (uma vez):

```powershell
& "$env:USERPROFILE\.cargo\bin\cargo.exe" install tauri-cli --version "^2" --locked
```

Com a chave de atualização nas duas variáveis de ambiente (seção seguinte):

```powershell
.\desktop\rust-app\scripts\pack.ps1
```

O script acha sozinho o `cargo` e o CMake do Visual Studio quando não estão no PATH.
Sai em `desktop\rust-app\release\` (fora do git):

- `Nimbo-Beta-Setup.exe` (~5 MB): instalador NSIS, por usuário, sem pedir administrador. Se a
  máquina não tiver o WebView2, o instalador baixa e instala. Aberto com dois cliques ele mostra
  as telas de instalação em português (o do Electron instala direto); com `/S` instala sem janela.
  Cria os atalhos "Nimbo Beta" na área de trabalho e no menu Iniciar e a entrada "Nimbo Beta" em
  Aplicativos instalados. Desinstalar não apaga o login, a não ser que a pessoa marque a opção.
- `Nimbo-Beta-Setup.exe.sig`: assinatura de atualização.
- `latest-beta.json`: o que o app instalado lê para saber da versão nova.

O script para com erro se a chave pública do `tauri.conf.json` ainda for o marcador, se a chave
privada não for o par dela ou se a assinatura não levar a versão.

O mesmo roda no GitHub em **Actions > Nimbo Beta installer (Rust app) > Run workflow**
(`.github/workflows/rust-app-release.yml`). Ele só deixa os três arquivos como artefato do
workflow: não cria Release nem tag.

## Atualização automática

O app instalado confere `latest-beta.json` ao abrir e a cada 6 horas, baixa a versão nova em
segundo plano, confere a assinatura e avisa o site, que mostra o cartão "pronto" e pede a
instalação (`watchUpdate`, `onUpdateProgress`, `installUpdate`). Instala sem janela e reabre.
Se a pessoa fechar o app com a atualização baixada, ela é instalada ao fechar, sem reabrir.

Falha de rede, 404 ou assinatura errada não aparecem para a pessoa: vão para
`%LOCALAPPDATA%\app.nimbo.rust\nimbo-update.log`, e a conferência tenta de novo (30 s, 2 min,
10 min, depois o intervalo normal). Build de desenvolvimento (`cargo run`) não se atualiza.

### Onde fica o canal e por que não esbarra no Electron

O app em Electron procura `latest.yml` no Release marcado como **latest**, e o link de download
do pessoal é `releases/latest/download/Nimbo-Setup.exe`. Se um Release do beta virasse "latest",
os dois quebrariam. Por isso o beta nunca usa "latest":

- existe **um Release fixo, com a tag `rust-beta`, marcado como pré-lançamento** (o GitHub nunca
  escolhe pré-lançamento como "latest");
- o app em Rust lê sempre
  `https://github.com/RafCarrasco/Nimbo/releases/download/rust-beta/latest-beta.json`;
- a cada versão do beta, os três arquivos desse Release são trocados.

O link para o pessoal baixar o beta também é fixo:
`https://github.com/RafCarrasco/Nimbo/releases/download/rust-beta/Nimbo-Beta-Setup.exe`.

### Chave de assinatura (uma vez só)

O app só instala uma atualização assinada pela chave privada cujo par público está dentro dele.
**A chave privada e a senha nunca entram no repositório.** Quem perde a chave privada não
consegue mais atualizar quem já instalou: guarde uma cópia fora do computador.

1. Crie o par de chaves fora do repositório. O comando pede uma senha (duas vezes); escolha uma
   e guarde.

   ```powershell
   New-Item -ItemType Directory -Force "$env:USERPROFILE\.nimbo-chaves" | Out-Null
   & "$env:USERPROFILE\.cargo\bin\cargo.exe" tauri signer generate -w "$env:USERPROFILE\.nimbo-chaves\nimbo-beta.key"
   ```

2. Cole a chave **pública** no repositório: abra `desktop/rust-app/tauri.conf.json` e troque o
   valor de `plugins.updater.pubkey` (hoje um marcador que deixa a atualização desligada) pelo
   texto que este comando mostra. Commit normal, por PR.

   ```powershell
   Get-Content "$env:USERPROFILE\.nimbo-chaves\nimbo-beta.key.pub"
   ```

3. Cadastre a chave **privada** e a senha como secrets do repositório. Pelo site: **Settings >
   Secrets and variables > Actions > New repository secret**, com os nomes
   `TAURI_SIGNING_PRIVATE_KEY` (o conteúdo inteiro de `nimbo-beta.key`) e
   `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` (a senha). Ou pelo terminal:

   ```powershell
   Get-Content "$env:USERPROFILE\.nimbo-chaves\nimbo-beta.key" -Raw | gh secret set TAURI_SIGNING_PRIVATE_KEY --repo RafCarrasco/Nimbo
   gh secret set TAURI_SIGNING_PRIVATE_KEY_PASSWORD --repo RafCarrasco/Nimbo
   ```

   O segundo comando pede a senha sem mostrar na tela.

4. Para gerar o instalador na sua máquina, as mesmas duas variáveis, só na sessão do terminal:

   ```powershell
   $env:TAURI_SIGNING_PRIVATE_KEY = "$env:USERPROFILE\.nimbo-chaves\nimbo-beta.key"
   $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = Read-Host 'Senha da chave'
   .\desktop\rust-app\scripts\pack.ps1
   ```

### Publicar uma versão do beta

1. Suba `version` em `desktop/rust-app/Cargo.toml` (e rode `cargo build` para o `Cargo.lock`
   acompanhar). Merge na `main`.
2. Rode o workflow **Nimbo Beta installer (Rust app)** e baixe o artefato `nimbo-beta-installer`
   (ou rode `scripts\pack.ps1` na sua máquina).
3. Na primeira vez, crie o Release fixo, **como pré-lançamento**:

   ```powershell
   gh release create rust-beta --repo RafCarrasco/Nimbo --prerelease --latest=false --title "Nimbo Beta (app em Rust)" --notes "Canal de testes do app em Rust. O Nimbo de sempre continua no Release mais recente."
   ```

4. Troque os arquivos do Release, **o manifesto por último** (é ele que avisa os apps):

   ```powershell
   gh release upload rust-beta Nimbo-Beta-Setup.exe Nimbo-Beta-Setup.exe.sig --repo RafCarrasco/Nimbo --clobber
   gh release upload rust-beta latest-beta.json --repo RafCarrasco/Nimbo --clobber
   ```

Nunca marque o Release `rust-beta` como "latest" nem tire o "pré-lançamento": é isso que mantém
o link e a atualização do Nimbo atual apontando para o Release dele.

### Testar o ciclo inteiro sem tocar no GitHub

`NIMBO_UPDATE_URL` aponta o app para outro `latest-beta.json`. Aceita `https://` ou, só nesta
máquina, `http://127.0.0.1` e `http://localhost`. A assinatura continua valendo: o instalador
servido tem que ter sido assinado pela chave cujo par público está no app em teste.

Roteiro usado na validação (tudo com nome de produto, identificador e chave descartáveis):

1. Crie uma chave de teste fora do repositório (`cargo tauri signer generate --ci -p <senha> -w <arquivo>`).
2. Gere duas versões com um arquivo de ajustes para o `tauri-cli`, por exemplo
   `{"productName":"Nimbo UpdTest","mainBinaryName":"nimbo-updtest","identifier":"app.nimbotest.updtest","version":"0.1.0","bundle":{"publisher":"NimboTest"},"plugins":{"updater":{"pubkey":"<pública de teste>"}}}`
   e outro igual com `"version":"0.1.1"`:
   `scripts\pack.ps1 -Config a.json -OutDir <pasta>\a -BaseUrl http://127.0.0.1:8765 -ArtifactName Setup.exe`.
3. Instale a primeira sem janela numa pasta de teste: `Setup.exe /S /D=<pasta>\instalado`.
4. Sirva `latest-beta.json` e `Setup.exe` da segunda em `http://127.0.0.1:8765`.
5. Abra o app instalado com `NIMBO_UPDATE_URL=http://127.0.0.1:8765/latest-beta.json`,
   `NIMBO_APP_ID`, `NIMBO_DATA_DIR` e um roteiro em `NIMBO_SELFTEST_SCRIPT` que chame
   `watchUpdate`, `onUpdateProgress` e `installUpdate`.
6. Desinstale no fim: `<pasta>\instalado\uninstall.exe /S`.
