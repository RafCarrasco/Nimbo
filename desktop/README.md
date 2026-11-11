# Nimbo para Windows

Janela própria que abre o Nimbo publicado: atualizações do site chegam sem novo executável. O app
acrescenta o que o navegador não faz bem:

- **Seletor de tela ou aplicativo** com miniaturas (`capture-picker.html`).
- **Áudio sem eco**: `native/NimboAudio.cs` usa a captura de áudio por processo do Windows (10 2004+).
  Tela inteira transmite todo o som do PC menos o próprio Nimbo; um aplicativo transmite só o som dele.
- **Login do Google pelo navegador do sistema** (o Google bloqueia login dentro de apps): o app abre
  `/desktop-login` com uma porta local e um código de uso único, e a página devolve a credencial.

## Gerar o executável (local)

```powershell
npm run desktop:pack
```

Saída em `release/`: `Nimbo-Setup.exe` (instalador de um clique por usuário, sem administrador, com atalhos
e desinstalação em Aplicativos instalados), `Nimbo-Setup.exe.blockmap` e `latest.yml`. Não é assinado: o
SmartScreen avisa na primeira vez.

## Atualização automática

O app instalado consulta o último Release de `RafCarrasco/Nimbo` ao abrir e a cada 6 horas
(`electron-updater`). Para publicar uma versão: suba `version` no `package.json`, rode `npm run desktop:pack`
e anexe ao Release os três arquivos acima. Mudanças só no site não precisam de versão nova do app.
O capturador de áudio é compilado com o `csc.exe` que já vem no Windows. O ícone (`icon.ico`, usado no
executável, atalhos, barra de tarefas e instalador) vem do pacote em `assets/brand/`; os antigos ficam
em `assets/brand/legacy/`.

## Testar com os emuladores

```powershell
$env:NIMBO_URL='http://localhost:5173'; $env:NIMBO_LOGIN_PRINT='1'; npm run desktop:dev
```

O link de login sai no terminal; abra-o com `&devUser=nome` para entrar com uma conta fictícia.

## Limites

Jogos em tela cheia exclusiva podem aparecer pretos: use janela sem bordas.

## App em Rust (prévia, ainda não substitui o app atual)

`desktop/rust/` contém o novo app nativo (Rust + WebView2), construído por etapas. O app Electron
descrito acima continua sendo o único distribuído; nada aqui altera o instalador, as atualizações
ou o site.

Estado da etapa 1: uma janela Windows (1280×800, mínimo 850×590, título e ícone do Nimbo, fundo
`#0b1020`) abre o Nimbo publicado no WebView2, com as mesmas regras do Electron para links externos,
pop-ups, permissões, recarregar (F5/Ctrl+R), versão no user agent e instância única. Ainda **não**
existem: ponte `nimboDesktop`, login pelo navegador, link `golive://`, seletor/captura de tela,
áudio sem eco, modo jogo, instalador e atualização automática. Requer o Microsoft Edge WebView2
Runtime.

Testes portáveis (qualquer sistema, Rust 1.99.1 fixado em `desktop/rust/rust-toolchain.toml`):

```powershell
cargo +1.99.1 test --manifest-path desktop/rust/Cargo.toml --locked
cargo +1.99.1 test --manifest-path desktop/rust/Cargo.toml --locked --features test-host
node --test desktop/tests/scope.test.mjs desktop/tests/windows-workflow.test.mjs desktop/tests/release-guard.test.mjs
```

Gerar no Windows x64 (Visual Studio Build Tools com MSVC e Windows SDK):

```powershell
cargo +1.99.1 build --manifest-path desktop/rust/Cargo.toml --locked --release
```

Saída: `desktop/rust/target/release/nimbo-desktop.exe` (prévia, não é instalador nem release). O
recurso `test-host` só compila em depuração; com `--release` o build falha de propósito. O teste
interativo do app (servidor local `127.0.0.1`, sem site publicado, sem login) e a matriz de paridade
com o Electron estão em `desktop/rust/README.md` e `desktop/tests/e2e/shell-parity.md`.
