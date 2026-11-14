# Lebenszeichen – Protokoll (Entwurf 2)

Stand: 13.11.2026. Gehört zu `konzept.md`.

Entschieden am 13.11.2026: mehrere Geräte von Anfang an, Regeln für Anfragen samt Wochenlimit, Verzicht auf Forward Secrecy, sichtbares Entfernen. Der Rest ist Vorschlag.

## Grundideen

1. **Ein Postfach pro Kontakt und Richtung.** Es gibt keine globale Kennung für eine Person. Wer einen Kontakt entfernt, löscht dessen Postfach; damit ist der Kontakt technisch ausgesperrt.
2. **Ein Schlüsselpaar pro Kontakt.** Zwei Kontakte derselben Person können nicht durch Schlüsselvergleich feststellen, dass sie dieselbe Person kennen.
3. **Das Relay ist dumm.** Es nimmt Chiffrat an, reicht es weiter und löscht es. Es kennt weder Namen noch Schlüssel noch Nachrichtentypen.
4. **Alle Nachrichten sind gleich groß.** Jede Nachricht wird auf 256 Byte aufgefüllt, damit das Relay Anfrage und Antwort nicht an der Länge unterscheiden kann.

## Begriffe

| Begriff | Bedeutung |
|---|---|
| Postfach | Warteschlange auf einem Relay mit zufälliger Kennung (32 Byte) |
| Lesetoken | Geheimnis, mit dem der Besitzer sein Postfach abruft und löscht |
| Sendetoken | Geheimnis, mit dem genau ein Kontakt in das Postfach einliefern darf |
| Leser | Ein Gerät des Postfach-Besitzers, mit eigener Kennung und Push-Adresse |
| Kontaktschlüssel | X25519-Schlüsselpaar, das nur für einen Kontakt gilt |

## Kopplung

B lädt A ein. Danach sind beide gleichberechtigt und können sich gegenseitig anfragen.

1. **B erzeugt eine Einladung.** Die App legt ein Postfach auf Bs Relay an, erzeugt einen Kontaktschlüssel und zeigt einen QR-Code (alternativ als Textcode zum Verschicken). Inhalt:
   - Protokollversion
   - Relay-Adresse, Postfach-Kennung und Sendetoken von Bs Postfach
   - Bs öffentlicher Kontaktschlüssel
   - optional ein Gutschein, mit dem A einmalig ein Postfach auf Bs Relay anlegen darf

   Die Einladung gilt 10 Minuten und nur einmal.
2. **A nimmt an.** As App erzeugt einen eigenen Kontaktschlüssel und ein eigenes Postfach, auf dem eigenen Relay oder per Gutschein auf Bs Relay. Sie schickt `hallo` an Bs Postfach: As öffentlicher Schlüssel sowie Adresse und Sendetoken von As Postfach.
3. **Beide sehen einen Prüfcode.** Sechs Ziffern, abgeleitet aus beiden öffentlichen Schlüsseln. Wurde der QR-Code direkt vom Bildschirm gescannt, ist der Vergleich überflüssig. Wurde die Einladung verschickt, schützt er vor einem Angreifer, der sie unterwegs ausgetauscht hat.
4. **B bestätigt** und schickt `hallo-ok`. Beide vergeben lokal einen Namen für den Kontakt und tauschen ihre `regeln` aus.

Namen werden nie übertragen. Jede Seite benennt ihre Kontakte selbst.

## Kontakt entfernen

Das Entfernen ist für die Gegenseite sichtbar (entschieden).

1. Bs App schickt `ende` an A und löscht danach das eigene Postfach für diesen Kontakt.
2. As App zeigt "B hat die Verbindung beendet", sperrt den Anfrage-Knopf und löscht ihr eigenes Postfach für B.
3. Geht `ende` verloren, erkennt As App das Entfernen spätestens bei der nächsten Anfrage: Das Relay meldet, dass das Postfach nicht mehr existiert.

Offene Anfragen in beide Richtungen verfallen dabei.

## Verschlüsselung

- `crypto_box` aus libsodium (X25519, XSalsa20, Poly1305) zwischen den beiden Kontaktschlüsseln, mit zufälliger Nonce pro Nachricht.
- Aufbau einer Nachricht auf dem Draht: Versionsbyte, Nonce (24 Byte), Chiffrat. Der Klartext wird vor dem Verschlüsseln auf feste Länge aufgefüllt.
- Schutz vor Wiedereinspielen: Jede Nachricht trägt Zeitstempel und zufällige Kennung; der Empfänger merkt sich Kennungen bis zum Ablauf und verwirft Doppelte und Abgelaufene.

**Keine Forward Secrecy (entschieden).** Es gibt kein Ratchet. Wer später einen Kontaktschlüssel erbeutet, kann alte mitgeschnittene Nachrichten dieses Kontakts entschlüsseln. Deren Inhalt ist "Anfrage", "✓" oder ein kurzer Grundtext. Dafür bleibt das Protokoll klein, und mehrere Geräte können denselben Schlüssel nutzen. Abhilfe bei Verdacht: Kontakt neu koppeln.

## Nachrichtentypen

### Zwischen Kontakten

| Typ | Felder | Zweck |
|---|---|---|
| `hallo` | öffentlicher Schlüssel, Postfach-Adresse, Sendetoken | Kopplung annehmen |
| `hallo-ok` | – | Kopplung bestätigen |
| `regeln` | Mindestabstand, Ablaufzeit, Wochenlimit | Der Angefragte teilt mit, wie oft er gefragt werden darf |
| `anfrage` | Kennung, Zeit | "Lebenszeichen?" |
| `antwort` | Kennung der Anfrage, Art (`manuell` oder `gemeldet`), Grund (nur bei `gemeldet`), Zeit | Das Lebenszeichen |
| `ende` | – | Kontakt wurde entfernt |

Clients ignorieren unbekannte Typen und unbekannte Felder.

Es gibt bewusst **keine** Zustell- oder Lesebestätigung. A erfährt nur, dass das Relay die Anfrage angenommen hat.

### Zwischen eigenen Geräten

Eigene Geräte teilen sich ein Abgleich-Postfach und einen symmetrischen Gruppenschlüssel.

| Typ | Felder | Zweck |
|---|---|---|
| `erledigt` | Kennung der Anfrage | Benachrichtigung auf den anderen Geräten zurückziehen |
| `kontakt` | hinzugefügt, geändert oder entfernt; Kontaktdaten und Regeln | Kontaktliste abgleichen |
| `gezählt` | Kontakt, Kennung und Zeit einer angenommenen Anfrage | Zähler für Mindestabstand und Wochenlimit auf allen Geräten gleich halten |

## Regeln für Anfragen (entschieden)

Die App soll kein Überwachungswerkzeug werden. Der Angefragte bestimmt die Regeln pro Kontakt und setzt sie auf seinen Geräten durch. Das Gerät des Anfragenden kennt sie aus `regeln` und sperrt den Knopf entsprechend ("wieder möglich ab 18:00").

| Regel | Vorgabe | Einstellbar |
|---|---|---|
| Offene Anfragen pro Kontakt | höchstens eine | nein |
| Ablaufzeit einer unbeantworteten Anfrage | 24 Stunden | ja |
| Mindestabstand nach einer Antwort | 6 Stunden | ja |
| Anfragen pro Woche | höchstens 7 | ja |
| Wartezeit vor gemeldeter Antwort | 15 Minuten | ja, nur lokal |

Zum Wochenlimit:
- Gezählt wird über die letzten 7 Tage gleitend, nicht pro Kalenderwoche.
- Jede angenommene Anfrage zählt, auch wenn sie unbeantwortet abläuft.
- Die Regeln gelten je Kontakt. Ein Limit über alle Kontakte hinweg gibt es nicht.

Anfragen, die gegen die Regeln verstoßen, verwirft das empfangende Gerät still. Bei einem unveränderten Client kommt das nicht vor, weil der Knopf gesperrt ist.

Auswertung von Antworten beim Anfragenden:
- Pro Anfrage wird höchstens eine `gemeldet`-Antwort angezeigt.
- Eine `manuell`-Antwort ersetzt eine frühere `gemeldet`-Antwort.
- Antworten auf abgelaufene oder unbekannte Anfragen werden verworfen.

## Mehrere Geräte (entschieden: von Anfang an)

- Alle Geräte einer Person teilen sich Kontaktschlüssel und Postfächer. Kontakte sehen nicht, wie viele Geräte jemand hat.
- Ein neues Gerät wird per QR-Code vom vorhandenen Gerät eingerichtet und übernimmt Kontaktliste, Regeln und Gruppenschlüssel.
- Jedes Gerät ist ein eigener Leser; das Relay hält Nachrichten, bis alle Leser quittiert haben.
- Reagiert die Person an einem Gerät, schickt es `erledigt` an die anderen.
- Eine Zustandsmeldung gilt nur auf dem Gerät, das sie erhalten hat. Dieses Gerät sendet nach der Wartezeit die `gemeldet`-Antwort.
- Ein Gerät wird entfernt, indem ein anderes es als Leser austrägt. Bei Verlust eines Geräts müssen die Kontakte neu gekoppelt werden, weil es alle Kontaktschlüssel kennt.

Nachteil: Wird ein Gerät kompromittiert, sind alle Kontaktschlüssel betroffen.

## Relay

### Schnittstelle

| Aufruf | Berechtigung | Wirkung |
|---|---|---|
| Postfach anlegen | Zugangstoken des Relays oder Gutschein | Liefert Postfach-Kennung; der Client übergibt die Hashes von Lese- und Sendetoken |
| Gutschein erzeugen | Zugangstoken | Einmal gültig, für genau ein Postfach |
| Nachricht einliefern | Sendetoken | Höchstens 512 Byte; meldet eindeutig, wenn das Postfach nicht mehr existiert |
| Leser eintragen, ändern oder austragen | Lesetoken | Leser-Kennung und Push-Adresse |
| Nachrichten abrufen (WebSocket) | Lesetoken, mehrere Postfächer pro Verbindung | Zustellung; der Leser quittiert jede Nachricht |
| Postfach löschen | Lesetoken | Sofort, samt Inhalt |

Bei einer neuen Nachricht sendet das Relay an die Push-Adresse jedes Lesers ein inhaltsloses Wecksignal (UnifiedPush). Das Gerät verbindet sich dann selbst und holt ab.

### Was das Relay speichert

| Daten | Dauer |
|---|---|
| Postfach-Kennung, Hash von Lese- und Sendetoken | bis zum Löschen des Postfachs |
| Leser je Postfach: Kennung, Push-Adresse, letzte Quittung | bis zum Löschen des Postfachs oder Lesers |
| Nachrichten: Chiffrat und Eingangszeit | bis alle Leser quittiert haben, höchstens 24 Stunden |
| Offene Gutscheine (Hash) | bis zur Einlösung, höchstens 10 Minuten |
| Tag der letzten Nutzung je Postfach | für die Aufräumregel |

### Was das Relay nicht speichert

- Namen, Schlüssel, Klartext, Nachrichtentypen
- IP-Adressen und Zugriffsprotokolle (muss auch im Reverse-Proxy abgeschaltet werden)
- zugestellte Nachrichten oder irgendeinen Verlauf
- welche Postfächer zusammengehören

### Was der Betreiber trotzdem sehen kann

Auch ohne Inhalt bleiben Metadaten, wenn jemand das Relay aktiv beobachtet:

- **Zeitpunkte:** wann in welches Postfach eingeliefert wird.
- **Zusammengehörigkeit:** Postfächer, die über dieselbe Verbindung abgerufen werden oder dieselbe Push-Adresse haben, gehören erkennbar zum selben Gerät. Auf eine Einlieferung in Bs Postfach folgt oft eine in As Postfach; daraus lässt sich ein Kontaktpaar ableiten.
- **IP-Adressen** während der Verbindung.

Bei einem selbst betriebenen Relay ist der Betreiber eine Vertrauensperson. Für eine spätere Veröffentlichung mit fremden Relays wäre das der wichtigste Punkt zum Nachschärfen.

Der Push-Dienst (ntfy) sieht nur, wann ein Gerät geweckt wird.

### Schutz vor Missbrauch

- Postfächer nur mit Zugangstoken oder Gutschein.
- Höchstens 8 wartende Nachrichten und 20 Einlieferungen pro Stunde je Postfach (entschieden am 14.11.2026; zuvor 6, was bei Kopplung, Regeländerungen und Antworten schon im normalen Gebrauch das `ende` blockieren konnte). Die Grenze zählt alle Nachrichten, weil das Relay die Typen nicht kennt; sie ist kein Ersatz für die Regeln für Anfragen.
- Postfächer ohne Abruf seit 90 Tagen werden gelöscht.

Diese Grenzen schützen nur das Relay. Die Regeln für Anfragen setzt das Gerät des Angefragten durch, weil das Relay die Nachrichtentypen nicht kennt.

## Offene Fragen

- Genaues Format der Einladung (Kodierung, Länge des Textcodes).
- Verhalten, wenn ein Kontakt das Relay wechselt.
- Wiederherstellung nach Verlust aller Geräte (derzeit: alle Kontakte neu koppeln).
