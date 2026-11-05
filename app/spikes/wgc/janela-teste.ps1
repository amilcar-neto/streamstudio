# Janela comum WinForms, sem conteúdo pessoal, para repetir a prova de captura.
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
Add-Type 'using System; using System.Runtime.InteropServices; public class JanelaWgc { [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n); }'
$form = New-Object System.Windows.Forms.Form
$form.Text = 'StreamStudio - teste WGC F1.1'
$form.ClientSize = New-Object System.Drawing.Size(960, 540)
$form.BackColor = [System.Drawing.Color]::MidnightBlue
$label = New-Object System.Windows.Forms.Label
$label.Dock = 'Fill'
$label.TextAlign = 'MiddleCenter'
$label.ForeColor = [System.Drawing.Color]::White
$label.Font = New-Object System.Drawing.Font('Segoe UI', 28)
$form.Controls.Add($label)
$script:quadro = 0
$timer = New-Object System.Windows.Forms.Timer
$timer.Interval = 16
$timer.Add_Tick({
    if ($script:quadro -eq 0) { [void][JanelaWgc]::ShowWindow($form.Handle, 5) }
    $script:quadro++
    $label.Text = "StreamStudio - prova WGC`nQuadro de teste: $script:quadro"
})
$timer.Start()
$form.Add_FormClosed({ $timer.Stop(); $timer.Dispose() })
[System.Windows.Forms.Application]::Run($form)
