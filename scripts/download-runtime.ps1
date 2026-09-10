#Requires -Version 5.1
param(
    [Parameter(Mandatory)][ValidateSet('x86', 'x64')][string]$Architecture,
    [Parameter(Mandatory)][string]$Destination,
    [switch]$Silent
)
$ErrorActionPreference = 'Stop'
try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    $client = New-Object Net.WebClient
    $url = "https://aka.ms/vc14/vc_redist.$Architecture.exe"
    if ($Silent) {
        $client.DownloadFile($url, $Destination)
    } else {
        Add-Type -AssemblyName System.Windows.Forms
        $form = New-Object Windows.Forms.Form
        $form.Text = "小狼毫RS：下载 VC++ 运行库 ($Architecture)"
        $form.Width = 480
        $form.Height = 130
        $form.StartPosition = 'CenterScreen'
        $form.FormBorderStyle = 'FixedDialog'
        $form.MaximizeBox = $false
        $form.MinimizeBox = $false
        $bar = New-Object Windows.Forms.ProgressBar
        $bar.SetBounds(20, 25, 420, 24)
        $form.Controls.Add($bar)
        $script:downloadError = $null
        $script:downloadComplete = $false
        $client.add_DownloadProgressChanged({ param($sender, $event) $bar.Value = $event.ProgressPercentage })
        $client.add_DownloadFileCompleted({
            param($sender, $event)
            $script:downloadError = $event.Error
            $script:downloadComplete = !$event.Cancelled -and !$event.Error
            $form.Close()
        })
        $form.add_Shown({ $client.DownloadFileAsync([Uri]$url, $Destination) })
        $null = $form.ShowDialog()
        if (!$script:downloadComplete) {
            $client.CancelAsync()
            throw "Download cancelled or failed: $script:downloadError"
        }
        $form.Dispose()
    }
    $client.Dispose()
    $signature = Get-AuthenticodeSignature -LiteralPath $Destination
    if ($signature.Status -ne 'Valid' -or !$signature.SignerCertificate -or
        $signature.SignerCertificate.Subject -notmatch 'O=Microsoft Corporation(?:,|$)') {
        throw 'Invalid Microsoft signature.'
    }
    exit 0
} catch {
    Write-Error $_ -ErrorAction Continue
    exit 1
}
