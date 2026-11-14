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

### M1 – Durchstich auf Android (abgeschlossen am 14.11.2026)
Wegwerf-Prototyp, der nur das Risiko klärt. Ergebnis und Einzelheiten stehen in `m1-durchstich.md`: Alle zehn Durchläufe sind auf einem Fairphone 4 bestanden, die Entscheidung für Tauri bleibt. Der Nachtlauf (Zeilen 9 und 10 des Testprotokolls) ist in der Nacht vom 13. auf den 14.11.2026 nach Protokoll nachgeholt und bestanden.
- Wecksignal über UnifiedPush kommt an, während die App beendet ist.
- Die Kotlin-Schicht ruft eine Funktion im Rust-Kern auf.
- Die Anfrage erscheint im Vordergrund; Antippen und Wegklicken lösen je einen Aufruf an einen Testserver aus.
- **Fertig, wenn:** das auf einem echten Gerät zehnmal hintereinander klappt, auch nach einem Neustart und über Nacht.
- **Abbruchkriterium:** Geht das mit Tauri nicht zuverlässig, wird hier auf Flutter gewechselt. Kern und Relay bleiben davon unberührt.

### M2 – Kern und Relay (abgeschlossen am 14.11.2026)

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

### M3 – Linux-Desktop (begonnen am 14.11.2026)

Stand:

- Erledigt: gemeinsame Client-Schicht `crates/client` (`lz-client`): Relay-Zugang, alle Befehle, Abruf mit `sync` und `listen`, deutsche Texte und eine strukturierte Übersicht der Kontakte. `lz` ist nur noch die Kommandozeile darüber; die Desktop-App nutzt dieselbe Schicht.
- Erledigt: Desktop-Hülle für Linux in `app/` (`ui/desktop.html`, `src-tauri/src/desktop.rs`): Gerät einrichten, Kontaktliste mit Anfrage-Knopf und „wieder möglich ab“, Einladen (Code als Text), Annehmen, Prüfcode bestätigen oder ablehnen, Antworten, Entfernen; Abruf im Hintergrund über `lz-client`. Die Android-Teile aus M1 sind unverändert. Am 14.11.2026 gegen ein lokales Relay und zwei `lz`-Geräte durchgeklickt; die dabei gefundenen Fehler sind behoben.
- Erledigt: Gemeinsame Texte in `lz-client` nennen keine Befehle mehr. Wo ein nächster Schritt passt, steht ein `Hint`; `lz` macht daraus seinen Befehl, die Desktop-App zeigt nichts, weil sie Knöpfe hat.
- Erledigt: Die CI baut die Desktop-App (Formatierung, Clippy, Tests) und prüft Lizenzen und Quellen ihres eigenen Lockfiles mit derselben `deny.toml`. Entschieden: Die Desktop-App ist ein eigenes Gerät mit eigenem Zustand im Datenverzeichnis der App, nicht dieselbe Datei wie `lz`.
- Erledigt: Tray mit „Fenster zeigen“, „Jetzt abrufen“ und „Beenden“. Schließen versteckt das Fenster nur, der Abruf läuft weiter. Ein zweiter Start holt das Fenster der laufenden App zurück, statt ein zweites Mal abzurufen; so bleibt die App auch ohne Tray erreichbar. Unter Linux braucht der Tray zur Laufzeit `libayatana-appindicator` oder `libappindicator` (für das Linux-Paket in M8 vormerken).
- Erledigt: Benachrichtigung unter Linux über D-Bus (`src-tauri/src/desktop/notify.rs`): Dringlichkeit „kritisch“, ohne Ton, bleibt bis zur Antwort, mit Knopf „Lebenszeichen senden“. Knopf, Klick und Schließen von Hand senden das Lebenszeichen; Ablaufen und Zurückziehen durch die App nicht. Ist die Anfrage nicht mehr offen, etwa weil im Fenster geantwortet wurde, zieht die App die Benachrichtigung zurück. Schlägt das Senden fehl, kommt sie wieder. Unter KDE Plasma (Wayland) gegen ein lokales Relay geprüft.
- Erledigt: Strenge Content Security Policy für die App (nur eigene Dateien und Tauris IPC, kein eingebetteter Code) statt `"csp": null` aus M1. Die Desktop-Oberfläche läuft damit; der Android-Prototyp nutzt dieselbe Konfiguration und wird in M4 geprüft.
- Erledigt: Einladung als QR-Code (`crates/client/src/qr.rs`, Fehlerkorrektur M, alphanumerischer Modus; Version 8 bis 12). Die Desktop-App zeigt ihn schwarz auf weiß über dem Textcode, als SVG aus dem DOM, sodass die CSP unverändert bleibt; `lz invite --qr` zeigt ihn im Terminal. Mit einer QR-App auf dem Handy gescannt, der Text stimmt. Scannen in der App kommt mit M4.
- Erledigt: Einstellungen pro Kontakt in der Desktop-App: wie oft der Kontakt fragen darf (Mindestabstand und Ablauf in halben Stunden bis zu einer Woche, Wochenlimit 0 bis 999), dazu lesend, wie oft man selbst fragen darf. Gesendet werden nur geänderte Werte. Entschieden: Der Schalter für gemeldete Antworten kommt mit M7, zusammen mit der Automatik, die ihn nutzt.
- Teil des Fertig-Kriteriums erfüllt am 14.11.2026: Desktop-App und `lz` haben sich gegenseitig angefragt und geantwortet, gegen ein lokales Relay mit frisch gekoppelten Geräten. Die App fragt über den Knopf und erhält die Antwort im Hintergrund; `lz` fragt, die App zeigt die Anfrage als Benachrichtigung und im Fenster, die Antwort kommt bei `lz` an. Die Sperren danach folgen den Regeln beider Seiten.
- Erledigt: Desktop-Oberfläche überarbeitet, mit eigenem Stylesheet (`style.css` bleibt beim Android-Prototyp): Kontaktliste als Hauptansicht, offene Anfragen hervorgehoben oben, Prüfcodes groß, Einstellungen und „Verbindung beenden“ hinter ⚙, Einladen und Annehmen als Felder aus der Kopfzeile, Verlauf eingeklappt. Hell und dunkel nach dem System, Akzent Grün. Damit ist der Teil „Oberfläche überarbeitet“ des Fertig-Kriteriums erfüllt.
- Erledigt: Vordergrund-Fenster unter X11 (`src-tauri/src/desktop/alert.rs`, `ui/alert.html`): statt der Benachrichtigung ein kleines Fenster über allen anderen, ohne Tastaturfokus, nicht in der Taskleiste, eins je Anfrage und leicht versetzt. Klick ins Fenster oder Schließen sendet das Lebenszeichen, Tasten nie. Ist die Anfrage anders beantwortet, schließt die App das Fenster ohne Antwort; lässt es sich nicht öffnen, kommt die Benachrichtigung. Das Fenster darf nur Text und Antwort zur eigenen Anfrage abrufen (eigene Capability). Unter XWayland auf KDE Plasma geprüft (`GDK_BACKEND=x11`): über allen Fenstern, kein Fokus, Tippen in einem anderen Programm sendet nichts, Klick, Schließen, Zurückziehen und zwei Fenster gleichzeitig. „Auf allen Arbeitsflächen“ ließ sich mit nur einer Arbeitsfläche nicht prüfen.
- Noch offen in M3: Benachrichtigung unter GNOME prüfen (bisher nur KDE Plasma), Vordergrund-Fenster in einer echten X11-Sitzung prüfen, auch mit mehreren Arbeitsflächen.

Umfang:

- Oberfläche: Kontaktliste, Anfrage-Knopf, Einladung als QR- und Textcode, Einstellungen pro Kontakt.
- Tray, dauerhafte Verbindung, Vordergrund-Fenster ohne Tastaturfokus, Benachrichtigung als Rückfall.
- Offen aus einer Dependabot-Meldung vom 14.11.2026: Tauri 2 braucht auf Linux die GTK-3-Bindungen und damit `glib` 0.18; behoben ist der Fehler in `VariantStrIter` erst ab 0.20, das es nur für GTK 4 gibt. Die betroffene Funktion ruft weder Tauri noch GTK, WebKit, `wry` oder `tao` auf, und der Android-Build enthält `glib` gar nicht. Mit dem Linux-Build erneut prüfen, ob Tauri inzwischen auf neuere Bindungen umgestiegen ist.
- **Fertig, wenn:** Desktop-App und `cli` sich gegenseitig anfragen können,
  die Anfrage unter X11 als Vordergrund-Fenster ohne Tastaturfokus erscheint,
  die Benachrichtigung unter KDE Plasma und GNOME geprüft ist und die
  Desktop-Oberfläche überarbeitet ist.

### M4 – Android-App
- Der Prototyp aus M1 wird durch die richtige Anbindung an den Kern ersetzt.
- QR-Code scannen, Vollbild-Anzeige, Benachrichtigung als Rückfall.
- Signierte APK aus der CI.
- Erledigt: Der Ablauf „Android-Prototyp“ läuft wieder automatisch bei Pull Requests auf `app/**`, `crates/**` und `Cargo.lock`. Der erste Lauf am 14.11.2026 mit den Abhängigkeiten aus M2 ging durch. Er enthält bisher nur `lz-core`; `lz-client` kommt mit der gemeinsamen Anbindung in den Android-Build.
- Erledigt: Push-Adresse beim Relay. Das Gerät speichert sie (`push_url`) und trägt sie bei jedem eigenen Postfach ein, auch bei neuen aus Einladen und Annehmen. Schlägt das fehl, bleibt sie vorgemerkt (`push_pending`), und der nächste Abruf trägt sie nach. Eine unveränderte Adresse kostet keinen Aufruf, weil UnifiedPush sie bei jedem Start erneut meldet. `lz push` setzt und entfernt sie von Hand.
- Erledigt: TLS-Zertifikate prüft das Betriebssystem (`rustls-platform-verifier` 0.6, weil 0.7 `jni` 0.22 bräuchte und Tauri noch 0.21 nutzt), für HTTP und WebSocket gleich. Unter Linux ändert sich damit nichts, unter Android nutzt der Client die Prüfung des Systems mit selbst installierten Zertifikaten. Dort braucht sie einen einmaligen Aufruf mit dem App-Kontext und die Kotlin-Bibliothek des Crates; beides kommt mit dem Abruf beim Wecksignal. Fehlermeldungen nennen jetzt den Grund, etwa ein unbekanntes Zertifikat.
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
- Schutz der Schlüssel auf dem Gerät. Das Speicherformat ist entschieden, siehe `konzept.md`.
- Lokale Schnittstelle um "offene Anfrage vorhanden?" und "beantworten" erweitern (für Microcontroller-Satelliten).
- Eingeschränkte Geräterolle für Geräte ohne sicheren Speicher.
