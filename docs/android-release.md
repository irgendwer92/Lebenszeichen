# Lebenszeichen – Signierte Android-App

Stand: 16.11.2026. Gehört zu `fahrplan.md` (M4).

Die signierte APK baut der Ablauf `.github/workflows/android-release.yml`, bei einem Tag `v*` oder von Hand. Pull Requests bauen weiter nur die Debug-APK. Den Schlüssel dazu legst du einmal selbst an; er verlässt deinen Rechner nur als Secret bei GitHub.

## 1. Schlüssel anlegen

```sh
keytool -genkeypair -v \
  -keystore ~/lebenszeichen-release.jks \
  -alias lebenszeichen \
  -keyalg RSA -keysize 4096 \
  -validity 10000 \
  -dname "CN=Lebenszeichen"
```

- `keytool` fragt nach einem Passwort. Ein langes, zufälliges nehmen, etwa aus dem Passwortmanager. Fragt es nach einem eigenen Passwort für den Schlüssel, Enter drücken: dann gilt dasselbe. Der Ablauf erwartet genau das.
- 10 000 Tage sind etwa 27 Jahre. Eine Android-App kann ihren Schlüssel **nie** wechseln. Geht er verloren, gibt es keine Updates mehr; alle müssen deinstallieren und neu koppeln.
- `-dname` enthält bewusst nichts Persönliches: Es steht lesbar in jeder APK.

## 2. Sichern

Die Datei `lebenszeichen-release.jks` und das Passwort an zwei getrennten Orten aufbewahren, etwa im Passwortmanager und in einer verschlüsselten Sicherung. Ohne beides zusammen ist der Schlüssel wertlos.

Den Fingerabdruck notieren, um ihn später mit dem CI-Lauf zu vergleichen:

```sh
keytool -list -v -keystore ~/lebenszeichen-release.jks -alias lebenszeichen | grep SHA256
```

## 3. Umgebung „release“ bei GitHub

Die Umgebung hält den Schlüssel und kann jeden Lauf an eine Freigabe binden. Was GitHub davon anbietet, hängt bei privaten Repositories vom Tarif ab:

| Tarif | Umgebung mit eigenen Secrets | Freigabe durch dich vor jedem Lauf |
|---|---|---|
| Free | nein | nein |
| Pro, Team | ja | nein |
| Enterprise, oder öffentliches Repository | ja | ja |

**Mit Umgebung** (Pro und höher): Repository → Settings → Environments → New environment, Name `release`.

- „Deployment branches and tags“: „Selected branches and tags“, dann `main` und das Tag-Muster `v*` hinzufügen. Dann kann kein anderer Zweig den Schlüssel benutzen.
- Falls angeboten: „Required reviewers“ mit dir selbst. Dann wartet jeder signierte Build auf deine Freigabe unter Actions.

**Ohne Umgebung** (Free): Die Secrets kommen ins Repository statt in die Umgebung (Befehle unten ohne `--env release`). Der Ablauf funktioniert genauso, denn er liest die Secrets aus der Umgebung und, wenn es sie dort nicht gibt, aus dem Repository. Schutz bieten dann nur die Auslöser: Tag oder Start von Hand, nie ein Pull Request.

## 4. Secrets hinterlegen

```sh
base64 -w0 ~/lebenszeichen-release.jks | gh secret set ANDROID_KEYSTORE_BASE64 --env release -R irgendwer92/Lebenszeichen
gh secret set ANDROID_KEYSTORE_PASSWORD --env release -R irgendwer92/Lebenszeichen
gh variable set ANDROID_KEY_ALIAS --env release -R irgendwer92/Lebenszeichen -b lebenszeichen
```

Der zweite Befehl fragt nach dem Passwort; so landet es nicht in der Shell-Historie. Der Alias ist kein Geheimnis und liegt deshalb als Variable vor: Als Secret ersetzte GitHub jedes „lebenszeichen“ in den Protokollen durch `***`. Prüfen:

```sh
gh secret list --env release -R irgendwer92/Lebenszeichen
gh variable list --env release -R irgendwer92/Lebenszeichen
```

Es sollten die beiden Secrets und die Variable erscheinen. Die Werte der Secrets zeigt GitHub nie wieder an.

## 5. Bauen

- **Zum Testen:** `gh workflow run android-release.yml --ref main` (vor dem Zusammenführen von M4: `--ref m4-android`). Die APK liegt als Artefakt am Lauf, `versionCode` ist die Laufnummer.
- **Als Version:** `version` in `app/src-tauri/tauri.conf.json` erhöhen, zusammenführen, dann `git tag v0.1.0 && git push origin v0.1.0`. Der Ablauf prüft, dass Tag und Version übereinstimmen, und legt einen **Entwurf** eines GitHub-Releases mit APK und Prüfsumme an. Veröffentlichen tust du ihn selbst. `versionCode` folgt der Formel von Tauri: 0.1.0 → 1000, 0.1.1 → 1001, 0.2.0 → 2000. Android installiert ein Update nur mit höherem `versionCode`.

## 6. Prüfen und installieren

- Im Lauf unter „APK prüfen“ steht „Zertifikat“ mit dem SHA-256-Fingerabdruck. Er muss mit dem aus Schritt 2 übereinstimmen.
- Die Release-App und die Debug-App aus den Pull Requests haben denselben Paketnamen, aber verschiedene Schlüssel. Android installiert die eine nicht über die andere: Vor dem Wechsel die Debug-App deinstallieren. Dabei gehen Gerät und Kontakte auf dem Handy verloren; danach neu koppeln.

## Fingerabdruck

Zertifikat der Release-APK, SHA-256 (seit 15.11.2026):

```
61:EE:59:A2:68:54:C5:F2:CD:A0:11:2D:3E:02:D7:6D:F0:6E:06:5E:6A:2C:C0:52:0D:5C:C3:A3:44:3C:F2:AF
```

Eine APK mit einem anderen Fingerabdruck stammt nicht aus diesem Projekt.
