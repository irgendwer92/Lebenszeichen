# Lebenszeichen

Manche Menschen schaffen es nicht, auf Nachrichten zu antworten, wenn es ihnen
nicht gut geht. Andere machen sich dann Sorgen. Lebenszeichen fragt auf Wunsch
ein minimales Lebenszeichen ab: eine Anfrage, eine einzige mögliche Antwort,
und Wegklicken zählt.

Kein Chat, kein Onlinestatus, kein Zeitplan, keine Alarmkette.

## Stand

Meilenstein M0: Grundgerüst. Es gibt noch keine Funktion.

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
docs/        Dokumentation
```

Die App (Tauri mit Kotlin-Schicht für Android) kommt ab M1 unter `app/` dazu.

## Entwickeln

Voraussetzung ist eine aktuelle stabile Rust-Toolchain.

```sh
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check        # braucht: cargo install cargo-deny
```

Dieselben Schritte laufen bei jedem Push und Pull Request in der CI.
Bezeichner im Code sind englisch, die Dokumentation ist deutsch.

## Lizenz

- `crates/wire`, `crates/core`, `crates/cli` und die App: GPL-3.0-or-later,
  siehe [LICENSE](LICENSE). Eine App-Store-Ausnahme ist geplant, der Wortlaut
  liegt als Entwurf in [docs/lizenz-ausnahme.md](docs/lizenz-ausnahme.md).
- `crates/relay`: AGPL-3.0-or-later, siehe
  [crates/relay/LICENSE](crates/relay/LICENSE).
