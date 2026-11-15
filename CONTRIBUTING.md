# Como trabalhar no Nimbo

## Branches

- **`main` é o que está no ar** (site e app). Toda branch nova sai dela, e tudo volta para ela por PR.
- Uma branch por tarefa, com prefixo: `feat/` (novidade), `fix/` (correção), `refactor/`, `docs/`, `ci/`.
- Não commite direto na `main`. Depois do merge, apague a branch.

```powershell
git switch main
git pull
git switch -c feat/nome-curto-da-tarefa
# ... trabalha, commita ...
git push -u origin feat/nome-curto-da-tarefa
gh pr create --base main --fill
```

Se a `main` andou enquanto você trabalhava, atualize a sua branch antes de pedir revisão:

```powershell
git fetch origin
git rebase origin/main
git push --force-with-lease
```

## O que é cada parte

| Pasta | O que é | Vai para o pessoal? |
|---|---|---|
| `src/` | Site (React + TypeScript + Vite) | Sim, pelo Firebase Hosting |
| `desktop/rust-app`, `rust-media`, `rust-audio`, `rust-native` | App de Windows (Rust): janela, motor de mídia, som, janelas | Sim, pelo Release no GitHub |
| `desktop/main.cjs`, `preload.cjs`, `native/` | App antigo (Electron), mantido só como caminho de volta | Não |
| `desktop/rust/` | Primeiro experimento em Rust (só diagnóstico) | Não |
| `scripts/build-rules.mjs` | Gera `database.rules.json` (não edite o JSON à mão) | Sim, junto com o site |

## Antes de abrir o PR

```powershell
npm run build
npm run test:desktop
npm run test:rules
```

Mexeu no app (`desktop/rust-*`)? Em cada pasta alterada, com o CMake no PATH (ver
`desktop/rust-app/README.md`):

```powershell
cargo fmt -- --check
```

```powershell
cargo clippy --locked --release --all-targets -- -D warnings
```

```powershell
cargo test --locked --release
```

O GitHub Actions roda tudo isso sozinho no PR: **Community validation** (site, regras e testes do app
antigo) e **Rust desktop** (cada pasta `desktop/rust-*`, em Windows). PR com check vermelho não entra.

## Publicar (só a partir da `main`)

- **Site e regras:** `npm run build`, depois
  `firebase deploy --project golive-ad118 --only hosting,database`. Regras e site vão sempre juntos.
- **App:** suba a versão em `desktop/rust-app/Cargo.toml` e em `package.json`, escreva as novidades em
  `src/lib/changelog.ts`, gere o instalador e crie o Release `vX.Y.Z` com `Nimbo-Setup.exe`,
  `Nimbo-Setup.exe.sig` e `latest.json`. O passo a passo e as duas regras do canal de atualização estão em
  `desktop/rust-app/README.md`. O app instalado se atualiza sozinho por esse Release.
- **Nunca publique um Release "latest" sem `latest.json`**, nem um Release do app antigo como "latest":
  os apps instalados ficariam sem atualização.

## Nunca

- Senha, token, chave privada ou credencial de TURN/Cloudflare no código ou em commit.
  A credencial do servidor de retransmissão é colada na aba **Rede** do painel.
- Segurança só escondendo botão no React: quem não pode, tem que ser barrado nas regras do banco.
- Fotos de perfil no Firebase Storage.
