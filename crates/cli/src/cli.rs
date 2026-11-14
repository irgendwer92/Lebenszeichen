//! Command line arguments and dispatch to the commands in [`lz_client::ctx`].

use std::path::PathBuf;
use std::time::Duration;

use clap::{ArgAction, CommandFactory, FromArgMatches, Parser, Subcommand};
use lz_core::store::JsonFile;

use lz_client::ctx::{self, Clock, Ctx, Result};
use lz_client::relay::Http;

/// How long `lz sync` waits for further messages before it ends.
const SYNC_IDLE: Duration = Duration::from_millis(1500);

const HELP_TEMPLATE: &str = "\
{about-with-newline}
Aufruf: {usage}

{all-args}{after-help}";

#[derive(Parser)]
#[command(
    name = "lz",
    version,
    about = "Lebenszeichen auf der Kommandozeile",
    help_template = HELP_TEMPLATE,
    subcommand_help_heading = "Befehle",
    next_help_heading = "Optionen",
    disable_help_flag = true,
    disable_version_flag = true,
    disable_help_subcommand = true
)]
struct Args {
    /// Zustandsdatei des Geräts [Vorgabe: $XDG_DATA_HOME/lebenszeichen/device.json]
    #[arg(long, env = "LZ_STATE", global = true, value_name = "PFAD")]
    state: Option<PathBuf>,

    /// Auch verworfene Nachrichten anzeigen
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Hilfe anzeigen
    #[arg(short, long, action = ArgAction::Help, global = true)]
    help: Option<bool>,

    /// Version anzeigen
    #[arg(short = 'V', long, action = ArgAction::Version)]
    version: Option<bool>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Legt die Zustandsdatei mit einem neuen Gerät an
    Init {
        /// Basisadresse des eigenen Relays, etwa https://relay.example.org
        #[arg(long, value_name = "URL")]
        relay: Option<String>,
        /// Zugangstoken des Relays (b64url); nötig zum Einladen
        #[arg(long, requires = "relay", value_name = "TOKEN")]
        access_token: Option<String>,
    },
    /// Lädt jemanden ein und zeigt den Einladungscode (10 Minuten gültig)
    Invite {
        /// Name, unter dem der Kontakt bei dir erscheint
        name: String,
        /// Gutschein beilegen, damit die Gegenseite ein Postfach auf deinem Relay anlegen kann
        #[arg(long)]
        voucher: bool,
        /// Den Code zusätzlich als QR-Code zeigen (für ein dunkles Terminal)
        #[arg(long)]
        qr: bool,
    },
    /// Nimmt eine Einladung an und zeigt den Prüfcode
    Accept {
        /// Name, unter dem der Kontakt bei dir erscheint
        name: String,
        /// Einladungscode LZ1-…
        code: String,
    },
    /// Bestätigt einen Kontakt, der die eigene Einladung angenommen hat
    Confirm {
        /// Name des Kontakts
        name: String,
    },
    /// Lehnt einen Kontakt ab, dessen Prüfcode nicht übereinstimmt (für die Gegenseite sichtbar)
    Reject {
        /// Name des Kontakts
        name: String,
    },
    /// Holt alle wartenden Nachrichten ab und endet dann
    Sync,
    /// Bleibt verbunden und zeigt Nachrichten, sobald sie eintreffen
    Listen,
    /// Listet Kontakte und offene Einladungen
    Contacts,
    /// Fragt nach einem Lebenszeichen
    Request {
        /// Name des Kontakts
        name: String,
    },
    /// Beantwortet die offene Anfrage
    Respond {
        /// Name des Kontakts
        name: String,
    },
    /// Meldet einen Zustand zur offenen Anfrage, etwa „schläft“
    Report {
        /// Name des Kontakts
        name: String,
        /// Kurzer Grund, höchstens 64 Byte
        reason: String,
    },
    /// Zeigt oder ändert, wie oft der Kontakt dich fragen darf
    Rules {
        /// Name des Kontakts
        name: String,
        /// Mindestabstand nach einer Antwort in Minuten
        #[arg(long, value_name = "N")]
        interval_min: Option<u32>,
        /// Ablaufzeit einer unbeantworteten Anfrage in Minuten
        #[arg(long, value_name = "N")]
        expiry_min: Option<u32>,
        /// Höchstzahl Anfragen in sieben Tagen
        #[arg(long, value_name = "N")]
        weekly: Option<u16>,
    },
    /// Entfernt einen Kontakt (für die Gegenseite sichtbar) oder zieht eine Einladung zurück
    Remove {
        /// Name des Kontakts
        name: String,
    },
}

/// Parses the arguments, with German headings in the help of subcommands.
fn parse() -> Args {
    let command = Args::command().mut_subcommands(|sub| {
        sub.help_template(HELP_TEMPLATE).mut_args(|arg| {
            let heading = if arg.is_positional() {
                "Argumente"
            } else {
                "Optionen"
            };
            arg.help_heading(heading)
        })
    });
    Args::from_arg_matches(&command.get_matches()).unwrap_or_else(|error| error.exit())
}

/// Parses the arguments and runs the command.
pub async fn run() -> Result<()> {
    let args = parse();
    let path = match args.state {
        Some(path) => path,
        None => ctx::default_state_path()?,
    };
    let store = JsonFile::new(path);
    if let Command::Init {
        relay,
        access_token,
    } = &args.command
    {
        return ctx::init(
            &store,
            relay.as_deref(),
            access_token.as_deref(),
            &mut std::io::stdout(),
        );
    }
    let mut ctx = Ctx {
        store,
        relay: Http::new()?,
        clock: Clock::System,
        out: Box::new(std::io::stdout()),
        verbose: args.verbose,
        idle: SYNC_IDLE,
        hints: crate::hints::cli,
    };
    match args.command {
        Command::Init { .. } => unreachable!("handled above"),
        Command::Invite { name, voucher, qr } => ctx.invite(&name, voucher).await.map(|code| {
            if qr {
                println!("\n{}", lz_client::qr::terminal(&code));
            }
        }),
        Command::Accept { name, code } => ctx.accept(&name, &code).await.map(drop),
        Command::Confirm { name } => ctx.confirm(&name).await,
        Command::Reject { name } => ctx.reject(&name).await,
        Command::Sync => ctx.sync().await.map(drop),
        Command::Listen => ctx.listen().await,
        Command::Contacts => ctx.contacts(),
        Command::Request { name } => ctx.request(&name).await,
        Command::Respond { name } => ctx.respond(&name).await,
        Command::Report { name, reason } => ctx.report(&name, &reason).await,
        Command::Rules {
            name,
            interval_min,
            expiry_min,
            weekly,
        } => ctx.rules(&name, interval_min, expiry_min, weekly).await,
        Command::Remove { name } => ctx.remove(&name).await,
    }
}
