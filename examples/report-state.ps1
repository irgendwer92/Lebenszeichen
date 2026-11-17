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

# Owner of the pipe, read from its handle. A handle opened for reading
# includes READ_CONTROL, which is all GetSecurityInfo needs.
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Security.Principal;
using Microsoft.Win32.SafeHandles;

public static class PipeOwner {
    [DllImport("advapi32.dll")]
    static extern uint GetSecurityInfo(SafePipeHandle handle, int objectType, int info,
        out IntPtr owner, IntPtr group, IntPtr dacl, IntPtr sacl, out IntPtr descriptor);
    [DllImport("kernel32.dll")]
    static extern IntPtr LocalFree(IntPtr memory);

    public static string Of(SafePipeHandle handle) {
        IntPtr owner, descriptor;
        // 6: SE_KERNEL_OBJECT, 1: OWNER_SECURITY_INFORMATION
        uint error = GetSecurityInfo(handle, 6, 1, out owner, IntPtr.Zero, IntPtr.Zero,
            IntPtr.Zero, out descriptor);
        if (error != 0) throw new System.ComponentModel.Win32Exception((int)error);
        try { return new SecurityIdentifier(owner).Value; }
        finally { LocalFree(descriptor); }
    }
}
'@

$pipe = New-Object System.IO.Pipes.NamedPipeClientStream('.', $pipeName, [System.IO.Pipes.PipeDirection]::InOut)
try {
    $pipe.Connect(2000)  # milliseconds; fails if the app does not run
    # Pipe names are global on the machine and the SID is no secret, so
    # another user could have taken the name first. The app sets itself
    # (the user) as owner of its pipe; anything else is not the app.
    $owner = [PipeOwner]::Of($pipe.SafePipeHandle)
    if ($owner -ne $sid) {
        throw "the pipe $pipeName belongs to $owner, not to you; not sending anything"
    }
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
