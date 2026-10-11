param(
    [Parameter(Mandatory)][string]$BaselineBinary,
    [Parameter(Mandatory)][string]$CandidateBinary,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [ValidateSet('Tiny','Playback','Full')][string]$Matrix = 'Tiny',
    [ValidateSet(60,120,144)][int]$Fps = 120,
    [ValidateRange(1,20)][int]$Frames = 3,
    [ValidateRange(1,5)][int]$Rounds = 3,
    [ValidateRange(0,5)][int]$Warmup = 0,
    [ValidateRange(30,1800)][int]$TimeoutSeconds = 300,
    [switch]$SampleDeviceVram,
    [switch]$KeepPixels
)
$ErrorActionPreference = 'Stop'
$baseline = (Resolve-Path -LiteralPath $BaselineBinary).Path
$candidate = (Resolve-Path -LiteralPath $CandidateBinary).Path
$output = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Path $output -Force | Out-Null
$env:WGPU_BACKEND = 'vulkan'
$env:EFFECTCRAFT_REQUIRE_GPU = '1'
$sensor = if ($SampleDeviceVram) { (Get-Command nvidia-smi -ErrorAction SilentlyContinue).Source } else { $null }
if ($SampleDeviceVram -and -not $sensor) { throw 'Requested VRAM observations require nvidia-smi.' }
$suites = @('tiny')
if ($Matrix -ne 'Tiny') { $suites += 'playback' }
if ($Matrix -eq 'Full') { $suites += 'high-samples' }
$records = [Collections.Generic.List[object]]::new()
$comparisons = [Collections.Generic.List[object]]::new()

function Invoke-Measured([string]$binary, [string[]]$arguments, [string]$stem) {
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $binary
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    foreach ($argument in $arguments) { $info.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $info
    $peakWorkingSet = 0L
    $sampledPrivatePeak = 0L
    $devicePeakMiB = $null
    $deviceSamples = 0
    $nextSensor = 0L
    $started = $false
    $clock = [Diagnostics.Stopwatch]::StartNew()
    try {
        if (-not $process.Start()) { throw "Cannot start $binary" }
        $started = $true
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        while (-not $process.WaitForExit(20)) {
            $process.Refresh()
            $peakWorkingSet = [Math]::Max($peakWorkingSet, $process.PeakWorkingSet64)
            $sampledPrivatePeak = [Math]::Max($sampledPrivatePeak, $process.PrivateMemorySize64)
            if ($clock.Elapsed.TotalSeconds -gt $TimeoutSeconds) {
                $process.Kill($true)
                $process.WaitForExit()
                throw "Timed out after $TimeoutSeconds seconds: $stem"
            }
            if ($sensor -and $clock.ElapsedMilliseconds -ge $nextSensor) {
                $observation = & $sensor '--query-gpu=memory.used' '--format=csv,noheader,nounits' '-i' '0' 2>$null
                $used = 0L
                if ($LASTEXITCODE -eq 0 -and [long]::TryParse(($observation | Select-Object -First 1), [ref]$used)) {
                    $devicePeakMiB = if ($null -eq $devicePeakMiB) { $used } else { [Math]::Max($devicePeakMiB, $used) }
                    $deviceSamples++
                }
                $nextSensor = $clock.ElapsedMilliseconds + 250
            }
        }
        $process.WaitForExit()
        $clock.Stop()
        $raw = $stdout.GetAwaiter().GetResult()
        $errorText = $stderr.GetAwaiter().GetResult()
        $raw | Set-Content -LiteralPath "$stem.stdout.json" -Encoding utf8
        $errorText | Set-Content -LiteralPath "$stem.stderr.txt" -Encoding utf8
        if ($process.ExitCode -ne 0) { throw "Benchmark failed ($($process.ExitCode)): $stem.stderr.txt" }
        $result = $raw | ConvertFrom-Json
        [pscustomobject]@{
            result = $result
            processWallMs = $clock.Elapsed.TotalMilliseconds
            peakWorkingSetBytes = $peakWorkingSet
            sampledPeakPrivateBytes = $sampledPrivatePeak
            memorySampleIntervalMs = 20
            sampledDeviceWideVramPeakMiB = $devicePeakMiB
            deviceVramSamples = $deviceSamples
            vramSensorEnabled = [bool]$sensor
        }
    } finally {
        if ($started -and -not $process.HasExited) { $process.Kill($true); $process.WaitForExit() }
        $process.Dispose()
    }
}

function Compare-Pixels([string]$left, [string]$right, [string]$tolerance, [string]$label) {
    $raw = & $candidate 'compare' $left $right $tolerance
    if ($LASTEXITCODE -ne 0) { throw "Pixel comparison failed: $label" }
    $comparisons.Add([pscustomobject]@{ comparison = $label; diff = ($raw | ConvertFrom-Json) })
}

Get-FileHash -LiteralPath $baseline,$candidate -Algorithm SHA256 |
    Select-Object Path,Hash | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $output 'binaries.json') -Encoding utf8
foreach ($suite in $suites) {
    foreach ($round in 1..$Rounds) {
        # Pixel capture only in round 1. Later timings retain per-frame checksums but avoid
        # repeated disk writes. All captures are bounded and removed after comparison.
        $capture = $round -eq 1
        $variants = if ($suite -eq 'high-samples') { @('candidate') } elseif ($round % 2) { @('baseline','candidate') } else { @('candidate','baseline') }
        $captures = @{}
        if ($capture) {
            $pixels = if ($suite -eq 'tiny') { 31L*19L*13L } elseif ($suite -eq 'playback') { 2560L*1440L*6L } else { 2560L*1440L*4L }
            $needed = $pixels*16L*$Frames*2L*$variants.Count + 1GB
            $drive = [IO.DriveInfo]::new([IO.Path]::GetPathRoot($output))
            if ($drive.AvailableFreeSpace -lt $needed) { throw "Pixel acceptance needs $needed free bytes at $output" }
        }
        foreach ($variant in $variants) {
            foreach ($backend in @('cpu','gpu')) {
                $binary = if ($variant -eq 'baseline') { $baseline } else { $candidate }
                $stem = Join-Path $output "$suite-$variant-$backend-$round"
                $arguments = @('run','--suite',$suite,'--backend',$backend,'--fps',"$Fps",'--frames',"$Frames",'--warmup',"$Warmup")
                if ($capture) {
                    $pixelFile = "$stem.pixels"
                    if (Test-Path -LiteralPath $pixelFile) { throw "Refusing to overwrite existing capture $pixelFile" }
                    $arguments += @('--capture',$pixelFile)
                    $captures["$variant-$backend"] = $pixelFile
                }
                $measurement = Invoke-Measured $binary $arguments $stem
                $records.Add([pscustomobject]@{ suite=$suite; variant=$variant; backend=$backend; round=$round; measurement=$measurement })
                Write-Output "$suite $variant $backend round $round`: peak working set $($measurement.peakWorkingSetBytes) bytes"
                $records | ConvertTo-Json -Depth 14 | Set-Content -LiteralPath (Join-Path $output 'measurements.json') -Encoding utf8
            }
        }
        if ($capture) {
            foreach ($variant in $variants) {
                Compare-Pixels $captures["$variant-cpu"] $captures["$variant-gpu"] '0.001' "$suite $variant CPU/GPU"
            }
            if ($variants.Count -eq 2) {
                Compare-Pixels $captures['baseline-cpu'] $captures['candidate-cpu'] '0' "$suite baseline/candidate CPU exact"
                Compare-Pixels $captures['baseline-gpu'] $captures['candidate-gpu'] '0.001' "$suite baseline/candidate GPU"
            }
            $comparisons | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $output 'pixel-comparisons.json') -Encoding utf8
            if (-not $KeepPixels) {
                foreach ($pixelFile in $captures.Values) { Remove-Item -LiteralPath $pixelFile }
            }
        }
    }
}
Write-Output "Completed; results: $(Join-Path $output 'measurements.json')"
