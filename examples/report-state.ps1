# Reports a state to the Lebenszeichen desktop app on Windows through its
# local interface (docs/m8-local-interface.md), a named pipe that speaks
# minimal HTTP and that only the current user can open.
#
#   .\report-state.ps1 set 480 asleep   # "asleep" for eight hours
#   .\report-state.ps1 clear            # clear the state
#   .\report-state.ps1 show             # print the current state (404: none)

param(
    [Parameter(Mandatory, Position = 0)][ValidateSet('set', 'clear', 'show')][string]$Action,
    [Parameter(Position = 1)][int]$Minutes,
    [Parameter(Position = 2, ValueFromRemainingArguments)][string[]]$Reason
)

# The pipe name carries the SID of the user: lebenszeichen-<SID>-app.
$sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$pipeName = "lebenszeichen-$sid-app"

switch ($Action) {
    'set' {
        if (-not $Minutes -or -not $Reason) { throw 'usage: set <minutes> <reason>' }
        $method = 'POST'
        $body = @{ reason = ($Reason -join ' '); minutes = $Minutes } | ConvertTo-Json -Compress
    }
    'clear' { $method = 'DELETE'; $body = '' }
    'show' { $method = 'GET'; $body = '' }
}

$pipe = New-Object System.IO.Pipes.NamedPipeClientStream('.', $pipeName, [System.IO.Pipes.PipeDirection]::InOut)
try {
    $pipe.Connect(2000)  # milliseconds; fails if the app does not run
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($body)
    $head = "$method /state HTTP/1.1`r`nHost: localhost`r`nConnection: close`r`n" +
            "Content-Type: application/json`r`nContent-Length: $($bytes.Length)`r`n`r`n"
    $headBytes = [System.Text.Encoding]::ASCII.GetBytes($head)
    $pipe.Write($headBytes, 0, $headBytes.Length)
    $pipe.Write($bytes, 0, $bytes.Length)
    $pipe.Flush()
    # The app closes the pipe after its answer, so read to the end.
    $answer = (New-Object System.IO.StreamReader($pipe, [System.Text.Encoding]::UTF8)).ReadToEnd()
} finally {
    $pipe.Dispose()
}

# "HTTP/1.1 204 No Content\r\n...\r\n\r\n<body>"
$parts = $answer -split "`r`n`r`n", 2
$status = ($parts[0] -split ' ')[1]
Write-Output "status: $status"
if ($parts.Length -gt 1 -and $parts[1]) { Write-Output $parts[1] }
