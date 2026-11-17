//! Command line arguments and dispatch to the commands in [`lz_client::ctx`].

use std::path::PathBuf;
use std::time::Duration;

use clap::{ArgAction, CommandFactory, FromArgMatches, Parser, Subcommand};
use lz_core::store::JsonFile;

use lz_client::ctx::{self, Clock, Ctx, Result};
use lz_client::i18n::{Language, Texts};
use lz_client::relay::Http;

/// How long `lz sync` waits for further messages before it ends.
const SYNC_IDLE: Duration = Duration::from_millis(1500);

// The doc comments are the English help; `localize` replaces them with the
// texts of the language of the environment (`cli-…` ids in the locale files).
#[derive(Parser)]
#[command(
    name = "lz",
    version,
    disable_help_flag = true,
    disable_version_flag = true,
    disable_help_subcommand = true
)]
struct Args {
    /// State file of the device
    #[arg(long, env = "LZ_STATE", global = true, value_name = "PATH")]
    state: Option<PathBuf>,

    /// Also show dropped messages
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Show help
    #[arg(short, long, action = ArgAction::Help, global = true)]
    help: Option<bool>,

    /// Show the version
    #[arg(short = 'V', long, action = ArgAction::Version)]
    version: Option<bool>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Creates the state file with a new device
    Init {
        /// Base address of the own relay
        #[arg(long, value_name = "URL")]
        relay: Option<String>,
        /// Access token of the relay
        #[arg(long, requires = "relay", value_name = "TOKEN")]
        access_token: Option<String>,
    },
    /// Invites someone and shows the invitation code
    Invite {
        /// Name under which the contact appears
        name: String,
        /// Attach a voucher
        #[arg(long)]
        voucher: bool,
        /// Also show the code as a QR code
        #[arg(long)]
        qr: bool,
    },
    /// Accepts an invitation
    Accept {
        /// Name under which the contact appears
        name: String,
        /// Invitation code
        code: String,
    },
    /// Confirms a contact who accepted the own invitation
    Confirm {
        /// Name of the contact
        name: String,
    },
    /// Rejects a contact whose check code does not match
    Reject {
        /// Name of the contact
        name: String,
    },
    /// Fetches all waiting messages, then ends
    Sync,
    /// Stays connected and shows messages as they arrive
    Listen {
        /// Socket of the local interface
        #[arg(long, value_name = "PATH")]
        socket: Option<PathBuf>,
    },
    /// Lists contacts and open invitations
    Contacts,
    /// Asks for a sign of life
    Request {
        /// Name of the contact
        name: String,
    },
    /// Answers the open request
    Respond {
        /// Name of the contact
        name: String,
    },
    /// Reports a state for the open request
    Report {
        /// Name of the contact
        name: String,
        /// Short reason, at most 64 bytes
        reason: String,
    },
    /// Shows or changes how often the contact may ask
    Rules {
        /// Name of the contact
        name: String,
        /// Minimum interval after a response in minutes
        #[arg(long, value_name = "N")]
        interval_min: Option<u32>,
        /// Expiry of an unanswered request in minutes
        #[arg(long, value_name = "N")]
        expiry_min: Option<u32>,
        /// Most requests in seven days
        #[arg(long, value_name = "N")]
        weekly: Option<u16>,
    },
    /// Removes a contact or withdraws an invitation
    Remove {
        /// Name of the contact
        name: String,
    },
    /// Own devices: add, list, remove
    #[command(subcommand)]
    Device(DeviceCommand),
    /// Shows the reported state, or sets, clears or allows it
    State {
        /// Socket of the local interface
        #[arg(long, global = true, value_name = "PATH")]
        socket: Option<PathBuf>,
        #[command(subcommand)]
        action: Option<StateCommand>,
    },
    /// Sets the push address
    Push {
        /// Push address
        #[arg(required_unless_present = "remove", value_name = "URL")]
        url: Option<String>,
        /// Remove the push address
        #[arg(long, conflicts_with = "url")]
        remove: bool,
    },
}

#[derive(Subcommand)]
enum StateCommand {
    /// Reports a state through the running lz listen
    Set {
        /// Hand the state on to the own devices as well
        #[arg(long)]
        share: bool,
        /// How long the state applies, in minutes
        minutes: u32,
        /// Short reason the contact sees, such as asleep
        #[arg(num_args = 1.., required = true)]
        reason: Vec<String>,
    },
    /// Ends the reported state
    Clear {
        /// End the shared state on the own devices as well
        #[arg(long)]
        share: bool,
    },
    /// Answers the contact with the reported state from this device
    Allow {
        /// Name of the contact
        name: String,
        /// Waiting time in minutes before that
        #[arg(long, value_name = "N")]
        wait: Option<u32>,
    },
    /// Never answers the contact with the reported state from this device
    Deny {
        /// Name of the contact
        name: String,
    },
}

#[derive(Subcommand)]
enum DeviceCommand {
    /// Sets this device up as a further device
    Join {
        /// Relay where the existing device has an account or mailbox
        #[arg(long, value_name = "URL")]
        relay: String,
        /// Name of this device
        #[arg(long, value_name = "NAME")]
        name: String,
    },
    /// Adds the device that shows the device code
    Add {
        /// Device code
        code: String,
        /// Name of this device as the other devices show it
        #[arg(long, value_name = "NAME")]
        name: String,
    },
    /// Lists the own devices
    List,
    /// Renames this device
    Rename {
        /// New name of this device
        name: String,
    },
    /// Removes another own device
    Remove {
        /// Name of the device
        name: String,
    },
}

/// Asks on the terminal whether a check code matches.
struct Terminal(Texts);

impl lz_client::devices::Dialog for Terminal {
    async fn confirm(&mut self, _check: lz_client::devices::Check, _code: &str) -> bool {
        print!("{} ", self.0.text("cli-confirm-prompt", &[]));
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let mut answer = String::new();
        if std::io::stdin().read_line(&mut answer).is_err() {
            return false;
        }
        is_yes(self.0, &answer)
    }
}

/// Whether an answer on the terminal means yes in the language.
fn is_yes(t: Texts, answer: &str) -> bool {
    let answer = answer.trim().to_lowercase();
    t.text("cli-confirm-yes", &[])
        .split_whitespace()
        .any(|yes| yes == answer)
}

/// Replaces the help texts of a command, its arguments and its
/// subcommands with those of the language. `path` is the names of the
/// commands above, joined by `-`; the root has none.
fn localize(command: clap::Command, t: Texts, path: &str) -> clap::Command {
    let template = format!(
        "{{about-with-newline}}\n{} {{usage}}\n\n{{all-args}}{{after-help}}",
        t.text("cli-usage", &[])
    );
    let prefix = if path.is_empty() {
        String::new()
    } else {
        format!("{path}-")
    };
    let about_id = if path.is_empty() {
        "cli-about".to_owned()
    } else {
        format!("cli-cmd-{path}")
    };
    command
        .about(t.text(&about_id, &[]))
        .help_template(template)
        .subcommand_help_heading(t.text("cli-heading-commands", &[]))
        .mut_args(|arg| {
            let id = format!(
                "cli-arg-{prefix}{}",
                arg.get_id().as_str().replace('_', "-")
            );
            let heading = if arg.is_positional() {
                "cli-heading-arguments"
            } else {
                "cli-heading-options"
            };
            let arg = arg
                .help(t.text(&id, &[]))
                .help_heading(t.text(heading, &[]));
            if arg.get_id() == "state" {
                arg.value_name(t.text("cli-value-path", &[]))
            } else {
                arg
            }
        })
        .mut_subcommands(|sub| {
            let path = format!("{prefix}{}", sub.get_name());
            localize(sub, t, &path)
        })
}

/// The command line definition with the texts of a language.
fn command(t: Texts) -> clap::Command {
    localize(Args::command(), t, "")
}

/// Parses the arguments, with the help in the language of the environment.
fn parse(t: Texts) -> Args {
    Args::from_arg_matches(&command(t).get_matches()).unwrap_or_else(|error| error.exit())
}

/// Parses the arguments and runs the command.
pub async fn run() -> Result<()> {
    let lang = Language::from_env();
    let t = lang.texts();
    let args = parse(t);
    let path = match args.state {
        Some(path) => path,
        None => ctx::default_state_path(t)?,
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
            lang,
            &mut std::io::stdout(),
        );
    }
    let mut ctx = Ctx {
        store,
        relay: Http::new().map_err(|error| ctx::Fail(error.text(t)))?,
        clock: Clock::System,
        out: Box::new(std::io::stdout()),
        verbose: args.verbose,
        idle: SYNC_IDLE,
        hints: crate::hints::cli,
        lang,
        on_notice: None,
    };
    match args.command {
        Command::Init { .. } => unreachable!("handled above"),
        Command::Invite { name, voucher, qr } => ctx.invite(&name, voucher).await.map(|code| {
            if qr {
                println!("\n{}", lz_client::qr::terminal(&code));
            }
        }),
        Command::Accept { name, code } => ctx.accept(&name, &code).await,
        Command::Confirm { name } => ctx.confirm(&name).await,
        Command::Reject { name } => ctx.reject(&name).await,
        Command::Sync => ctx.sync().await.map(drop),
        Command::Listen { socket } => listen(&mut ctx, socket).await,
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
        Command::Push { url, .. } => ctx.set_push_url(url.as_deref()).await,
        Command::State { socket, action } => match action {
            Some(StateCommand::Allow { name, wait }) => ctx.allow_reports(&name, true, wait),
            Some(StateCommand::Deny { name }) => ctx.allow_reports(&name, false, None),
            other => state_call(&mut ctx, socket, other).await,
        },
        Command::Device(DeviceCommand::Join { relay, name }) => {
            ctx.join_device(&relay, &name, None, &mut Terminal(t)).await
        }
        Command::Device(DeviceCommand::Add { code, name }) => {
            ctx.add_device(&code, &name, &mut Terminal(t)).await
        }
        Command::Device(DeviceCommand::List) => ctx.list_devices(),
        Command::Device(DeviceCommand::Rename { name }) => ctx.rename_device(&name).await,
        Command::Device(DeviceCommand::Remove { name }) => ctx.remove_device(&name).await,
    }
}

/// The socket given, or the default one of `lz`.
#[cfg(unix)]
fn socket_path(socket: Option<PathBuf>, t: Texts) -> Result<PathBuf> {
    socket
        .or_else(|| lz_client::local::default_socket_path("lz"))
        .ok_or_else(|| ctx::Fail(t.text("error-no-socket-path", &[])))
}

/// `lz listen`, with the local interface next to it where there is one.
async fn listen(ctx: &mut Ctx<Http>, socket: Option<PathBuf>) -> Result<()> {
    #[cfg(unix)]
    {
        let t = ctx.texts();
        let bound = socket_path(socket, t).and_then(|path| {
            lz_client::local::bind(&path)
                .map_err(|error| ctx::Fail(format!("{}: {error}", path.display())))
        });
        match bound {
            Ok(listener) => {
                let local = lz_client::local::Local {
                    store: ctx.store.clone(),
                    clock: Clock::System,
                    token: None,
                };
                return tokio::select! {
                    result = ctx.listen() => result,
                    result = lz_client::local::serve(listener, local) => {
                        result.map_err(|error| ctx::Fail(error.to_string()))
                    }
                };
            }
            // Fetching matters more than the interface: go on without it.
            Err(error) => eprintln!(
                "lz: {}",
                t.text(
                    "warn-no-interface",
                    &[("error", lz_client::i18n::Arg::Text(&error.0))]
                )
            ),
        }
    }
    #[cfg(not(unix))]
    let _ = socket;
    ctx.listen().await
}

/// `lz state`, `lz state set` and `lz state clear` through the socket.
async fn state_call(
    ctx: &mut Ctx<Http>,
    socket: Option<PathBuf>,
    action: Option<StateCommand>,
) -> Result<()> {
    #[cfg(unix)]
    {
        use lz_client::reports::StateCall;
        let path = socket_path(socket, ctx.texts())?;
        let reason;
        let call = match action {
            None => StateCall::Show,
            Some(StateCommand::Set {
                share,
                minutes,
                reason: words,
            }) => {
                reason = words.join(" ");
                StateCall::Set {
                    reason: &reason,
                    minutes,
                    share,
                }
            }
            Some(StateCommand::Clear { share }) => StateCall::Clear { share },
            Some(StateCommand::Allow { .. } | StateCommand::Deny { .. }) => {
                unreachable!("handled by the caller")
            }
        };
        ctx.state_call(&path, call).await
    }
    #[cfg(not(unix))]
    {
        let _ = (socket, action);
        Err(ctx::Fail(ctx.texts().text("error-no-interface-here", &[])))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk(command: &clap::Command, found: &mut Vec<String>) {
        let texts = command
            .get_about()
            .map(ToString::to_string)
            .into_iter()
            .chain(
                command
                    .get_arguments()
                    .filter_map(|a| a.get_help().map(ToString::to_string)),
            );
        found.extend(texts);
        for sub in command.get_subcommands() {
            walk(sub, found);
        }
    }

    #[test]
    fn every_help_text_is_translated() {
        for &language in Language::ALL {
            let mut found = Vec::new();
            walk(&command(language.texts()), &mut found);
            assert!(found.len() > 30);
            for text in found {
                assert!(
                    !text.starts_with("cli-") && !text.is_empty(),
                    "{}: {text}",
                    language.tag()
                );
            }
        }
    }

    #[test]
    fn answers_depend_on_the_language() {
        let (de, en) = (Language::De.texts(), Language::En.texts());
        assert!(is_yes(de, "j\n") && is_yes(de, " Ja ") && !is_yes(de, "y") && !is_yes(de, ""));
        assert!(is_yes(en, "y\n") && is_yes(en, "YES") && !is_yes(en, "j") && !is_yes(en, "n"));
    }

    #[test]
    fn help_is_in_the_language() {
        let help = |language: Language| command(language.texts()).render_help().to_string();
        assert!(help(Language::De).contains("Befehle:"));
        assert!(help(Language::En).contains("Commands:"));
    }
}
