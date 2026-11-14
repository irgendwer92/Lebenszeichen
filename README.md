# Lebenszeichen

Manche Menschen schaffen es nicht, auf Nachrichten zu antworten, wenn es ihnen
nicht gut geht. Andere machen sich dann Sorgen. Lebenszeichen fragt auf Wunsch
ein minimales Lebenszeichen ab: eine Anfrage, eine einzige mögliche Antwort,
und Wegklicken zählt.

Kein Chat, kein Onlinestatus, kein Zeitplan, keine Alarmkette.

## Stand

Meilenstein M1 in Arbeit: Ein Wegwerf-Prototyp für Android prüft, ob ein
Wecksignal die beendete App erreicht und die Kotlin-Schicht den Rust-Kern
aufrufen kann. Der Test auf einem Gerät ist bestanden, ein Lauf über eine ganze
Nacht steht noch aus; siehe [docs/m1-durchstich.md](docs/m1-durchstich.md).

Meilenstein M2 in Arbeit: `crates/wire` setzt das Drahtformat aus
[docs/drahtformat.md](docs/drahtformat.md) um (Klartext der Nachrichten,
Einladung, Prüfung des Umschlags), `crates/core` Verschlüsselung, Kopplung,
die Regeln für Anfragen und den Speicher als JSON-Datei, `crates/cli` den
Client `lz` für die Kommandozeile (siehe unten). Das Relay (`crates/relay`,
Programm `lz-relay`) wird über Umgebungsvariablen eingestellt und lässt sich
als Docker-Image bauen: `docker build -f crates/relay/Dockerfile .`

| Meilenstein | Inhalt |
|---|---|
| M0 | Grundgerüst, CI, Renovate, Lizenzdateien |
| M1 | Durchstich auf Android |
| M2 | Kern und Relay, bedienbar über die Kommandozeile |
| M3 | Linux-Desktop |
| M4 | Android-App |
| M5 | Mehrere Geräte |
| M6 | Windows |
| M7 | Lokale Schnittstelle für Zustandsmeldungen |
| M8 | Härtung |

## Kommandozeile

`lz` ist der Client ohne Oberfläche. Jedes Gerät hat eine Zustandsdatei, wählbar
mit `--state <pfad>` oder `LZ_STATE`; Vorgabe ist
`$XDG_DATA_HOME/lebenszeichen/device.json` (sonst
`~/.local/share/lebenszeichen/device.json`).

```sh
# Einmalig: Gerät anlegen. Mit Zugangstoken kann es einladen.
lz init --relay https://relay.example.org --access-token <b64url>

# Anna lädt Bert ein und gibt ihm den Code (10 Minuten gültig).
lz invite Bert              # mit --voucher darf Bert ein Postfach auf Annas Relay anlegen
# Bert nimmt an und sieht einen sechsstelligen Prüfcode.
lz accept Anna LZ1-…
# Anna holt die Annahme ab, vergleicht den Prüfcode und bestätigt.
lz sync
lz confirm Bert
# Bert holt die Bestätigung und Annas Regeln ab.
lz sync

lz request Anna             # nach einem Lebenszeichen fragen
lz respond Bert             # die offene Anfrage beantworten
lz report Bert "schläft"    # einen Zustand melden; die Anfrage bleibt offen
lz rules Bert --interval-min 360 --expiry-min 1440 --weekly 7
lz contacts                 # Kontakte, offene Anfragen, „wieder möglich ab …“
lz listen                   # verbunden bleiben und Nachrichten sofort zeigen
lz remove Bert              # Kontakt entfernen; Bert sieht das beim nächsten Abruf
```

`lz sync` holt alles Wartende ab und endet, `lz listen` bleibt verbunden.
`--verbose` zeigt auch verworfene Nachrichten. Ein Gerät ohne Zugangstoken
kann nur Einladungen mit Gutschein annehmen. Für lokale Tests sind
`http://`-Adressen erlaubt; im Betrieb steht das Relay hinter TLS.

Die Einladung muss innerhalb von 10 Minuten angenommen werden. Abholen kann
die einladende Seite die Annahme auch später, bis zu 24 Stunden danach.

## Grundsätze

- Die App ist kein Überwachungswerkzeug. Wer angefragt wird, bestimmt, wie oft
  das geschehen darf, und das eigene Gerät setzt es durch.
- Die App ist kein Notrufsystem. Keine Antwort heißt nicht "Notfall".
- Kein Konto, keine Telefonnummer, keine E-Mail-Adresse.
- Ende-zu-Ende-verschlüsselt. Das Relay sieht nur Chiffrat fester Länge.

## Aufbau

```
crates/
├── wire/    Nachrichtenhülle und Relay-Schnittstelle (no_std)
├── core/    Protokoll, Verschlüsselung, Regelprüfung, Speicher
├── cli/     Client ohne Oberfläche (Programm: lz)
└── relay/   Relay-Server (Programm: lz-relay)
app/
├── src-tauri/   Tauri-Hülle, in M1 nur der Android-Prototyp
├── ui/          Oberfläche (schlichtes HTML ohne Build-Schritt)
└── android/     Kotlin-Schicht: UnifiedPush, Vollbild-Anzeige, Benachrichtigungen
docs/        Konzept, Protokoll, Fahrplan, Durchstich M1, Entwurf der Lizenz-Ausnahme
```

`app/` gehört nicht zum Cargo-Workspace und wird nur vom CI-Ablauf
"Android-Prototyp" gebaut. Die fertige APK liegt dort als Artefakt.

## Entwickeln

Voraussetzung ist eine aktuelle stabile Rust-Toolchain.

```sh
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check        # braucht: cargo install cargo-deny
```

Dieselben Schritte laufen bei jedem Push und Pull Request in der CI. Sie
erfassen `crates/`, nicht `app/`.
Bezeichner im Code sind englisch, die Dokumentation ist deutsch.

## Lizenz

- `crates/wire`, `crates/core`, `crates/cli` und die App: GPL-3.0-or-later,
  siehe [LICENSE](LICENSE). Eine App-Store-Ausnahme ist geplant, der Wortlaut
  liegt als Entwurf in [docs/lizenz-ausnahme.md](docs/lizenz-ausnahme.md).
- `crates/relay`: AGPL-3.0-or-later, siehe
  [crates/relay/LICENSE](crates/relay/LICENSE).
