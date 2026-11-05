# Ensaio automático da F1.2 (sem jogo nem Discord).
# tom-a.exe toca 440 Hz; um powershell "pai" abre tom-b.exe (880 Hz) como filho.
# Grava 10 s em três modos ao mesmo tempo e mede as duas frequências.
$ErrorActionPreference = 'Stop'
$raiz = Resolve-Path "$PSScriptRoot\..\..\.."
$d = "$PSScriptRoot\target\debug"
$out = "$raiz\dev\estado\audio"
New-Item -ItemType Directory -Force $out | Out-Null
Copy-Item "$d\tom.exe" "$d\tom-a.exe" -Force
Copy-Item "$d\tom.exe" "$d\tom-b.exe" -Force
$exe = "$d\spike-audio-processo.exe"

$a = Start-Process "$d\tom-a.exe" -ArgumentList 440,14,0.03 -PassThru -WindowStyle Hidden
$pai = Start-Process powershell -ArgumentList '-NoProfile','-Command',"& '$d\tom-b.exe' 880 14 0.03" -PassThru -WindowStyle Hidden
Start-Sleep 1

$j = @(
  Start-Job { param($e,$o) & $e --incluir tom-a.exe --saida "$o\incluir-a.wav" 2>&1 } -ArgumentList $exe,$out
  Start-Job { param($e,$o) & $e --excluir tom-a.exe --saida "$o\excluir-a.wav" 2>&1 } -ArgumentList $exe,$out
  Start-Job { param($e,$o,$p) & $e --incluir $p --saida "$o\incluir-pai-b.wav" 2>&1 } -ArgumentList $exe,$out,$pai.Id
)
$j | Wait-Job | Receive-Job
$j | Remove-Job
foreach ($m in 'incluir-a','excluir-a','incluir-pai-b') { & $exe --analisar "$out\$m.wav" 440,880 }
$a, $pai | Wait-Process -Timeout 10 -ErrorAction SilentlyContinue
