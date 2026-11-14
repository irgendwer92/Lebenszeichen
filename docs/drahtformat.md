# Lebenszeichen – Drahtformat und Relay-Schnittstelle (Version 1)

Stand: 13.11.2026. Konkretisiert `protokoll.md` für Meilenstein M2. Dieses Dokument ist die verbindliche Vorlage für `crates/wire`, `crates/core` und `crates/relay` und soll ausreichen, um einen Client in einer anderen Sprache zu schreiben, etwa für einen Microcontroller.

Die Festlegungen hier sind neu und noch nicht einzeln bestätigt. Sie sind im Abschnitt [Festlegungen dieses Dokuments](#festlegungen-dieses-dokuments) gesammelt.

Schreibweisen: Alle Zahlen sind vorzeichenlos und Big-Endian. `b64url` heißt Base64 mit URL-Alphabet ohne Füllzeichen (RFC 4648, Abschnitt 5). `hex` heißt Kleinbuchstaben. Zeiten sind Unix-Sekunden in UTC.

## Schlüssel und Geheimnisse

| Wert | Länge | Herkunft |
|---|---|---|
| Kontaktschlüssel | X25519, 32 Byte öffentlich, 32 Byte geheim | Client, je Kontakt neu |
| Postfach-Kennung | 32 Byte | Relay, zufällig |
| Lesetoken, Sendetoken | je 32 Byte | Client, zufällig |
| Gutschein | 32 Byte | Relay, zufällig |
| Leser-Kennung | 16 Byte | Client, zufällig, je Gerät |
| Nachrichten-Kennung | 16 Byte | Client, zufällig, je Nachricht |

Das Relay speichert von Token und Gutscheinen nur `SHA-256(wert)`.

## Umschlag

Jede Nachricht auf dem Draht ist genau 297 Byte lang. Es gibt zwei Formen, die das Relay nicht unterscheiden kann.

**Form A, gewöhnlich** (`crypto_box` aus libsodium: X25519, XSalsa20, Poly1305):

| Offset | Länge | Inhalt |
|---|---|---|
| 0 | 1 | Protokollversion, `0x01` |
| 1 | 24 | Nonce, zufällig |
| 25 | 16 | Prüfwert |
| 41 | 256 | verschlüsselter Klartext |

Byte 25 bis 296 sind genau die Ausgabe von `crypto_box_easy(klartext, nonce, pk_empfänger, sk_absender)`: libsodium stellt den Prüfwert vor das Chiffrat.

**Form B, versiegelt** (`crypto_box_seal` aus libsodium), nur für `hello`:

| Offset | Länge | Inhalt |
|---|---|---|
| 0 | 1 | Protokollversion, `0x01` |
| 1 | 32 | flüchtiger öffentlicher Schlüssel |
| 33 | 16 | Prüfwert |
| 49 | 248 | verschlüsselter Klartext |

Byte 1 bis 296 sind genau die Ausgabe von `crypto_box_seal(klartext, pk_empfänger)`.

Form B ist nötig, weil der Empfänger von `hello` den Schlüssel des Absenders noch nicht kennt. Der Empfänger wählt die Form nach seinem eigenen Zustand: Solange zu einem Postfach eine offene Einladung gehört und noch kein Kontakt, erwartet er Form B, danach nur noch Form A. Was sich nicht entschlüsseln lässt, wird still verworfen.

## Klartext

Der Klartext beginnt immer mit demselben Kopf und wird mit Nullbytes auf die volle Länge aufgefüllt (256 Byte in Form A, 248 Byte in Form B).

| Offset | Länge | Inhalt |
|---|---|---|
| 0 | 1 | Nachrichtentyp |
| 1 | 16 | Nachrichten-Kennung |
| 17 | 8 | Zeit des Absendens |
| 25 | 2 | Länge `n` des Rumpfs |
| 27 | `n` | Rumpf, je nach Typ |
| 27+`n` | Rest | Nullbytes |

Der Rumpf darf in Form A höchstens 229 Byte lang sein, in Form B höchstens 221.

### Nachrichtentypen

| Typ | Name im Code | Name in `protokoll.md` | Rumpf |
|---|---|---|---|
| 1 | `hello` | `hallo` | öffentlicher Kontaktschlüssel (32), Postfach-Kennung des Absenders (32), Sendetoken dafür (32), Länge der Relay-Adresse (1), Relay-Adresse (höchstens 120 Byte UTF-8) |
| 2 | `hello_ok` | `hallo-ok` | leer |
| 3 | `rules` | `regeln` | Mindestabstand in Minuten (4), Ablaufzeit in Minuten (4), Wochenlimit (2) |
| 4 | `request` | `anfrage` | leer; die Nachrichten-Kennung ist die Kennung der Anfrage |
| 5 | `response` | `antwort` | Kennung der Anfrage (16), Art (1): `1` manuell, `2` gemeldet; Länge des Grunds (1), Grund (höchstens 64 Byte UTF-8, nur bei Art 2, sonst Länge 0) |
| 6 | `end` | `ende` | leer |
| 64–127 | reserviert | Abgleich zwischen eigenen Geräten (M5) | – |

Empfänger ignorieren unbekannte Typen. Ist der Rumpf eines bekannten Typs länger als hier beschrieben, werden die zusätzlichen Bytes ignoriert; ist er kürzer, wird die Nachricht verworfen.

Die Relay-Adresse ist die Basisadresse ohne abschließenden Schrägstrich, zum Beispiel `https://relay.example.org`.

### Schutz vor Wiedereinspielen

Ein Empfänger nimmt eine Nachricht nur an, wenn ihre Zeit höchstens 24 Stunden und 10 Minuten zurückliegt und höchstens 10 Minuten in der Zukunft. Er merkt sich angenommene Nachrichten-Kennungen je Kontakt mindestens 25 Stunden und verwirft Wiederholungen.

## Einladung

Die Einladung wird als QR-Code oder als Text weitergegeben: die Zeichen `LZ1-`, danach Base32 (RFC 4648, Großbuchstaben, ohne Füllzeichen) über diese Bytes:

| Länge | Inhalt |
|---|---|
| 1 | Version, `0x01` |
| 1 | Schalter; Bit 0: Gutschein vorhanden |
| 8 | Ablaufzeit der Einladung |
| 32 | Postfach-Kennung des Einladenden |
| 32 | Sendetoken dafür |
| 32 | öffentlicher Kontaktschlüssel des Einladenden |
| 32 | Gutschein, nur wenn Bit 0 gesetzt |
| 1 | Länge der Relay-Adresse |
| höchstens 120 | Relay-Adresse |

Base32 in Großbuchstaben passt in den alphanumerischen Modus von QR-Codes und bleibt als Text abtippbar. Beim Einlesen sind Kleinbuchstaben, Leerzeichen und Zeilenumbrüche zu dulden.

Der Einladende nimmt je Einladung genau ein `hello` an, und nur, wenn es vor der Ablaufzeit abgeschickt wurde (Vorgabe: 10 Minuten nach dem Erzeugen). Maßgeblich ist die Absendezeit im `hello`, nicht der Zeitpunkt des Abholens; abholen kann der Einladende, solange das Zeitfenster gegen Wiedereinspielen es erlaubt, also bis gut 24 Stunden nach der Ablaufzeit. Erst danach löscht er Einladung und Postfach.

### Prüfcode

Beide Seiten zeigen sechs Ziffern:

```
h    = SHA-256("lebenszeichen-v1-code" || kleinerer_schlüssel || größerer_schlüssel)
code = (erste 4 Byte von h als Zahl) mod 1000000, mit führenden Nullen
```

„Kleiner“ und „größer“ meint den byteweisen Vergleich der beiden öffentlichen Kontaktschlüssel.

## Regeln für Anfragen

Der Angefragte prüft jede eingehende `request` gegen seine Regeln für diesen Kontakt, in dieser Reihenfolge. Trifft eine Bedingung zu, wird die Anfrage still verworfen.

1. Es gibt eine offene Anfrage dieses Kontakts. Offen heißt: angenommen, noch nicht manuell beantwortet und noch nicht abgelaufen.
2. Seit der letzten manuellen Antwort an diesen Kontakt ist weniger als der Mindestabstand vergangen.
3. In den letzten 7 × 24 Stunden wurden bereits so viele Anfragen dieses Kontakts angenommen, wie das Wochenlimit erlaubt.

Sonst wird die Anfrage angenommen und gezählt. Eine gemeldete Antwort schließt die Anfrage nicht; sie bleibt offen, bis sie manuell beantwortet wird oder abläuft.

Zeitpunkt einer Anfrage ist ihre Absendezeit, höchstens aber die eigene Uhrzeit beim Empfang. Ablaufzeit und Wochenlimit rechnen ab diesem Zeitpunkt, damit beide Seiten denselben Moment zählen, auch wenn die Zustellung sich verzögert hat. Eine Anfrage, die erst nach ihrer Ablaufzeit ankommt, wird verworfen und nicht gezählt.

Eine Antwort gilt, wenn sie abgeschickt wurde, solange die Anfrage noch nicht abgelaufen war, auch wenn sie später ankommt. Je Anfrage nimmt der Anfragende höchstens eine gemeldete und eine manuelle Antwort an; nach einer manuellen wird keine gemeldete mehr angenommen.

Der Anfragende wendet dieselbe Prüfung mit den Regeln der Gegenseite auf seine eigenen gesendeten Anfragen an, um den Knopf zu sperren und den nächsten möglichen Zeitpunkt anzuzeigen. Maßgeblich bleibt die Prüfung beim Angefragten.

Vorgaben: Mindestabstand 360 Minuten, Ablaufzeit 1440 Minuten, Wochenlimit 7. Nach `hello_ok` schickt jede Seite einmal `rules` und erneut nach jeder Änderung. Bis die Regeln der Gegenseite eingetroffen sind, gelten die Vorgaben.

## Relay-Schnittstelle

HTTPS und WebSocket unter dem Pfad `/v1`. Token stehen im Kopf `Authorization: Bearer <b64url>`. Kennungen in Pfaden und JSON sind `b64url`. Fehler haben einen JSON-Rumpf `{"error":"<code>"}`.

| Aufruf | Berechtigung | Anfrage | Antwort |
|---|---|---|---|
| `GET /v1/health` | – | – | `200` |
| `POST /v1/vouchers` | Zugangstoken | – | `201 {"voucher":"…","expires_in":600}` |
| `POST /v1/mailboxes` | Zugangstoken oder Gutschein | `{"read_token_hash":"<hex>","send_token_hash":"<hex>"}` | `201 {"mailbox":"…"}` |
| `DELETE /v1/mailboxes/{mailbox}` | Lesetoken | – | `204` |
| `PUT /v1/mailboxes/{mailbox}/readers/{reader}` | Lesetoken | `{"push_url":"https://…"}` oder `{"push_url":null}` | `204` |
| `DELETE /v1/mailboxes/{mailbox}/readers/{reader}` | Lesetoken | – | `204` |
| `POST /v1/mailboxes/{mailbox}/messages` | Sendetoken | Rumpf: die 297 Byte des Umschlags, `application/octet-stream` | `202` |

Fehlercodes:

| Status | `error` | Bedeutung |
|---|---|---|
| 400 | `bad_request` | Anfrage nicht lesbar |
| 401 | `unauthorized` | Token fehlt oder passt nicht |
| 404 | `unknown_mailbox` | Das Postfach gibt es nicht oder nicht mehr. Daran erkennt ein Client, dass er entfernt wurde. |
| 404 | `unknown_reader` | Leser nicht eingetragen |
| 413 | `too_large` | Rumpf länger als 512 Byte |
| 429 | `mailbox_full` | Schon 8 wartende Nachrichten |
| 429 | `rate_limited` | Mehr als 20 Einlieferungen in der letzten Stunde |

Ein Gutschein gilt 10 Minuten und für genau ein Postfach. Das Relay nimmt beliebige Rümpfe bis 512 Byte an und prüft ihren Inhalt nicht.

### Zustellung

`GET /v1/stream` öffnet einen WebSocket. Alle Rahmen sind Text mit einem JSON-Objekt.

Vom Client:

```json
{"op":"subscribe","mailbox":"…","token":"<Lesetoken>","reader":"…"}
{"op":"ack","mailbox":"…","seq":17}
```

Vom Relay:

```json
{"op":"subscribed","mailbox":"…"}
{"op":"message","mailbox":"…","seq":17,"data":"<Umschlag als b64url>"}
{"op":"error","mailbox":"…","code":"unknown_mailbox"}
```

- Eine Verbindung kann beliebig viele Postfächer abonnieren. Der Leser muss vorher eingetragen sein.
- `seq` zählt je Postfach aufwärts und beginnt bei 1. Nach `subscribed` schickt das Relay alle wartenden Nachrichten mit `seq` über der letzten Quittung dieses Lesers, danach neue, sobald sie eintreffen.
- `ack` quittiert alle Nachrichten dieses Postfachs bis einschließlich `seq`. Ein Client quittiert erst, wenn er die Nachricht dauerhaft gespeichert oder verarbeitet hat.
- Das Relay löscht eine Nachricht, sobald alle eingetragenen Leser sie quittiert haben, spätestens 24 Stunden nach dem Eingang. Hat ein Postfach keinen Leser, bleibt sie bis zu diesen 24 Stunden liegen.
- Postfächer ohne Abruf seit 90 Tagen werden gelöscht.

### Wecksignal

Nimmt das Relay eine Nachricht an, sendet es an die `push_url` jedes Lesers, der dieses Postfach gerade nicht abonniert hat, ein `POST` mit dem festen Rumpf `wake`. Der Rumpf trägt keine Information; ein leerer Rumpf wird von manchen Verteilern nicht zugestellt. Fehlschläge werden nicht wiederholt und nicht gemeldet.

Weil ein Client damit das Relay zu Aufrufen an fremde Adressen veranlassen kann, gilt: nur `https`, keine privaten oder lokalen Zieladressen, keine Weiterleitungen, kurze Zeitgrenze. Der Betreiber kann die erlaubten Rechnernamen auf eine Liste beschränken und sollte das tun, etwa auf den eigenen ntfy-Server.

### Betrieb

Einstellungen über Umgebungsvariablen:

| Variable | Bedeutung | Vorgabe |
|---|---|---|
| `LZ_ACCESS_TOKEN` | Zugangstoken als `b64url` von 32 Byte; Pflicht | – |
| `LZ_LISTEN` | Adresse und Port | `0.0.0.0:8080` |
| `LZ_DB` | Pfad der SQLite-Datei | `/data/relay.sqlite` |
| `LZ_PUSH_ALLOWED_HOSTS` | erlaubte Rechnernamen für Wecksignale, durch Komma getrennt; leer heißt alle öffentlichen | leer |

Das Relay schreibt keine IP-Adressen und keine Postfach-Kennungen in seine Ausgabe. TLS übernimmt der Reverse-Proxy davor.

## Festlegungen dieses Dokuments

Diese Punkte gehen über `protokoll.md` hinaus und sind neu:

- **Festes Binärformat** statt JSON oder CBOR im Klartext: klein, ohne Parser und auf Microcontrollern einfach.
- **Versiegelte Form für `hello`**, weil der Empfänger den Absender noch nicht kennt.
- **Prüfwert vor dem Chiffrat**, wie libsodium ihn ausgibt. Die erste Fassung dieses Dokuments hatte ihn dahinter; berichtigt am 14.11.2026, bevor Code darauf aufbaute.
- **Einladung als Base32 mit Vorsatz `LZ1-`**; damit ist die offene Frage zum Format aus `protokoll.md` beantwortet.
- **Prüfcode über SHA-256.**
- **Mindestabstand zählt ab der manuellen Antwort**; eine gemeldete Antwort schließt die Anfrage nicht.
- **Annahme einer Einladung zählt nach der Absendezeit des `hello`**, damit der Einladende nicht innerhalb von 10 Minuten abholen muss. Preis: Die Absendezeit setzt der Absender selbst. Wer einen Code abfängt, kann ein zurückdatiertes `hello` schicken; ein abgefangener Code bleibt so bis zu gut 24 Stunden statt 10 Minuten nutzbar. Er gilt weiterhin nur einmal, und der Prüfcode zeigt, dass der falsche Kontakt angenommen hat.
- **Zeitpunkt einer Anfrage ist ihre Absendezeit**, begrenzt durch die Uhr des Empfängers; verspätet angekommene Anfragen und rechtzeitig abgeschickte Antworten werden danach beurteilt.
- **Zeitfenster gegen Wiedereinspielen** von 24 Stunden plus 10 Minuten Toleranz. Ein Gerät mit stark falscher Uhr kann deshalb nicht teilnehmen.
- **Relay: JSON für Steuerung, rohe Bytes für Nachrichten, kumulative Quittungen.**
- **Wecksignal nur an Leser ohne offene Verbindung**, mit festem Rumpf `wake`.
- **Schutz des Relays vor missbräuchlichen Push-Adressen.**
- **SQLite als Speicher des Relays.** Der Kern spricht seinen Speicher über eine Schnittstelle an und speichert in einer JSON-Datei je Gerät (siehe `konzept.md`).

Klarstellungen, die bei der Umsetzung des Relays (`crates/relay`) getroffen wurden:

- **Reihenfolge der Prüfungen:** Fehlt der Token oder ist er nicht `b64url` von 32 Byte, kommt `401`. Danach wird das Postfach gesucht; ein unbekanntes Postfach meldet `404 unknown_mailbox` auch dann, wenn der Token nicht passen würde, damit ein entfernter Client das sicher erkennt. Erst dann wird der Token verglichen (`401`).
- **Unlesbare Kennungen** in Pfaden (nicht `b64url` oder falsche Länge) ergeben `400 bad_request`. Hashes in `POST /v1/mailboxes` müssen 64 Hex-Ziffern sein; Großbuchstaben werden geduldet.
- **Gutschein:** Er steht wie ein Token im Kopf `Authorization: Bearer …`. Ein unbekannter, eingelöster oder abgelaufener Gutschein ergibt `401`. Er gilt genau 600 Sekunden.
- **Leerer Rumpf** bei `POST …/messages` ergibt `400 bad_request`. Der `Content-Type` wird nicht geprüft.
- **Grenzen gleichzeitig überschritten:** Die Ratenbegrenzung wird vor dem Füllstand geprüft, es kommt dann `rate_limited`. Abgelehnte Einlieferungen zählen nicht mit. „Wartend“ heißt: noch gespeichert, auch wenn einzelne Leser sie schon quittiert haben.
- **Ratenbegrenzung nur im Speicher:** Die Eingangszeiten für die Grenze von 6 je Stunde hält das Relay nur im Arbeitsspeicher und nur eine Stunde lang. Nach einem Neustart beginnt die Zählung neu.
- **Fehler des Relays selbst** melden `500` mit `{"error":"internal"}`.
- **Leser:** Höchstens 16 Leser je Postfach, darüber `400 bad_request`, weil jeder Leser ein Wecksignal auslösen kann. Ein neu eingetragener Leser beginnt mit Quittung 0 und erhält alle wartenden Nachrichten; ein erneutes `PUT` ändert nur die Push-Adresse. Ein Rumpf `{}` gilt wie `{"push_url":null}`. Wird ein Leser ausgetragen, löscht das Relay sofort die Nachrichten, die alle verbleibenden Leser quittiert haben.
- **Push-Adresse:** höchstens 1024 Byte, nur `https`, ohne Benutzername und Passwort, nicht `localhost`, als IP-Adresse nur öffentliche. Das Relay löst den Rechnernamen beim Wecken selbst auf und verbindet sich nur mit den geprüften öffentlichen Adressen (IPv4 und IPv6), damit eine zweite Auflösung nichts anderes liefern kann. Kein Proxy, keine Weiterleitungen, Zeitgrenze 5 Sekunden. `LZ_PUSH_ALLOWED_HOSTS` vergleicht den Rechnernamen genau, ohne Groß- und Kleinschreibung und ohne Unterdomänen. Ungültige Adressen ergeben beim Eintragen `400 bad_request`.
- **Tag der letzten Nutzung:** Ihn setzen das Anlegen des Postfachs, das Eintragen eines Lesers, `subscribe` und `ack`, nicht aber das Einliefern. Gelöscht wird ein Postfach, dessen Tag mehr als 90 Tage zurückliegt. Das Relay räumt alle 5 Minuten auf.
- **Quittungen:** `seq` wird nie wiederverwendet, auch nicht nach dem Löschen von Nachrichten. Eine Quittung über die höchste vergebene `seq` hinaus wird auf diese begrenzt, damit sie keine künftigen Nachrichten erfasst. Auf `ack` antwortet das Relay nicht, außer mit einem Fehler.
- **WebSocket-Fehler:** `code` ist einer der Fehlercodes oben (`bad_request`, `unauthorized`, `unknown_mailbox`, `unknown_reader`, `internal`). Bei einem unlesbaren Rahmen fehlt das Feld `mailbox`. `ack` für ein Postfach, das diese Verbindung nicht abonniert hat, ergibt `bad_request`. Wird ein abonniertes Postfach gelöscht oder der Leser ausgetragen, schickt das Relay `unknown_mailbox` bzw. `unknown_reader` und beendet das Abonnement. Ein erneutes `subscribe` derselben Verbindung auf dasselbe Postfach ersetzt das alte Abonnement. Höchstens 256 Postfächer je Verbindung, Rahmen höchstens 4 KiB.

Nicht festgelegt: die Nachrichten zwischen eigenen Geräten (M5) und das Verhalten beim Wechsel des Relays.
