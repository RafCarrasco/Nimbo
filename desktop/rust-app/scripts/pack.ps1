<#
.SYNOPSIS
  Gera o instalador do Nimbo (app em Rust) e o manifesto de atualização.

.DESCRIPTION
  Roda `cargo tauri build`, confere a assinatura e deixa em -OutDir os três arquivos de um
  lançamento: Nimbo-Setup.exe, Nimbo-Setup.exe.sig e latest.json.
  Não publica nada. Ver desktop/rust-app/README.md.

  Precisa de TAURI_SIGNING_PRIVATE_KEY (a chave privada ou o caminho do arquivo dela) e, se a
  chave tiver senha, TAURI_SIGNING_PRIVATE_KEY_PASSWORD. Nenhuma das duas vai para o repositório.

.PARAMETER BaseUrl
  Endereço da pasta onde o instalador vai ficar publicado. O manifesto aponta para
  <BaseUrl>/<ArtifactName>.

.PARAMETER Config
  Arquivo JSON com ajustes do tauri.conf.json só para este build (testes: outro nome de produto,
  outra chave pública, outra versão).
#>
param(
  # Vazio = o Release desta versão: https://github.com/RafCarrasco/Nimbo/releases/download/v<versão>
  [string]$BaseUrl = '',
  [string]$OutDir = '',
  [string]$Config = '',
  [string]$Notes = '',
  [string]$ArtifactName = 'Nimbo-Setup.exe',
  [string]$ManifestName = 'latest.json'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Fail([string]$message) {
  Write-Host "ERRO: $message" -ForegroundColor Red
  exit 1
}

# Identificador da chave (8 bytes) dentro de uma chave pública ou de uma assinatura do minisign,
# as duas em base64 de um texto cuja segunda linha é o conteúdo.
function Get-KeyId([string]$encoded) {
  $text = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($encoded.Trim()))
  $raw = [Convert]::FromBase64String(($text -split "`n")[1].Trim())
  return [BitConverter]::ToString($raw[2..9])
}

$app = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $OutDir) { $OutDir = Join-Path $app 'release' }

# O Rust costuma não estar no PATH; o tauri-cli chama `cargo` pelo nome.
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
if (Test-Path $cargoBin) { $env:PATH = "$cargoBin;$env:PATH" }
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { Fail 'cargo não encontrado. Instale o Rust (https://rustup.rs).' }
if (-not (Get-Command cargo-tauri -ErrorAction SilentlyContinue)) {
  Fail 'tauri-cli não encontrado. Instale com: cargo install tauri-cli --version "^2" --locked'
}

# O libopus do motor de mídia é compilado pelo CMake, que também costuma ficar fora do PATH:
# procura o que vem com o Visual Studio / Build Tools.
if (-not (Get-Command cmake -ErrorAction SilentlyContinue)) {
  $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
  $studios = @()
  if (Test-Path $vswhere) { $studios = @(& $vswhere -products * -property installationPath) }
  $cmakeBin = $studios |
    ForEach-Object { Join-Path $_ 'Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin' } |
    Where-Object { Test-Path (Join-Path $_ 'cmake.exe') } |
    Select-Object -First 1
  if (-not $cmakeBin) { Fail 'cmake não encontrado. Instale o CMake ou o componente "C++ CMake tools" do Visual Studio Build Tools.' }
  $env:PATH = "$cmakeBin;$env:PATH"
}

if (-not $env:TAURI_SIGNING_PRIVATE_KEY) {
  Fail 'defina TAURI_SIGNING_PRIVATE_KEY (a chave privada de atualização ou o caminho do arquivo). Ver README.md.'
}
# Colada com quebra de linha no fim (acontece no secret do GitHub), a chave não decodifica.
$env:TAURI_SIGNING_PRIVATE_KEY = $env:TAURI_SIGNING_PRIVATE_KEY.Trim()
if (-not $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD) {
  # Com --ci o tauri-cli não pede a senha no terminal: sem a variável, tenta a senha vazia.
  Write-Host 'Aviso: TAURI_SIGNING_PRIVATE_KEY_PASSWORD vazia; vale só para chave criada sem senha.' -ForegroundColor Yellow
}

# Chave pública que vai dentro do app: a do tauri.conf.json, ou a do arquivo de ajustes do teste.
$settings = Get-Content (Join-Path $app 'tauri.conf.json') -Raw -Encoding UTF8 | ConvertFrom-Json
$pubkey = $settings.plugins.updater.pubkey
$buildArgs = @('tauri', 'build', '--ci')
if ($Config) {
  $Config = (Resolve-Path $Config).Path
  $override = Get-Content $Config -Raw -Encoding UTF8 | ConvertFrom-Json
  $fromOverride = $null
  try { $fromOverride = $override.plugins.updater.pubkey } catch { }
  if ($fromOverride) { $pubkey = $fromOverride }
  $buildArgs += @('--config', $Config)
}
if ($pubkey -notmatch '^[A-Za-z0-9+/=]{64,}$') {
  Fail 'a chave pública em tauri.conf.json (plugins.updater.pubkey) ainda é o marcador. Crie a chave e cole a pública lá (README.md).'
}

# Instaladores de builds anteriores ficam na mesma pasta; sobra só o deste build.
$bundle = Join-Path $app 'target\release\bundle\nsis'
if (Test-Path $bundle) { Remove-Item $bundle -Recurse -Force }

Push-Location $app
try {
  & cargo @buildArgs -- --locked
  if ($LASTEXITCODE -ne 0) { Fail "cargo tauri build falhou (código $LASTEXITCODE)." }
} finally {
  Pop-Location
}

$installers = @(Get-ChildItem $bundle -Filter '*-setup.exe')
if ($installers.Count -ne 1) { Fail "esperava um instalador em $bundle, achei $($installers.Count)." }
$installer = $installers[0]
$signatureFile = "$($installer.FullName).sig"
if (-not (Test-Path $signatureFile)) { Fail "o tauri-cli não gerou $signatureFile (bundle.createUpdaterArtifacts)." }
if ($installer.Name -notmatch '_(?<version>\d+\.\d+\.\d+[^_]*)_x64-setup\.exe$') {
  Fail "não reconheci a versão no nome $($installer.Name)."
}
$version = $Matches.version
$signature = (Get-Content $signatureFile -Raw).Trim()

# Assinado com outra chave, nenhum app instalado aceitaria a atualização: melhor parar aqui.
if ((Get-KeyId $signature) -ne (Get-KeyId $pubkey)) {
  Fail 'a assinatura não é da chave pública que está no app. A chave privada usada não é o par dela.'
}
# O app exige a versão dentro da assinatura (requireSignedVersion).
$signed = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($signature))
if ($signed -notmatch "version:$([regex]::Escape($version))(\s|$)") {
  Fail "a assinatura não traz a versão $version. Atualize o tauri-cli."
}

New-Item -ItemType Directory -Force $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path
Copy-Item $installer.FullName (Join-Path $OutDir $ArtifactName) -Force
Copy-Item $signatureFile (Join-Path $OutDir "$ArtifactName.sig") -Force

if (-not $Notes) { $Notes = "Nimbo $version" }
# O manifesto aponta para o instalador desta versão, e não para "o mais recente": quem lê um
# manifesto antigo tem que baixar o arquivo que a assinatura dele confere.
if (-not $BaseUrl) { $BaseUrl = "https://github.com/RafCarrasco/Nimbo/releases/download/v$version" }
$manifest = [ordered]@{
  version   = $version
  notes     = $Notes
  pub_date  = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
  platforms = [ordered]@{
    'windows-x86_64' = [ordered]@{
      signature = $signature
      url       = "$($BaseUrl.TrimEnd('/'))/$ArtifactName"
    }
  }
}
# Sem BOM: o app lê este arquivo como JSON puro.
$json = $manifest | ConvertTo-Json -Depth 5
[IO.File]::WriteAllText((Join-Path $OutDir $ManifestName), $json, (New-Object Text.UTF8Encoding($false)))

Write-Host ''
Write-Host "Nimbo $version pronto em $OutDir"
Get-ChildItem $OutDir -File | Where-Object { $_.Name -in @($ArtifactName, "$ArtifactName.sig", $ManifestName) } |
  ForEach-Object { '{0,12:N0} bytes  {1}' -f $_.Length, $_.Name }
Write-Host "SHA256 $((Get-FileHash (Join-Path $OutDir $ArtifactName) -Algorithm SHA256).Hash)  $ArtifactName"
