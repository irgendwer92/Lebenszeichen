# Windows release

The workflow "Windows release" (`.github/workflows/windows-release.yml`) builds
the NSIS installer of the desktop app on `windows-latest`. It runs on a tag
`v*` and by hand:

```
gh workflow run windows-release.yml --ref main
```

The installer is **not signed** (decision 3 in [m6-windows.md](m6-windows.md);
signing is an open point for M8).

## Getting and checking the installer

- The finished `Lebenszeichen_<version>_x64-setup.exe` is the artifact
  `lebenszeichen-windows-setup` of the run (30 days).
- The SHA-256 of the file is in the job summary of the run. Compare it with
  the downloaded file in PowerShell:
  `Get-FileHash .\Lebenszeichen_0.1.0_x64-setup.exe -Algorithm SHA256`

## Installing

- Because the installer is unsigned, Windows SmartScreen warns on first start
  ("Windows protected your PC"). Choose "More info", then "Run anyway", only
  after the checksum matched.
- The installation is per user and needs no administrator rights. The app goes
  to `%LOCALAPPDATA%\Lebenszeichen`; the Start menu gets a shortcut. The
  installer is in English or German, following the system language.
- WebView2 is loaded by the installer if it is missing (already part of
  Windows 10 and 11).

## Uninstalling

"Settings > Apps > Installed apps > Lebenszeichen > Uninstall", or the
uninstaller in the install folder. It also removes the autostart entry
(`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`). The state of the
app with the device keys
(`%APPDATA%\io.github.irgendwer92.lebenszeichen\device.json`) stays, unless
"Delete the application data" is ticked in the uninstaller; remove the device
in another device's list first if it is to go for good.
