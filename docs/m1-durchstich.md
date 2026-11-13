# Lebenszeichen – M1: Durchstich auf Android

Stand: 13.11.2026. Gehört zu `konzept.md` und `fahrplan.md`.

M1 ist ein Wegwerf-Prototyp. Er klärt eine einzige Frage: Trägt die Entscheidung für Tauri auf Android, obwohl beim Wecksignal keine WebView läuft? Wenn nicht, wird laut Fahrplan hier auf Flutter gewechselt.

**Der Code ist bisher nur übersetzt und nie auf einem Gerät gelaufen.** Ob der Durchstich gelingt, entscheidet erst der Test in diesem Dokument. Was nicht überprüft ist, steht im Abschnitt [Nicht verifiziert](#nicht-verifiziert).

## Was der Prototyp tut

1. Die App meldet sich über UnifiedPush bei einem Verteiler an (gedacht ist die ntfy-App) und zeigt den Endpunkt an.
2. Ein HTTP POST an den Endpunkt ist das Wecksignal. Der Verteiler startet damit die App, auch wenn sie beendet ist.
3. Die Kotlin-Schicht ruft per JNI die Funktion `prototype::marker` im Rust-Kern auf. Tauri, Activity und WebView laufen dabei nicht.
4. Die Anfrage erscheint als stille Benachrichtigung mit Vollbild-Intent: im Vollbild, wenn das Gerät gesperrt oder der Bildschirm aus ist, sonst als Banner. Kein Ton, keine Vibration.
5. Jede dieser Reaktionen sendet genau einen HTTP POST an eine einstellbare Adresse:
   - Benachrichtigung antippen (`tap`)
   - Benachrichtigung wegwischen, auch über „Alle löschen“ (`dismiss`)
   - Vollbild-Anzeige antippen (`fullscreen-tap`)
   - Vollbild-Anzeige mit „Zurück“ schließen (`fullscreen-close`)
6. Der Text des POST kommt aus dem Rust-Kern, zum Beispiel:

   ```
   lebenszeichen-m1 core=rust wire=1 event=dismiss request=r20261113-203300-a1b2
   ```

   Ein empfangener POST mit `core=rust` belegt also, dass der Kern im geweckten Prozess gelaufen ist. Erreicht die Kotlin-Schicht den Kern nicht, steht stattdessen `rust-core-unavailable` mit dem Grund im Text.
7. Ein Protokoll in einer Datei hält jeden Schritt mit Uhrzeit fest und übersteht das Ende des Prozesses.

Nicht als Antwort zählen: der Bildschirm geht von selbst aus, die Vollbild-Anzeige wird über die Home-Taste verlassen, ein Banner verschwindet von selbst. Die Benachrichtigung bleibt dann stehen und kann später beantwortet werden. Der Prototyp lässt Benachrichtigungen nie ablaufen.

## Was der Prototyp nicht tut

- Keine Verschlüsselung, keine Kontakte, kein Relay, keine Regeln für Anfragen. Das ist M2 und M4.
- Der Inhalt des Wecksignals wird nicht gelesen. Jedes Wecksignal erzeugt eine neue Anfrage mit eigener Kennung.
- Die Antwort geht unverschlüsselt an die eingestellte Adresse. Sie enthält nur die Markierung, die Art der Reaktion und die Kennung der Anfrage.
- Kein Ablauf nach 24 Stunden, kein Zurückziehen auf anderen Geräten.
- Kein eigenes App-Symbol (Standard-Symbol von Tauri), keine Übersetzungen, keine Barrierefreiheits-Prüfung.
- Nur Android, nur arm64, nur als Debug-Build.

## Aufbau

```
app/
├── src-tauri/   Tauri-Hülle (Rust). Enthält die JNI-Funktion für die Kotlin-Schicht.
├── ui/          Oberfläche: schlichtes HTML, CSS und JavaScript ohne Build-Schritt
├── android/     Kotlin-Schicht als lokales Tauri-Plugin
│   ├── src/         Rust-Seite des Plugins (meldet nur die Kotlin-Klasse an)
│   └── kotlin/      Android-Bibliotheksmodul: Manifest, Kotlin-Code
└── package.json Nur für die Tauri-CLI
crates/core/     Platzhalter-Funktion prototype::marker
```

Der Weg einer Anfrage:

```
curl ──POST──▶ ntfy-Server ──▶ ntfy-App (Verteiler)
                                   │ Broadcast
                                   ▼
        Empfänger der Connector-Bibliothek ──▶ PushServiceImpl.onMessage
                                                   │
                           Requests.open ──JNI──▶ lz_core::prototype::marker
                                   │
                    RequestPresenter.show: Benachrichtigung mit Vollbild-Intent
                                   │
      ┌────────────────────────────┼─────────────────────────────┐
      ▼                            ▼                             ▼
 antippen                     wegwischen                   Vollbild-Anzeige
 (ReactionReceiver)           (ReactionReceiver)           (RequestActivity)
      └────────────────────────────┼─────────────────────────────┘
                                   ▼
        Requests.react: einmal pro Anfrage ──JNI──▶ marker ──▶ AnswerWorker
                                                                  │ HTTP POST
                                                                  ▼
                                                            Antwort-Adresse
```

Die Oberfläche ist für diesen Weg nicht nötig. Sie zeigt nur Zustand und Protokoll und spricht über Tauri-Befehle mit der Kotlin-Klasse `LebenszeichenPlugin`.

## Entscheidungen

**Kotlin-Schicht als lokales Tauri-Plugin.** Ein Tauri-Plugin darf ein Android-Bibliotheksmodul mitbringen. Tauri bindet es beim Bauen in das erzeugte Gradle-Projekt ein; Manifest (Dienst, Empfänger, Activity, Berechtigungen) und Abhängigkeiten des Moduls werden dabei zusammengeführt. Am erzeugten Projekt muss deshalb nichts von Hand geändert werden. Abweichung vom Fahrplan: Der Kotlin-Code liegt unter `app/android/kotlin/` statt direkt unter `app/android/`, weil im selben Ordner auch das Rust-Paket des Plugins liegt.

**Das Android-Projekt wird nicht eingecheckt.** `app/src-tauri/gen/android` erzeugt die CI bei jedem Lauf mit `tauri android init` neu. Für einen Wegwerf-Prototyp heißt das: keine erzeugten Gradle-Dateien im Repository, und das Projekt passt immer zur verwendeten Tauri-CLI. Der Preis: Anpassungen am erzeugten Projekt sind nicht möglich, App-Symbol und `targetSdk` kommen aus der Vorlage von Tauri. Für M4 sollte das Projekt eingecheckt werden, wie Tauri es vorsieht.

**Eine gemeinsame native Bibliothek.** Die JNI-Funktion liegt im Paket der Tauri-App (`app/src-tauri/src/jni_bridge.rs`) und damit in derselben Bibliothek `liblebenszeichen_lib.so`, die Tauri ohnehin in die APK legt. Die Kotlin-Schicht lädt sie selbst mit `System.loadLibrary`. Gründe:
- Die Bibliothek landet ohne zusätzlichen Build-Schritt in der APK.
- Genau das ist das Risiko, das M1 klären soll: Lässt sich die Tauri-Bibliothek laden und benutzen, ohne dass Tauri läuft? Laut Quelltext ja: Weder Tauri noch `wry` oder `tao` haben ein `JNI_OnLoad`; beim Laden läuft nur ein harmloser Konstruktor aus `tauri-utils`, der den Pfad des Programms merkt.
- Die Funktion steht nicht in `lz-core`, weil JNI-Exporte `unsafe`-Attribute brauchen und der Workspace `unsafe_code` verbietet. `lz-core` bleibt frei von Android.

Die CI prüft nach dem Bauen, dass das Symbol `Java_io_github_irgendwer92_lebenszeichen_droid_CoreBridge_marker` in der Bibliothek der APK exportiert ist. Der Name der Bibliothek steht an zwei Stellen und muss gleich bleiben: `[lib] name` in `app/src-tauri/Cargo.toml` und `LIBRARY` in `CoreBridge.kt`.

**App außerhalb des Workspace.** `app/` ist im Wurzel-`Cargo.toml` ausgeschlossen. Tauri baut auf Linux nur mit WebKitGTK und bringt einen großen Abhängigkeitsbaum mit. So bleibt der Ablauf „Prüfen“ unverändert schnell. Folge: `cargo deny`, Clippy und Tests laufen nicht über den Code unter `app/`.

**UnifiedPush über die offizielle Connector-Bibliothek.** `org.unifiedpush.android:connector` in der Dienst-Variante (`PushService`), wie die aktuelle Dokumentation sie vorsieht; der Empfänger kommt aus der Bibliothek. Kein eingebetteter FCM-Verteiler, keine Google-Dienste. Den Verteiler wählt der Prototyp ohne Rückfrage: den gespeicherten, sonst ntfy (`io.heckel.ntfy`), sonst den ersten gefundenen.

**Nach einem Neustart ist nichts zu tun.** Die Registrierung liegt beim Verteiler und in den Daten der App. Der Verteiler startet nach dem Neustart selbst und weckt die App per Broadcast. Die Bibliothek verlangt dafür keinen eigenen Empfänger für den Systemstart. Die Dokumentation empfiehlt nur, die Registrierung regelmäßig zu erneuern; das geschieht bei jedem Öffnen der Oberfläche.

**Antwort über WorkManager.** Der Empfänger für Antippen und Wegwischen lebt nur wenige Sekunden. Er sendet deshalb nicht selbst, sondern übergibt die Antwort an WorkManager und wartet nur, bis der Auftrag gespeichert ist. WorkManager wartet auf eine Netzverbindung, wiederholt nach Netzfehlern (höchstens sechs Versuche, wachsende Abstände) und übersteht Prozessende und Neustart. Ab Android 12 läuft der Auftrag als beschleunigter Auftrag und startet sofort. Es gibt bewusst nur diesen einen Weg: Ein zusätzlicher Direktversuch könnte bei einer Zeitüberschreitung zu zwei POSTs führen.

**Genau eine Antwort pro Anfrage.** Die erste Reaktion setzt synchron eine Marke in den Einstellungen der App; jede weitere Reaktion auf dieselbe Anfrage wird nur protokolliert (`reaction-ignored`). Zusätzlich nimmt WorkManager pro Anfrage nur einen Auftrag an. Bleibt ein Rest: Nimmt der Server den POST an, aber die Bestätigung geht verloren, sendet die Wiederholung ihn ein zweites Mal. Die Kennung der Anfrage steht deshalb im Text.

**Still, aber wichtig.** Der Benachrichtigungskanal hat hohe Wichtigkeit, weil Android sonst weder Vollbild noch Banner zeigt. Ton und Vibration sind am Kanal abgeschaltet. Die Kategorie ist „Erinnerung“, nicht „Wecker“: Wecker dürfen „Nicht stören“ in der Voreinstellung durchbrechen, und das Konzept verlangt, dass „Nicht stören“ respektiert wird.

**Home-Taste zählt nicht.** Das Konzept sagt „jede Reaktion zählt“. Der Prototyp wertet beim Vollbild nur Tippen und „Zurück“, weil sich das Verlassen über die Home-Taste nicht sicher von Vorgängen ohne Zutun unterscheiden lässt. Die Benachrichtigung bleibt in dem Fall stehen. Ob das so bleiben soll, ist für M4 zu entscheiden.

## Versionen

| Baustein | Version |
|---|---|
| Tauri | 2.12.5 (`tauri-build` 2.7.4, `tauri-plugin` 2.7.2) |
| Tauri-CLI | 2.12.4 (npm-Paket `@tauri-apps/cli`) |
| `jni` | 0.21.1 |
| UnifiedPush-Connector | 3.3.5 |
| WorkManager | 2.10.0 |
| Android-Gradle-Plugin, Kotlin, Gradle | 9.3.1, 2.2.10, 9.6.1 (aus der Vorlage der Tauri-CLI) |
| `compileSdk` / `targetSdk` / `minSdk` der App | 37 / 37 / 24 (aus der Vorlage der Tauri-CLI) |
| NDK, JDK, Rust | 30.0.16248370, Temurin 21, 1.100.0 (vom CI-Runner, nicht festgeschrieben) |

## APK holen und installieren

1. Auf GitHub: **Actions → Android-Prototyp →** letzter grüner Lauf **→ Artifacts →** `lebenszeichen-m1-arm64-debug-apk` herunterladen und entpacken. Darin liegt `lebenszeichen-m1-arm64-debug.apk`. Mit der GitHub-CLI:

   ```sh
   gh run download --repo irgendwer92/are-you-alive- --name lebenszeichen-m1-arm64-debug-apk
   ```

2. Installieren, entweder per USB:

   ```sh
   adb install -r lebenszeichen-m1-arm64-debug.apk
   ```

   oder die Datei aufs Handy kopieren, dort öffnen und der Dateiverwaltung die Installation aus unbekannten Quellen erlauben.

Der Ablauf startet bei jedem Pull Request, der `app/`, `crates/` oder den Ablauf selbst ändert, und von Hand über „Run workflow“. Artefakte werden 30 Tage aufbewahrt.

**Zur Signatur:** Debug-Builds werden mit einem Debug-Schlüssel signiert, den das Android-Gradle-Plugin auf dem CI-Runner erzeugt. Der Ablauf hebt diesen Schlüssel im Cache von GitHub Actions auf, damit aufeinanderfolgende Builds dieselbe Signatur haben und sich übereinander installieren lassen. Das ist nur ein Behelf: Der Cache gilt pro Branch oder Pull Request und verfällt nach sieben Tagen ohne Zugriff. Meldet Android beim Installieren, das Paket stehe in Konflikt mit einer vorhandenen App, hat sich die Signatur geändert. Dann die alte App deinstallieren und neu installieren. Dabei gehen Registrierung, Einstellungen und Protokoll verloren, und der Endpunkt ändert sich. Die Prüfsumme des Zertifikats steht im CI-Lauf im Schritt „APK prüfen“ unter „Signatur und Versionen“. Der Debug-Schlüssel ist kein Geheimnis und schützt nichts; die richtige Signatur kommt mit M4.

## Einrichten

Benötigt werden ein ntfy-Server (der eigene oder ein öffentlicher) und die ntfy-App als Verteiler.

1. **ntfy-App installieren**, am besten aus F-Droid; diese Variante kommt ohne Google-Dienste aus. In ntfy den eigenen Server als Standard-Server eintragen, falls einer genutzt wird, und ntfy von der Akku-Optimierung ausnehmen. Die App einmal öffnen.
2. **Lebenszeichen öffnen** und unter „Berechtigungen“ alles auf „Ja“ bringen:
   - Benachrichtigungen erlauben (ab Android 13 fragt das System).
   - Vollbild-Anzeige: ab Android 14 eine eigene Berechtigung. Bei Sideload ist sie in der Regel schon erteilt; sonst führt „Einstellungen“ zur passenden Seite.
   - Akku-Optimierung: „Ausnehmen“.
3. **„Registrieren“ tippen.** Nach wenigen Sekunden steht unter „Endpunkt“ eine Adresse der Form `https://<ntfy-server>/up…?up=1`. Bleibt das Feld leer, steht der Grund im Protokoll.
4. **Antwort-Adresse eintragen und speichern**, zum Beispiel ein ntfy-Thema mit schwer zu ratendem Namen: `https://<ntfy-server>/lz-test-<zufall>`. Das Thema muss ohne Anmeldung beschreibbar sein; der Prototyp sendet keine Zugangsdaten. Dieses Thema nicht in der ntfy-App auf demselben Handy abonnieren, sonst erzeugt jede Antwort dort eine weitere Benachrichtigung.
5. **„Anfrage lokal auslösen“** als erste Probe: Es erscheint ein Banner; antippen. Im Protokoll folgen `local-trigger`, `core`, `shown`, `reaction`, `core`, `answer-queued`, `answer-sent`.

### Wecksignal senden und Antworten beobachten

Am Rechner in einem Fenster die Antworten mitlesen (eine Zeile pro Antwort):

```sh
curl -sN "https://<ntfy-server>/lz-test-<zufall>/raw"
```

In einem zweiten Fenster wecken. Den fertigen Befehl kopiert die App über „curl-Befehl kopieren“:

```sh
curl -d "wecken" "https://<ntfy-server>/up…?up=1"
```

Der Inhalt ist beliebig, sollte aber nicht leer sein. Für Zeitmessungen:

```sh
date +%T; curl -d "wecken" "https://<ntfy-server>/up…?up=1"
```

## Protokoll in der App

Jede Zeile: Uhrzeit, Kennung der Anfrage, Ereignis, Einzelheiten. Neueste zuerst. „Kopieren“ legt das ganze Protokoll in die Zwischenablage. Dieselben Zeilen stehen in `adb logcat -s Lebenszeichen`.

| Ereignis | Bedeutung |
|---|---|
| `register`, `endpoint`, `registration-failed`, `unregistered`, `unregister` | Anmeldung beim Verteiler |
| `wake` | Wecksignal über UnifiedPush angekommen |
| `local-trigger` | Anfrage über den Knopf in der App |
| `core` | Rückgabe des Rust-Kerns; `core=rust` heißt, der Aufruf hat geklappt |
| `shown` | Benachrichtigung abgesetzt, mit Zustand: Bildschirm an, gesperrt, Vollbild erlaubt |
| `not-shown` | Anzeige nicht möglich, mit Grund |
| `fullscreen-shown` | Vollbild-Anzeige wurde tatsächlich geöffnet |
| `reaction` | Erste Reaktion, mit Art |
| `reaction-ignored` | Weitere Reaktion auf eine schon beantwortete Anfrage |
| `answer-queued` | Antwort an WorkManager übergeben |
| `answer-sent` | HTTP POST angenommen, mit Statuscode und Versuch |
| `answer-retry`, `answer-failed` | POST fehlgeschlagen, mit Grund |

## Testprotokoll

Abnahmekriterium aus dem Fahrplan: Es klappt auf einem echten Gerät zehnmal hintereinander, auch nach einem Neustart und über Nacht.

Vorbereitung: eingerichtet wie oben, Mitlesen der Antworten läuft, Protokoll in der App geleert. „App beendet“ heißt: aus der Liste der letzten Apps gewischt. Nicht „Beenden erzwingen“ in den Einstellungen verwenden; das sperrt die App für Wecksignale, bis sie wieder geöffnet wird (siehe Fallstricke).

Ein Durchlauf gilt als bestanden, wenn alle vier Punkte stimmen:

- **A** Die Anzeige erscheint in der erwarteten Form.
- **B** Auf die Reaktion kommt genau ein POST an, mit `core=rust` und der erwarteten Reaktion.
- **C** Im Protokoll steht für die Anfrage die Kette `wake` → `core` → `shown` → `reaction` → `core` → `answer-queued` → `answer-sent`.
- **D** Ohne Reaktion kommt kein POST.

| Nr. | Ausgangslage | Reaktion | Erwartete Anzeige | Erwarteter POST | Wecksignal gesendet | Anzeige erschienen | POST angekommen | Bestanden |
|---|---|---|---|---|---|---|---|---|
| 1 | App beendet, Bildschirm aus und gesperrt | Vollbild antippen | Vollbild | `fullscreen-tap` | | | | |
| 2 | App beendet, Bildschirm aus und gesperrt | Vollbild mit „Zurück“ schließen | Vollbild | `fullscreen-close` | | | | |
| 3 | App beendet, Bildschirm aus und gesperrt | 2 Minuten nichts tun, dann entsperren und die Benachrichtigung wegwischen | Vollbild, danach Benachrichtigung | In den 2 Minuten keiner, dann `dismiss` | | | | |
| 4 | App beendet, Bildschirm an, andere App offen | Banner antippen | Banner | `tap` | | | | |
| 5 | App beendet, Bildschirm an, andere App offen | Banner verschwinden lassen, Leiste öffnen, einzeln wegwischen | Banner | Erst keiner, dann `dismiss` | | | | |
| 6 | App beendet, Bildschirm an, andere App offen | Leiste öffnen, „Alle löschen“ | Banner | `dismiss` | | | | |
| 7 | Zwei Wecksignale kurz nacheinander, App beendet, Bildschirm an | Eine antippen, eine wegwischen | Zwei Benachrichtigungen | Zwei POSTs mit verschiedenen Kennungen: `tap`, `dismiss` | | | | |
| 8 | Gerät neu gestartet und einmal entsperrt, Lebenszeichen nicht geöffnet, 5 Minuten gewartet | Beliebig | Je nach Bildschirm | Passend zur Reaktion | | | | |
| 9 | Über Nacht: Gerät liegt mindestens 6 Stunden still, nicht am Ladegerät, Bildschirm aus. Wecksignal morgens senden, bevor das Gerät angefasst wird | Vollbild antippen | Vollbild | `fullscreen-tap` | | | | |
| 10 | Über Nacht: Wecksignal abends senden, Anzeige nicht anfassen. Morgens entsperren | Benachrichtigung wegwischen | Abends Vollbild, morgens Benachrichtigung | Abends keiner, morgens `dismiss` | | | | |

Zu jedem Durchlauf notieren:

- Uhrzeit des Wecksignals (`date +%T` vor dem `curl`)
- Uhrzeit von `wake` und `shown` im Protokoll, daraus die Verzögerung
- Uhrzeit, zu der der POST am Rechner ankam, und sein Text
- Anzahl der POSTs zu dieser Kennung
- bei 9 und 10 zusätzlich: Stand des Akkus, WLAN oder Mobilfunk

Einmalig notieren: Gerät, Android-Version und Oberfläche des Herstellers (stehen in der App unter „Gerät“), Version der ntfy-App, ntfy-Server, Nummer des CI-Laufs der APK.

**Auswertung:**

- Alle zehn bestanden: M1 ist erfüllt, die Entscheidung für Tauri steht.
- Schlägt ein Durchlauf fehl, zuerst im Protokoll die Stelle suchen, an der die Kette abreißt:
  - Kein `wake`: Das Wecksignal hat die App nicht erreicht. Das liegt am Verteiler, am Akku-Sparen oder an der Zustellung, nicht an Tauri. Mit Flutter wäre es dasselbe.
  - `wake`, aber `rust-core-unavailable` statt `core=rust`: Der Kern lässt sich im geweckten Prozess nicht aufrufen. Das ist die Frage, um die es in M1 geht.
  - `shown`, aber nichts zu sehen: Berechtigungen und „Nicht stören“ prüfen.
  - `reaction`, aber kein `answer-sent`: Netz oder WorkManager; `answer-retry` und `answer-failed` nennen den Grund.
- Für das Abbruchkriterium zählen nur Fehler, die am Zusammenspiel von Tauri, Kotlin-Schicht und Rust-Kern liegen. Was Android jeder App auferlegt, trifft Flutter genauso.

## Fallstricke auf Android

- **Akku-Sparen der Hersteller.** Xiaomi, Huawei, Samsung, OnePlus, Oppo und andere beenden Apps im Hintergrund härter als Android selbst und verhindern teils, dass sie geweckt werden. Für Lebenszeichen und für ntfy die Akku-Optimierung abschalten, je nach Hersteller zusätzlich „Autostart“ erlauben und die App vor dem Aufräumen schützen. Anleitungen je Hersteller sammelt dontkillmyapp.com.
- **„Beenden erzwingen“.** Danach stellt Android der App keine Broadcasts mehr zu, bis sie von Hand geöffnet wird. Manche Hersteller behandeln schon das Wegwischen aus den letzten Apps so. Zeigt sich das im Test bei Durchlauf 1 bis 7, ist es eine Eigenheit des Geräts.
- **Doze.** Liegt das Gerät still, bündelt Android Netzzugriffe in Wartungsfenstern. Ob ein Wecksignal sofort ankommt, hängt davon ab, ob der Verteiler seine Verbindung halten darf; deshalb muss ntfy von der Akku-Optimierung ausgenommen sein. Verzögerungen von Minuten sind im Tiefschlaf möglich und kein Fehler des Prototyps. Die Antwort selbst läuft ab Android 12 als beschleunigter Auftrag.
- **Vollbild nur bei gesperrtem Gerät.** Ist der Bildschirm an und entsperrt, zeigt Android statt des Vollbilds ein Banner. Das ist so gewollt und nicht zu ändern.
- **Berechtigung für Vollbild.** Ab Android 14 kann sie entzogen sein; dann erscheint auch bei gesperrtem Gerät nur eine Benachrichtigung. Die App zeigt den Zustand an.
- **Banner nach oben schieben** ist kein Wegwischen: Die Benachrichtigung wandert nur in die Leiste und bleibt offen. Erst das Wegwischen dort oder zur Seite zählt.
- **„Nicht stören“.** Die Anfrage wird dann still in der Leiste abgelegt, ohne Vollbild und ohne Banner. Das ist gewollt. Wer es anders will, erlaubt in den Einstellungen von „Nicht stören“ Erinnerungen oder nimmt die App aus.
- **Sperrbildschirm.** Sind Benachrichtigungen auf dem Sperrbildschirm ausgeblendet, bleibt nur die Vollbild-Anzeige.
- **Kanal-Einstellungen sind dauerhaft.** Ton, Vibration und Wichtigkeit eines Kanals legt Android beim ersten Anlegen fest; danach kann nur noch der Nutzer sie ändern. Wer am Kanal herumgestellt hat, bekommt die Vorgaben nur durch Neuinstallation zurück. Die App zeigt an, ob der Kanal noch hohe Wichtigkeit hat.
- **Nach dem Neustart einmal entsperren.** Vor dem ersten Entsperren sind die Daten der Apps verschlüsselt; weder ntfy noch Lebenszeichen laufen dann.
- **`targetSdk` 37.** Die Vorlage von Tauri zielt auf die neueste Android-Version. Verhalten, das Android an das Ziel-SDK knüpft, kann sich dadurch von älteren Anleitungen unterscheiden.

## Nicht verifiziert

Geprüft ist: Der Rust-Teil des Workspace besteht Formatierung, Clippy und Tests. Die JNI-Funktion wurde auf einem Linux-Rechner aus einer JVM heraus aufgerufen und liefert den erwarteten Text. Die CI baut die APK und prüft, dass das JNI-Symbol exportiert ist, dass Dienst, Empfänger, Activity und Berechtigungen im zusammengeführten Manifest stehen und dass die Klassen der Kotlin-Schicht in der APK liegen. Zwei aufeinanderfolgende CI-Läufe im selben Pull Request haben APKs mit derselben Signatur erzeugt.

Alles Folgende ist nur übersetzt und nie auf einem Gerät oder Emulator gelaufen:

- **Die App als Ganzes.** Ob sie startet und die Oberfläche lädt.
- **Die Befehle der Oberfläche.** Namen der Befehle, Freigabe über die Berechtigungen von Tauri, Übergabe der Argumente. Ein Fehler zeigt sich als Meldung oben in der App.
- **Registrierung und Empfang über UnifiedPush**, auch mit ntfy als Verteiler. Insbesondere: dass ntfy einen unverschlüsselten Text durchreicht und die Bibliothek ihn als Nachricht meldet, und dass die Bibliothek den nicht exportierten Dienst auf aktuellen Android-Versionen erreicht. Gelesen wurde der Quelltext der Bibliothek in Version 3.0.10; gebaut wird mit 3.3.5, deren Quelltext hier nicht einsehbar war. Die Schnittstelle ist laut API-Dokumentation dieselbe.
- **Der Kern des Durchstichs:** dass `System.loadLibrary` und der JNI-Aufruf im geweckten Prozess ohne Activity funktionieren. Belegt ist nur, dass nichts im Quelltext dagegen spricht.
- **Die Anzeige:** Vollbild bei gesperrtem Gerät, Banner ohne Ton und Vibration bei hoher Wichtigkeit, Verhalten bei „Nicht stören“, Darstellung der Vollbild-Anzeige (Ränder, Dunkelmodus).
- **Die Reaktionen:** dass das Wegwischen und „Alle löschen“ den `deleteIntent` auslösen, das Verschwinden eines Banners aber nicht; dass „Zurück“ in der Vollbild-Anzeige ankommt; dass genau ein POST entsteht.
- **WorkManager:** Start aus dem geweckten Prozess, beschleunigte Aufträge aus dem Hintergrund, Wiederholung nach Netzfehlern.
- **Neustart, Doze, Akku-Sparen der Hersteller.** Die Aussage „nach einem Neustart ist nichts zu tun“ stützt sich auf Dokumentation und Quelltext der Bibliothek.
- **Android unter 12.** Dort läuft die Antwort als gewöhnlicher Auftrag und kann sich verzögern. Android unter 8 (ohne Kanäle) ist gar nicht bedacht worden.
- **Lizenzen der App-Abhängigkeiten.** `cargo deny` läuft nicht über `app/`. Die Abhängigkeiten von Tauri und die Android-Bibliotheken (Connector, WorkManager, Tink: Apache-2.0) sind nicht förmlich gegen GPL-3.0-or-later geprüft.

Nicht ausprobiert und deshalb ohne Gewähr sind auch die Hinweise zur ntfy-App (Variante aus F-Droid, Standard-Server).

## Wie es weitergeht

- Test nach dem Protokoll oben. Das Ergebnis entscheidet über Tauri oder Flutter.
- Bei Erfolg bleibt der Prototyp liegen, bis M4 ihn ersetzt. Übernommen werden die Erkenntnisse, nicht der Code.
- Für M4 offen: Android-Projekt einchecken, eigene Signatur, App-Symbol, Ablauf nach 24 Stunden, Zählt die Home-Taste als Reaktion, Ton und Vibration über die Systemeinstellungen des Kanals.
