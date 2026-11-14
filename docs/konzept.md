# Lebenszeichen – Konzept

Stand: 13.11.2026. Technische Details zu Kopplung, Nachrichten und Relay stehen in `protokoll.md`, Repo-Struktur und Meilensteine in `fahrplan.md`.

## Idee

Eine Person fragt bei einer anderen ein minimales Lebenszeichen an. Die Antwort kostet praktisch keinen Aufwand und verrät nichts außer "am Gerät hat jemand reagiert". Kein Chat, kein Onlinestatus, kein Zeitplan, keine Alarmkette.

## Entscheidungen

| Thema | Entscheidung |
|---|---|
| Antwortmöglichkeiten | Genau eine. Es gibt nichts auszuwählen. |
| Was zählt als Antwort | Jede Reaktion auf die Anzeige: antippen, anklicken, öffnen oder wegklicken. |
| Anzeige der Anfrage | Standard: schiebt sich wie ein Wecker in den Vordergrund. Rückfall: Benachrichtigung. Pro Gerät einstellbar. |
| Ton und Vibration | Standardmäßig aus. Der Angefragte kann sie auf seinem Gerät einschalten. |
| Automatische Antwort aus Gerätenutzung | Nein. Sagt zu wenig aus. |
| Zustandsmeldung von außen | Ja, über eine kleine lokale Schnittstelle (Grund + Zeitspanne). Greift erst nach Ablauf der Wartezeit für eine normale Antwort. |
| Wartezeit vor gemeldeter Antwort | Einstellbar, Vorgabe 15 Minuten. |
| Ladbare Plugins in der App | Nein. Die lokale Schnittstelle reicht. |
| Regeln für Anfragen | Pro Kontakt: höchstens eine offene Anfrage, Ablauf nach 24 Stunden, 6 Stunden Mindestabstand nach einer Antwort, höchstens 7 Anfragen pro Woche. Ablauf, Abstand und Wochenlimit stellt der Angefragte ein. |
| Mehrere Geräte | Von Anfang an: Handy und Rechner teilen sich Schlüssel und Postfächer. |
| Kontakt entfernen | Für die Gegenseite sichtbar. |
| Forward Secrecy | Bewusster Verzicht zugunsten eines kleinen, mehrgerätefähigen Protokolls. |
| Speicher des Clients | Eine JSON-Datei je Gerät hinter einer Schnittstelle im Kern, bei jeder Änderung als Ganzes ersetzt. Genau ein Prozess besitzt den Zustand eines Geräts. Schlüssel vorerst unverschlüsselt, nur für den Besitzer lesbar; Schutz über den Schlüsselspeicher des Systems folgt mit den Apps. Entschieden am 14.11.2026. |
| Framework | Tauri, mit eigenständigem Rust-Kern und dünner Kotlin-Schicht auf Android. |
| Lizenz | Clients und Kern: GPL-3.0-or-later mit App-Store-Ausnahme. Relay: AGPL-3.0-or-later. |
| Quellcode-Hosting | GitHub, mit GitHub Actions und Renovate. |
| Reihenfolge der Meilensteine | Wie in `fahrplan.md`, bestätigt. |
| Betrieb | Zunächst selbst gehostet, Apps per Sideload. |
| Veröffentlichung | Später eventuell; die Architektur soll das nicht verbauen. |
| Plattformen | Android, Windows, Linux zuerst; iOS und macOS eventuell später. |

## Ablauf

1. A tippt bei Kontakt B auf "Lebenszeichen?".
2. Auf Bs Geräten erscheint die Anfrage im Vordergrund, wo das System es zulässt, sonst als Benachrichtigung. Standardmäßig ohne Ton und Vibration.
3. B tippt sie an oder schließt sie. Beides sendet die Antwort.
4. A sieht "✓" mit Uhrzeit.

Reagiert B innerhalb der Wartezeit nicht:
- Liegt eine gültige Zustandsmeldung vor, sendet die App diese als Antwort. A sieht den Grund und dass er gemeldet, nicht getippt wurde, z. B. "schläft (gemeldet)".
- Liegt keine vor, passiert nichts: keine Mahnung, keine "ignoriert"-Anzeige bei B, bei A bleibt die Anfrage offen und verfällt.

Die Anzeige bleibt nach einer gemeldeten Antwort stehen. Reagiert B später doch, bekommt A zusätzlich das normale "✓".

## Grundsätze

- Die App soll kein Überwachungswerkzeug werden. Der Angefragte bestimmt, wie oft er gefragt werden darf, und sein Gerät setzt das durch.
- Wie sich eine Anfrage bemerkbar macht, entscheidet allein der Angefragte auf seinem Gerät. Der Anfragende kann weder Ton noch Dringlichkeit beeinflussen.
- Keine Verlaufsstatistik, keine Streaks, keine Lesebestätigung, keine Zustellbestätigung.
- Die App ist kein Notrufsystem. Keine Antwort heißt nicht "Notfall".
- Ein "✓" bedeutet "am Gerät hat jemand reagiert", nicht "der Person geht es gut".
- Gemeldete Antworten sind für A immer als solche erkennbar.

## Anzeige der Anfrage

Das Wecksignal vom Relay ans Gerät ist unsichtbar. Sichtbar ist nur, was die App daraufhin anzeigt.

### Vordergrund (Standard)

| Plattform | Umsetzung | Einschränkung |
|---|---|---|
| Android | Benachrichtigung mit Vollbild-Intent, wie Wecker und Anruf | Vollbild nur bei gesperrtem oder ausgeschaltetem Bildschirm; während der Nutzung erscheint ein Banner. Die Berechtigung ist per Sideload aktiv; bei Installation über den Play Store müssten Nutzer sie selbst einschalten. |
| Windows | Eigenes Fenster über allen anderen | – |
| Linux, X11 | Eigenes Fenster über allen anderen | – |
| Linux, Wayland | Nicht zuverlässig: Apps dürfen sich nicht selbst nach vorn holen | Rückfall auf Benachrichtigung mit Dringlichkeit "kritisch", die stehen bleibt. Unter KDE Plasma hilft lokal eine KWin-Fensterregel. |
| macOS (später) | Eigenes Fenster | – |
| iOS (später) | Nicht möglich | Nur Benachrichtigung. |

Regeln für die Vordergrund-Anzeige:
- "Nicht stören" des Systems wird respektiert.
- Auf dem Desktop nimmt das Fenster keinen Tastaturfokus. Nur Klick oder Tippen auf das Fenster oder das Schließen zählt als Antwort; ein Tastendruck mitten im Tippen darf nie als Lebenszeichen gelten.

### Ton und Vibration

- Standardmäßig aus, bei Vordergrund-Anzeige und Benachrichtigung gleichermaßen.
- Der Angefragte kann beides pro Gerät einschalten. So bleibt etwa das Handy am Bett still, während der Rechner im Arbeitszimmer einen Ton gibt.
- Die Einstellung gilt nur lokal und wird nicht übertragen.
- "Nicht stören" des Systems hat auch bei eingeschaltetem Ton Vorrang.
- Auf Android steuert das System Ton und Vibration über den Benachrichtigungskanal. Die App legt den Kanal still an und verweist für Änderungen auf die Systemeinstellungen.

### Benachrichtigung (Rückfall und wählbare Alternative)

Wegklicken zählt als Antwort, automatisches Ablaufen nie.

| Plattform | Mechanismus | Zu beachten |
|---|---|---|
| Android | `deleteIntent` der Benachrichtigung | Feuert auch bei "Alle löschen"; das ist gewollt. |
| Linux | D-Bus-Signal `NotificationClosed` | Nur Grund 2 (vom Nutzer geschlossen) zählt, Grund 1 (abgelaufen) nicht. |
| Windows | `Dismissed`-Ereignis des Toasts | Nur `UserCanceled` zählt, `TimedOut` nicht. |
| iOS (später) | Kategorie mit `customDismissAction` | Ohne diese Option meldet iOS das Wegwischen nicht. |

Antwortet ein Gerät, wird die Anzeige auf den anderen Geräten derselben Person zurückgezogen.

## Lokale Schnittstelle für Zustandsmeldungen

Externe Programme (Schlaferkennung, Hausautomation, Skript am CPAP-Gerät) melden der App auf demselben Gerät einen Zustand.

**Umfang**
- `setzen(grund, dauer)`: Der Zustand gilt ab jetzt für die angegebene Zeitspanne und verfällt danach von selbst.
- `löschen()`: Der Zustand endet sofort.
- Es gibt immer höchstens einen Zustand; ein neuer ersetzt den alten.

**Regeln**
- Die Meldung wird erst nach Ablauf der einstellbaren Wartezeit für eine normale Antwort verwendet und nur, wenn sie dann noch gültig ist.
- Pro Kontakt einstellbar, ob gemeldete Antworten gesendet werden. Standard: aus.
- Der Grund ist ein kurzer Text, den B selbst festlegt. A sieht genau diesen Text, also "schläft" statt "CPAP aktiv".
- Übertragen werden nur Grund und Kennzeichnung "gemeldet", nie die Restdauer oder der Zeitpunkt der Meldung.
- Nur lokal erreichbar: Unix-Socket (Linux), Named Pipe (Windows), Intent mit eigener Berechtigung (Android). Kein Netzwerk-Port.
- Die Meldung gilt nur auf dem Gerät, das sie erhalten hat.

**Nicht vorgesehen:** Übertragung von Tonausschnitten oder anderen Sensordaten. Android verweigert Diensten, die aus dem Hintergrund starten, den Mikrofonzugriff, und die Funktion hätte hohes Missbrauchspotenzial.

## Sicherheit und Privatsphäre

- Kein Konto, keine Telefonnummer, keine E-Mail-Adresse.
- Kopplung per QR-Code oder Einmalcode. Pro Kontakt ein eigenes Schlüsselpaar und ein eigenes Postfach; keine globale Kennung.
- Namen werden nie übertragen.
- Schlüssel bleiben auf den Geräten der Person. Ende-zu-Ende-Verschlüsselung mit libsodium-kompatibler `crypto_box`.
- Das Relay sieht nur Chiffrat fester Länge und zufällige Postfach-Kennungen und löscht nach Zustellung.
- Wecksignale enthalten keinen Inhalt.
- Bekannte Grenze: Wer das Relay aktiv beobachtet, sieht Zeitpunkte und kann Kontaktpaare ableiten. Bei selbst betriebenem Relay vertretbar.

## Architektur

Drei Schichten im Client:

1. **Rust-Kern** als eigene Bibliothek: Protokoll, Verschlüsselung, Regelprüfung, Speicher. Keine Abhängigkeit von Tauri. Das Relay nutzt dieselbe Nachrichtenhülle.
2. **Tauri** für Oberfläche, Tray und das Vordergrund-Fenster auf dem Desktop.
3. **Kotlin-Schicht auf Android** für UnifiedPush, Vollbild-Anzeige und Benachrichtigungen. Sie ruft den Kern direkt auf, weil beim Wecksignal keine WebView läuft.

Weitere Bausteine:

- **Relay:** kleiner Rust-Dienst im Docker-Container, hinter Reverse-Proxy mit TLS und abgeschalteten Zugriffsprotokollen. Muss aus dem Internet erreichbar sein, da die Kontakte nicht im eigenen Netz sind.
- **Wecksignal auf Android:** UnifiedPush mit selbst gehostetem ntfy, damit kein Google-Dienst nötig ist. Für Tauri gibt es dafür kein fertiges Plugin; die Anbindung entsteht in der Kotlin-Schicht.
- **Desktop:** Tray-Anwendung mit dauerhafter WebSocket-Verbindung zum Relay.
- **Client ohne Oberfläche:** Kommandozeilen-Programm auf Basis des Kerns, für Tests und für Geräte ohne Bildschirm.

Für eine spätere Veröffentlichung: Relay-Adresse nie fest einbauen, Weckweg austauschbar halten (UnifiedPush, FCM, APNs).

## Lizenz

- **Clients und Kern:** GPL-3.0-or-later mit ausdrücklicher App-Store-Ausnahme als Zusatzerlaubnis. Version 3, weil viele Rust-Abhängigkeiten unter Apache 2.0 stehen und das nur mit Version 3 verträglich ist.
- **Relay:** AGPL-3.0-or-later, damit auch veränderte Relays, die nur als Dienst betrieben werden, offenzulegen sind.
- **Warum die Ausnahme:** Die GPL verträgt sich schlecht mit den Bedingungen von Apples App Store. Mit der Ausnahme von Anfang an tragen spätere Beitragende automatisch unter denselben Bedingungen bei, und iOS bleibt möglich.
- **Offen:** der genaue Wortlaut der Ausnahme, siehe `lizenz-ausnahme.md`. Er sollte vor dem ersten fremden Beitrag feststehen und juristisch gegengelesen sein.

## Quellcode-Hosting und CI

Entschieden: GitHub.

- **Builds:** GitHub Actions mit gehosteten Linux- und Windows-Runnern; der Windows-Build braucht damit keine eigene Infrastruktur.
- **Abhängigkeiten:** Renovate statt Dependabot, weil es auch auf Forgejo läuft.
- **Umzug offen halten:** Workflows im Actions-Format sind weitgehend mit Forgejo Actions kompatibel, falls das Projekt später nach Codeberg zieht.

Gegen Codeberg sprach zum Entscheidungszeitpunkt: gehostete Forgejo Actions nur eingeschränkt, Woodpecker nur auf Antrag, für Windows eine eigene VM als Runner nötig.

## Reihenfolge

Kurzfassung der bestätigten Reihenfolge; Einzelheiten und Abnahmekriterien stehen in `fahrplan.md`.

1. Grundgerüst und Durchstich auf Android. Scheitert der Durchstich mit Tauri, ist ein Wechsel auf Flutter hier noch billig.
2. Rust-Kern und Relay, bedienbar über die Kommandozeile.
3. Linux-Desktop, dann Android-App.
4. Mehrere Geräte, Windows, lokale Schnittstelle.
5. Härtung; später eventuell iOS, macOS und Veröffentlichung.

## Offene Punkte

- Wortlaut der App-Store-Ausnahme.
- Technik der Oberfläche und Schutz der Schlüssel auf dem Gerät (siehe `fahrplan.md`).

## Recherche: ähnliche Apps

Tägliche Check-in-Apps gibt es viele, aber keine gefundene verbindet Abfrage nur auf Anfrage, Ende-zu-Ende-Verschlüsselung und Desktop-Unterstützung.

- **Totmannschalter für Alleinlebende** (Sileme/Demumu, Snug, Alma, leb, Corlo): fester Zeitplan, Alarmkette bei Ausbleiben.
- **Soziale Check-ins** (Knuff, Cercle, Just Checking In): laufend sichtbarer Status, oft mit Stimmung und Chat. Knuff ist am nächsten dran.
- **Technische Vorbilder** (Starling, LastSignal): Ende-zu-Ende-verschlüsselt, ohne Konto, selbst hostbares Relay.
