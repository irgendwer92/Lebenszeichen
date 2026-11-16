//! Hints in the wording of the command line: which `lz` command comes next.

use lz_client::i18n::Texts;
use lz_client::show::Hint;

pub fn cli(hint: &Hint<'_>, t: Texts) -> Option<String> {
    use lz_client::i18n::Arg;
    let (id, name) = match hint {
        Hint::Init => ("hint-init", None),
        Hint::InitWithAccessToken => ("hint-init-access-token", None),
        Hint::InviteWithVoucher => ("hint-invite-voucher", None),
        Hint::Fetch => ("hint-fetch", None),
        Hint::StopListening => ("hint-stop-listening", None),
        Hint::Confirm(name) => ("hint-confirm", Some(*name)),
        Hint::Reject(name) => ("hint-reject", Some(*name)),
        Hint::Respond(name) => ("hint-respond", Some(*name)),
        Hint::Remove(name) => ("hint-remove", Some(*name)),
        Hint::Invite(name) => ("hint-invite", Some(*name)),
        Hint::InviteSomeone => ("hint-invite-someone", None),
        Hint::AddDevice => ("hint-add-device", None),
        Hint::JoinAgain => ("hint-join-again", None),
        Hint::JoinSomeDevice => ("hint-join-some-device", None),
    };
    Some(match name {
        Some(name) => t.text(id, &[("name", Arg::Text(name))]),
        None => t.text(id, &[]),
    })
}
