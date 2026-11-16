//! Hints in the wording of the command line: which `lz` command comes next.

use lz_client::show::Hint;

pub fn cli(hint: &Hint<'_>) -> Option<String> {
    Some(match hint {
        Hint::Init => "Zuerst: lz init".into(),
        Hint::InitWithAccessToken => "(lz init --relay … --access-token …)".into(),
        Hint::InviteWithVoucher => "(lz invite --voucher)".into(),
        Hint::Fetch => "Abholen mit lz sync oder lz listen.".into(),
        Hint::StopListening => "Abbruch mit Strg+C".into(),
        Hint::Confirm(name) => format!("Bestätigen mit: lz confirm {name}"),
        Hint::Reject(name) => format!("Ablehnen mit: lz reject {name}"),
        Hint::Respond(name) => format!("Antworten mit: lz respond {name}"),
        Hint::Remove(name) => format!("Entfernen mit: lz remove {name}"),
        Hint::Invite(name) => format!("Einladen mit: lz invite {name}"),
        Hint::InviteSomeone => "Einladen mit: lz invite <name>".into(),
        Hint::AddDevice => "Dort eingeben: lz device add <code> --name <name>".into(),
        Hint::JoinAgain => "Neu beginnen mit: lz device join --relay … --name …".into(),
        Hint::JoinSomeDevice => "Auf dem neuen Gerät: lz device join --relay … --name …".into(),
    })
}
