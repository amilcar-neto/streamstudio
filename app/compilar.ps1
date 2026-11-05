param([switch]$Executar, [switch]$Testar, [switch]$Instalador)
$ErrorActionPreference = 'Stop'
if ($Instalador -and ($Testar -or $Executar)) { throw 'Use -Instalador separado de -Testar e -Executar.' }
$raiz = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
# Dependências, compilação e dados do WebView2 ficam no repositório.
$env:CARGO_HOME = Join-Path $raiz 'dev/estado/cargo'
$env:CARGO_TARGET_DIR = Join-Path $raiz 'dev/estado/app'
$env:TEMP = Join-Path $raiz 'dev/estado/tmp'
$env:TMP = $env:TEMP
$env:CARGO_BUILD_JOBS = '4'
# O SDK do LiveKit (libwebrtc pré-compilada) exige o CRT estático no Windows (F1.3/F1.4).
$env:RUSTFLAGS = '-C target-feature=+crt-static'
$env:WEBVIEW2_USER_DATA_FOLDER = Join-Path $raiz 'dev/estado/app-webview'
New-Item -ItemType Directory -Force -Path $env:TEMP | Out-Null
$ErrorActionPreference = 'Continue'
$manifesto = Join-Path $PSScriptRoot 'src-tauri/Cargo.toml'
# Builds de desenvolvimento e testes usam outro identificador: não brigam com o app
# instalado (instância única) nem pegam os dados dele. O instalador usa o do tauri.conf.json.
if (-not $Instalador) { $env:TAURI_CONFIG = '{"identifier":"com.streamstudio.dev","app":{"windows":[{"label":"main","title":"StreamStudio (dev)","width":1280,"height":800,"minWidth":640,"minHeight":400}]}}' }
# -Testar roda os testes (inclusive contra a API local, que o próprio teste sobe com o python)
if ($Instalador) {
    # CLI fixada e ferramentas NSIS locais; sem instalação global.
    $cli = Join-Path $raiz 'dev/bin/tauri/node_modules/.bin/tauri.cmd'
    if (-not (Test-Path $cli)) {
        npm install --prefix (Join-Path $raiz 'dev/bin/tauri') --cache (Join-Path $raiz 'dev/estado/npm-cache') --no-audit --no-fund '@tauri-apps/cli@2.12.4'
        if ($LASTEXITCODE -ne 0) { throw 'Não foi possível preparar a CLI do Tauri.' }
    }
    # F5.2: assina o instalador para a atualização automática. A chave fica em segredos/
    # (fora do git). Sem ela o build falha: o tauri.conf.json pede os arquivos de atualização.
    $chave = Join-Path $raiz 'segredos/atualizacao.key'
    if (-not (Test-Path $chave)) { throw "Falta $chave (chave das atualizações). Gere uma com: npx @tauri-apps/cli@2 signer generate -w segredos/atualizacao.key" }
    $env:TAURI_SIGNING_PRIVATE_KEY = $chave
    $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = ''
    Push-Location $PSScriptRoot
    try { & $cli build --bundles nsis -- --locked; $codigo = $LASTEXITCODE } finally { Pop-Location }
} elseif ($Testar) { cargo test --locked --manifest-path $manifesto; $codigo = $LASTEXITCODE } else { cargo build --locked --manifest-path $manifesto; $codigo = $LASTEXITCODE }
$ErrorActionPreference = 'Stop'
if ($codigo -ne 0) { throw "A compilação falhou (código $codigo)." }
if ($Executar) {
    & (Join-Path $env:CARGO_TARGET_DIR 'debug/streamstudio.exe')
}
