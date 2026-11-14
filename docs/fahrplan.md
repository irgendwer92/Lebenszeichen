# Lebenszeichen – Repo-Struktur und Meilensteine

Stand: 13.11.2026. Gehört zu `konzept.md` und `protokoll.md`. Die Reihenfolge der Meilensteine ist bestätigt.

## Repo-Struktur

Ein Repository für alles. Kern, Relay und App ändern sich anfangs oft gemeinsam, und so bleibt jede Protokolländerung ein einzelner Commit.

```
lebenszeichen/
├── crates/
│   ├── wire/        Nachrichtenhülle und Relay-Schnittstelle (von Kern und Relay genutzt)
│   ├── core/        Protokoll, Verschlüsselung, Regelprüfung, Speicher
│   ├── cli/         Client ohne Oberfläche, auch als Dienst nutzbar
│   └── relay/       Relay-Server
├── app/
│   ├── src-tauri/   Tauri-Hülle: Tray, Fenster, Brücke zum Kern
│   ├── ui/          Oberfläche (Web)
│   └── android/     Kotlin-Schicht: UnifiedPush, Vollbild-Anzeige, Benachrichtigungen
├── deploy/          docker-compose für Relay und ntfy, Beispiel für den Reverse-Proxy
├── docs/            Konzept, Protokoll, Anleitung für Selbsthoster
├── examples/        Beispielskripte für die lokale Schnittstelle
├── .github/workflows/
├── renovate.json
├── LICENSE          GPL-3.0-or-later mit App-Store-Ausnahme
└── crates/relay/LICENSE   AGPL-3.0-or-later
```

Anmerkungen:

- **`cli` ist kein Nebenprodukt.** Damit lässt sich das Protokoll testen, bevor es eine Oberfläche gibt. Später ist es der Client für Geräte ohne Bildschirm, etwa ein Server, an dem ein Skript den Zustand "schläft" meldet.
- **Bezeichner im Code auf Englisch**, Dokumentation auf Deutsch. Die Nachrichtentypen aus `protokoll.md` heißen im Code `request`, `response`, `rules`, `hello`, `hello_ok`, `end`. Das erleichtert eine spätere Veröffentlichung.
- **Lizenzen:** `wire`, `core`, `cli` und `app` stehen unter GPL-3.0-or-later mit Ausnahme, `relay` unter AGPL-3.0-or-later. Beide Lizenzen erlauben ausdrücklich, miteinander kombiniert zu werden.

## CI und Abhängigkeiten

| Ablauf | Auslöser | Inhalt |
|---|---|---|
| Prüfen | jeder Push und Pull Request | Formatierung, Clippy, Tests, Lizenz- und Sicherheitsprüfung der Abhängigkeiten (`cargo-deny`) |
| Bauen | jeder Push auf `main` | Linux-Paket, Windows-Installer, Android-APK, Relay-Image; als Artefakte abrufbar |
| Veröffentlichen | Versions-Tag | Signierte APK, Installer und Relay-Image als Release |
| Renovate | wöchentlich | Sammel-Pull-Request für kleine Updates; Sicherheitsupdates sofort und einzeln |

Der Signaturschlüssel für Android liegt als Secret im Repository und wird extern gesichert. Geht er verloren, lassen sich installierte Apps nicht mehr aktualisieren.

## Meilensteine

Jeder Meilenstein endet mit etwas, das sich ausprobieren lässt.

### M0 – Grundgerüst (erledigt am 13.11.2026)
- Repository, Workspace, Lizenzdateien, CI-Ablauf "Prüfen", Renovate-Konfiguration.
- **Fertig, wenn:** ein Commit grün durch die CI läuft. Erfüllt.
- Noch offen aus M0: Renovate-App installieren, Wortlaut der App-Store-Ausnahme juristisch gegenlesen (Entwurf in `lizenz-ausnahme.md`).

### M1 – Durchstich auf Android (vorläufig abgeschlossen am 13.11.2026)
Wegwerf-Prototyp, der nur das Risiko klärt. Ergebnis und Einzelheiten stehen in `m1-durchstich.md`: Alle zehn Durchläufe sind auf einem Fairphone 4 bestanden, die Entscheidung für Tauri bleibt. Offen ist ein Lauf über eine ganze Nacht; getestet wurde mit einer Stunde Ruhezeit.
- Wecksignal über UnifiedPush kommt an, während die App beendet ist.
- Die Kotlin-Schicht ruft eine Funktion im Rust-Kern auf.
- Die Anfrage erscheint im Vordergrund; Antippen und Wegklicken lösen je einen Aufruf an einen Testserver aus.
- **Fertig, wenn:** das auf einem echten Gerät zehnmal hintereinander klappt, auch nach einem Neustart und über Nacht.
- **Abbruchkriterium:** Geht das mit Tauri nicht zuverlässig, wird hier auf Flutter gewechselt. Kern und Relay bleiben davon unberührt.

### M2 – Kern und Relay (begonnen am 14.11.2026)

Stand:

- Erledigt: Drahtformat in `crates/wire` ohne Verschlüsselung (Klartext aller sechs Nachrichtentypen, Einladung als `LZ1-`-Code, Prüfung des Umschlags).
- Erledigt: Verschlüsselung und Kopplung in `crates/core` (beide Umschlagformen byteweise gegen libsodium geprüft, Prüfcode, Ablauf von Einladung bis `hello_ok`, Zeitfenster gegen Wiedereinspielen).
- Erledigt: Regeln für Anfragen in `crates/core` (offene Anfrage, Mindestabstand, gleitendes Wochenlimit, auf beiden Seiten mit derselben Prüfung), Antworten, gemeldete Antworten, Entfernen, Erkennen wiederholter Nachrichten.
- Erledigt: Speicher in `crates/core` als eine JSON-Datei je Gerät, mit Sperre und atomarem Ersetzen.
- Erledigt: Relay `lz-relay` (axum, SQLite) mit allen Aufrufen aus `drahtformat.md`, Zustellung per WebSocket mit Quittungen, Wecksignal mit Schutz vor privaten Zieladressen, Aufräumregeln und Docker-Image (`crates/relay/Dockerfile`); Integrationstests laufen ohne Netz.
- Erledigt: Kommandozeile `lz` in `crates/cli` (Einladen mit und ohne Gutschein, Annehmen, Bestätigen, Abrufen per WebSocket mit Quittung nach dem Speichern, Anfragen, Antworten, Melden, Regeln, Entfernen samt Erkennen über `unknown_mailbox`), getestet gegen ein Relay im Speicher.

Umfang:

- Nachrichtenhülle, Verschlüsselung, Kopplung, Anfrage und Antwort, Regeln samt Wochenlimit, Entfernen.
- Relay mit Postfächern, Lesern, Gutscheinen, Aufräumregeln; als Docker-Image.
- Mehrere Leser pro Postfach sind von Anfang an vorgesehen.
- Vorgaben, damit später Microcontroller-Clients möglich bleiben (siehe unten): sprachneutrales, dokumentiertes Drahtformat; Protokollteil des Kerns ohne Abhängigkeit vom Betriebssystem.
- **Fertig, wenn:** zwei `cli`-Instanzen sich über ein Relay koppeln, anfragen und antworten und die Regeln greifen.

### M3 – Linux-Desktop
- Oberfläche: Kontaktliste, Anfrage-Knopf, Einladung als QR- und Textcode, Einstellungen pro Kontakt.
- Tray, dauerhafte Verbindung, Vordergrund-Fenster ohne Tastaturfokus, Benachrichtigung als Rückfall.
- Offen aus einer Dependabot-Meldung vom 14.11.2026: Tauri 2 braucht auf Linux die GTK-3-Bindungen und damit `glib` 0.18; behoben ist der Fehler in `VariantStrIter` erst ab 0.20, das es nur für GTK 4 gibt. Die betroffene Funktion ruft weder Tauri noch GTK, WebKit, `wry` oder `tao` auf, und der Android-Build enthält `glib` gar nicht. Mit dem Linux-Build erneut prüfen, ob Tauri inzwischen auf neuere Bindungen umgestiegen ist.
- **Fertig, wenn:** Desktop-App und `cli` sich gegenseitig anfragen können.

### M4 – Android-App
- Der Prototyp aus M1 wird durch die richtige Anbindung an den Kern ersetzt.
- QR-Code scannen, Vollbild-Anzeige, Benachrichtigung als Rückfall.
- Signierte APK aus der CI.
- **Fertig, wenn:** Handy und Desktop zweier Personen sich gegenseitig anfragen können. Ab hier ist die App im Alltag nutzbar, mit einem Gerät pro Person.

### M5 – Mehrere Geräte
- Zweites Gerät per QR-Code einrichten, Abgleich von Kontakten, Zählern und erledigten Anfragen.
- Gerät austragen.
- **Fertig, wenn:** eine Antwort am Handy die Anzeige am Rechner zurückzieht und das Wochenlimit über beide Geräte zählt.

### M6 – Windows
- Installer, Tray, Vordergrund-Fenster, Autostart.
- **Fertig, wenn:** M3 und M5 auf Windows gleich funktionieren.

### M7 – Lokale Schnittstelle
- `setzen` und `löschen` über Unix-Socket, Named Pipe und Android-Intent.
- Gemeldete Antwort nach Ablauf der Wartezeit, Freigabe pro Kontakt.
- Beispielskript.
- **Fertig, wenn:** ein Skript "schläft" meldet und der Anfragende nach der Wartezeit "schläft (gemeldet)" sieht.

### M8 – Härtung
- Durchsicht von Verschlüsselung und Relay durch eine zweite Person.
- Anleitung für Selbsthoster, inklusive abgeschalteter Protokolle im Reverse-Proxy.
  - Hinweis darin: Das Relay schickt Wecksignale nur an öffentliche Adressen. ntfy muss ohnehin aus dem Internet erreichbar sein, sonst bekommt das Handy unterwegs keine Wecksignale. Laufen Relay und ntfy auf demselben Heimserver und kann der Router Verbindungen an die eigene öffentliche Adresse nicht zurückleiten (NAT-Loopback), hilft kein lokaler DNS-Eintrag auf die private Adresse, weil das Relay ihn ablehnt. Die Lösung liegt im eigenen Netz, etwa NAT-Loopback im Router einschalten.
- Verhalten bei Relay-Ausfall, Uhrzeitabweichung und verlorenem Gerät testen.
- **Fertig, wenn:** jemand ohne Vorwissen mit der Anleitung ein Relay aufsetzen und zwei Geräte koppeln kann.

### Später
iOS und macOS, Veröffentlichung in Stores, fremde Relays, Microcontroller-Clients.

## Microcontroller-Clients (später)

Gewünscht, aber ohne Termin und ohne Entscheidung zwischen den Wegen. Gedacht ist etwa an einen Knopf mit Leuchte, der eine offene Anfrage anzeigt und per Druck beantwortet. Beide Wege haben ihren Platz: Weg A für zu Hause, Weg B für unterwegs, etwa bei vergessenem Smartphone.

**Weg A – Satellit eines vollwertigen Clients.** Der Microcontroller spricht im Heimnetz mit einem Client, der schon läuft (etwa `cli` auf einem Server), über ein Brückenskript oder eine Hausautomation. Schlüssel bleiben auf dem Client.
- Voraussetzung: Die lokale Schnittstelle bekommt zwei weitere Aufrufe, "offene Anfrage vorhanden?" und "beantworten". Noch nicht entschieden.
- Aufwand gering, möglich ab M7.
- Funktioniert nur, solange der Client zu Hause erreichbar ist.

**Weg B – eigenständiger Client.** Der Microcontroller spricht das Protokoll selbst mit dem Relay.
- Machbar auf Geräten der ESP32-Klasse: WLAN, TLS, WebSocket und libsodium sind dort verfügbar, die Nachrichten sind 256 Byte groß.
- Kein Wecksignal nötig; das Gerät hält die Verbindung selbst.
- Braucht eine Uhrzeit aus dem Netz (NTP), weil Nachrichten Zeitstempel tragen.
- Der Microcontroller wäre ein weiteres Gerät der Person und hielte nach dem jetzigen Entwurf alle Kontaktschlüssel. Ohne verschlüsselten Speicher ist das ein Risiko bei Diebstahl. Abhilfe wäre eine eingeschränkte Geräterolle; die gibt es im Protokoll noch nicht.

Was jetzt schon dafür getan wird, ohne Mehraufwand:
- Drahtformat sprachneutral und vollständig dokumentiert, nicht an eine Rust-Bibliothek gebunden.
- Relay-Schnittstelle bleibt schlichtes HTTPS und WebSocket.
- Protokollteil des Kerns von Speicher und Betriebssystem getrennt.

## Reihenfolge und Abhängigkeiten

- M1 kommt bewusst vor M2, obwohl M2 die Grundlage ist: M1 ist die einzige Stelle, an der die Framework-Entscheidung noch kippen kann.
- M2 und M1 lassen sich parallel bearbeiten, weil M1 nur eine Platzhalter-Funktion im Kern braucht.
- M5 steht nach M4, das Relay und der Kern kennen mehrere Geräte aber schon ab M2. So muss später nichts am Protokoll geändert werden.
- M6 und M7 sind unabhängig voneinander und können getauscht werden.

## Offene Punkte

- Wortlaut der App-Store-Ausnahme.
- Technik der Oberfläche in `app/ui` (schlichtes HTML oder ein kleines Framework).
- Schutz der Schlüssel auf dem Gerät. Das Speicherformat ist entschieden, siehe `konzept.md`.
- Lokale Schnittstelle um "offene Anfrage vorhanden?" und "beantworten" erweitern (für Microcontroller-Satelliten).
- Eingeschränkte Geräterolle für Geräte ohne sicheren Speicher.
