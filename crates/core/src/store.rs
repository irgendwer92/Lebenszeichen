//! What a device stores, and a store that keeps it in one JSON file.
//!
//! Exactly one process owns the state of a device. [`JsonFile`] still takes a
//! lock for every change, so that two commands of the command-line client
//! running at the same time do not lose each other's changes.
//!
//! Keys are stored unencrypted for now; the file is readable only by its
//! owner. Protection through the key store of the system follows with the
//! apps.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use rand_core::CryptoRngCore;
use serde::{Deserialize, Serialize};

use crate::contact::{ContactState, Seen};
use crate::crypto::GroupKey;
use crate::pairing::{Address, PendingInvitation};
use crate::wire::{
    ENVELOPE_LEN, MailboxId, MessageId, READER_ID_LEN, ReaderId, Reason, RelayUrl, Token,
    TransferId,
};

/// Version of the stored format. Raised on incompatible changes.
pub const FORMAT_VERSION: u32 = 1;

/// Everything one device stores.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Device {
    pub format: u32,
    /// Identifies this device as a reader of all its mailboxes.
    #[serde(with = "crate::hex")]
    pub reader_id: [u8; READER_ID_LEN],
    /// Relay on which this device creates its mailboxes.
    pub relay: Option<RelayAccount>,
    pub contacts: Vec<StoredContact>,
    pub invitations: Vec<StoredInvitation>,
    /// UnifiedPush endpoint of this device. The relay sends wake signals
    /// there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_url: Option<String>,
    /// Set while some mailbox may still have an older push address.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub push_pending: bool,
    /// Name of this device, shown on the other devices of the person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Sync mailbox shared with the other own devices, once there are any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<Group>,
    /// The other devices of the person, without this one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub devices: Vec<KnownDevice>,
    /// Envelopes for the sync mailbox the relay has not accepted yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outbox: Vec<Outgoing>,
    /// Contacts arriving from another own device, not complete yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transfers: Vec<Transfer>,
    /// Sync messages about contacts this device does not know yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parked: Vec<Parked>,
    /// Status reports of this device. Local only: never part of the
    /// handover or the group sync.
    #[serde(default, skip_serializing_if = "Reports::is_empty")]
    pub reports: Reports,
}

/// The sync mailbox of the own devices and the group key.
///
/// The same JSON form is part of the handover.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Group {
    pub relay: RelayUrl,
    #[serde(with = "crate::hex")]
    pub mailbox: MailboxId,
    #[serde(with = "crate::hex")]
    pub read_token: Token,
    #[serde(with = "crate::hex")]
    pub send_token: Token,
    pub key: GroupKey,
    /// Replay protection for the whole sync mailbox.
    pub seen: Vec<Seen>,
}

impl Group {
    /// A group for a freshly created sync mailbox, with a new group key.
    pub fn new(
        address: Address,
        read_token: Token,
        rng: &mut impl rand_core::CryptoRngCore,
    ) -> Self {
        Self {
            relay: address.relay,
            mailbox: address.mailbox,
            read_token,
            send_token: address.send_token,
            key: GroupKey::generate(rng),
            seen: Vec::new(),
        }
    }

    /// Where to deliver into the sync mailbox.
    pub fn address(&self) -> Address {
        Address {
            relay: self.relay,
            mailbox: self.mailbox,
            send_token: self.send_token,
        }
    }
}

/// Another device of the person.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownDevice {
    #[serde(with = "crate::hex")]
    pub reader: ReaderId,
    pub name: String,
}

/// An envelope waiting for delivery into the sync mailbox.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outgoing {
    #[serde(with = "crate::hex")]
    pub envelope: [u8; ENVELOPE_LEN],
    /// Dropped 24 hours after this.
    pub created_at: u64,
}

/// Parts of one transfer of a contact received so far.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transfer {
    #[serde(with = "crate::hex")]
    pub contact: MailboxId,
    #[serde(with = "crate::hex")]
    pub transfer: TransferId,
    /// Number of parts, as given by the first part.
    pub count: u8,
    /// Sorted by index, without repetitions.
    pub parts: Vec<TransferPart>,
    /// The transfer is dropped if not complete 24 hours after this.
    pub first_seen: u64,
    /// An inconsistent part arrived; later parts are ignored until the
    /// transfer is dropped.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub discarded: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferPart {
    pub index: u8,
    #[serde(with = "crate::hex::bytes")]
    pub data: Vec<u8>,
}

/// A sync message about a contact this device does not know yet, kept for
/// 24 hours in case the contact arrives.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Parked {
    #[serde(with = "crate::hex")]
    pub envelope: [u8; ENVELOPE_LEN],
    pub received_at: u64,
}

/// Access to a relay.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RelayAccount {
    pub url: RelayUrl,
    /// Allows creating mailboxes and vouchers. Without it, mailboxes can
    /// only be created with a voucher from a contact.
    #[serde(with = "crate::hex::option")]
    pub access_token: Option<Token>,
}

/// A contact with what only this device knows about it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredContact {
    /// Local name, never transmitted.
    pub name: String,
    /// Relay of the own mailbox for this contact. Usually the own relay,
    /// but the relay of the contact if the mailbox was created with a
    /// voucher from its invitation.
    pub own_relay: RelayUrl,
    /// Read token of the own mailbox for this contact.
    #[serde(with = "crate::hex")]
    pub read_token: Token,
    pub state: ContactState,
}

/// What this device does with reported states. See `report.rs`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reports {
    /// The reported state while set; may lie in the past, then unused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<ReportedState>,
    /// Per contact, only where it differs from the default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contacts: Vec<ReportSetting>,
    /// When open requests arrived on this device; the waiting time starts
    /// there.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arrivals: Vec<Arrival>,
    /// Token for reports from other apps (Android) or through a shared
    /// socket. `None`: such reports are off.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::hex::option"
    )]
    pub token: Option<Token>,
}

impl Reports {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// A state reported through the local interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportedState {
    pub reason: Reason,
    /// End of the state, Unix seconds; it applies while `now < until`.
    pub until: u64,
}

/// Whether reported answers go to a contact from this device, and after
/// which waiting time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportSetting {
    /// Own mailbox of the contact.
    #[serde(with = "crate::hex")]
    pub mailbox: MailboxId,
    pub allowed: bool,
    pub delay_minutes: u32,
}

/// When a request arrived on this device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Arrival {
    /// Own mailbox of the contact.
    #[serde(with = "crate::hex")]
    pub mailbox: MailboxId,
    #[serde(with = "crate::hex")]
    pub request: MessageId,
    pub at: u64,
}

/// An invitation handed out and not yet accepted.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredInvitation {
    /// Local name the contact will get.
    pub name: String,
    #[serde(with = "crate::hex")]
    pub read_token: Token,
    pub pending: PendingInvitation,
}

impl Device {
    /// A new device without relay and contacts.
    pub fn new(rng: &mut impl CryptoRngCore) -> Self {
        let mut reader_id = [0; READER_ID_LEN];
        rng.fill_bytes(&mut reader_id);
        Self {
            format: FORMAT_VERSION,
            reader_id,
            relay: None,
            contacts: Vec::new(),
            invitations: Vec::new(),
            push_url: None,
            push_pending: false,
            name: None,
            group: None,
            devices: Vec::new(),
            outbox: Vec::new(),
            transfers: Vec::new(),
            parked: Vec::new(),
            reports: Reports::default(),
        }
    }

    /// The contact whose own mailbox is `mailbox`, the contact identifier
    /// between own devices.
    pub fn contact_by_mailbox(&self, mailbox: &MailboxId) -> Option<&StoredContact> {
        self.contacts
            .iter()
            .find(|c| &c.state.contact.own_mailbox == mailbox)
    }

    pub fn contact_by_mailbox_mut(&mut self, mailbox: &MailboxId) -> Option<&mut StoredContact> {
        self.contacts
            .iter_mut()
            .find(|c| &c.state.contact.own_mailbox == mailbox)
    }

    /// Whether this device has its relay account or a mailbox of its own on
    /// `relay`. Tokens of this device go only to such relays.
    pub fn knows_relay(&self, relay: &RelayUrl) -> bool {
        self.relay.as_ref().is_some_and(|r| &r.url == relay)
            || self.group.as_ref().is_some_and(|g| &g.relay == relay)
            || self.contacts.iter().any(|c| &c.own_relay == relay)
            || self
                .invitations
                .iter()
                .any(|i| &i.pending.own.relay == relay)
    }
}

/// Storage of the device state.
pub trait Store {
    /// Reads the current state.
    fn load(&self) -> io::Result<Device>;

    /// Reads the state, applies `change` and stores the result, all under
    /// one lock. Nothing is stored if `change` fails.
    fn update<R, E: From<io::Error>>(
        &self,
        change: impl FnOnce(&mut Device) -> Result<R, E>,
    ) -> Result<R, E>;
}

/// The state in one JSON file, replaced as a whole on every change.
///
/// Next to it lie `<file>.lock` for the lock and, briefly, `<file>.tmp`.
#[derive(Clone, Debug)]
pub struct JsonFile {
    path: PathBuf,
}

impl JsonFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Writes the first state. Fails if the file already exists.
    pub fn create(&self, device: &Device) -> io::Result<()> {
        let _lock = self.lock()?;
        if self.path.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "device state already exists",
            ));
        }
        self.write(device)
    }

    fn lock(&self) -> io::Result<File> {
        let file = private_options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(sibling(&self.path, "lock"))?;
        file.lock()?;
        Ok(file)
    }

    fn read(&self) -> io::Result<Device> {
        let device: Device = serde_json::from_slice(&fs::read(&self.path)?)?;
        if device.format != FORMAT_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown storage format {}", device.format),
            ));
        }
        Ok(device)
    }

    /// Writes a temporary file and renames it over the old one, so a crash
    /// leaves either the old or the new state.
    fn write(&self, device: &Device) -> io::Result<()> {
        let tmp = sibling(&self.path, "tmp");
        let mut file = private_options()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        file.write_all(&serde_json::to_vec_pretty(device)?)?;
        file.sync_all()?;
        fs::rename(&tmp, &self.path)?;
        sync_parent(&self.path)
    }
}

impl Store for JsonFile {
    fn load(&self) -> io::Result<Device> {
        let _lock = self.lock()?;
        self.read()
    }

    fn update<R, E: From<io::Error>>(
        &self,
        change: impl FnOnce(&mut Device) -> Result<R, E>,
    ) -> Result<R, E> {
        let _lock = self.lock()?;
        let mut device = self.read()?;
        let result = change(&mut device)?;
        self.write(&device)?;
        Ok(result)
    }
}

fn sibling(path: &Path, extension: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".");
    name.push(extension);
    PathBuf::from(name)
}

/// Files readable and writable only by their owner, where the system has
/// such permissions.
fn private_options() -> OpenOptions {
    // Only Unix changes the options; elsewhere they stay as they are.
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
}

/// Makes the rename itself durable.
fn sync_parent(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        File::open(parent)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io;

    use rand_core::OsRng;

    use super::{Device, JsonFile, RelayAccount, Store, StoredContact};
    use crate::contact::ContactState;
    use crate::pairing::{Address, PendingInvitation, accept};
    use crate::wire::RelayUrl;

    const T: u64 = 1_794_600_000;

    fn store() -> (tempfile::TempDir, JsonFile) {
        let dir = tempfile::tempdir().unwrap();
        let file = JsonFile::new(dir.path().join("device.json"));
        file.create(&Device::new(&mut OsRng)).unwrap();
        (dir, file)
    }

    #[test]
    fn contact_survives_storage_and_keeps_working() {
        let (_dir, file) = store();
        let address = |fill| Address {
            relay: RelayUrl::new("https://relay.example").unwrap(),
            mailbox: [fill; 32],
            send_token: [fill + 1; 32],
        };
        let (pending, invitation) = PendingInvitation::create(address(1), None, T, &mut OsRng);
        let (a, hello) = accept(&invitation, address(3), T, &mut OsRng).unwrap();
        let mut b = ContactState::new(pending.receive_hello(&hello, T).unwrap());
        let mut a = ContactState::new(a);
        a.receive(&b.contact.reveal(T, &mut OsRng).unwrap(), T)
            .unwrap();
        let [hello_ok, _] = b.confirm(T, &mut OsRng).unwrap();
        a.receive(&hello_ok, T).unwrap();
        let (_, request) = a.request(T, &mut OsRng).unwrap();
        b.receive(&request, T).unwrap();

        file.update(|device| {
            device.relay = Some(RelayAccount {
                url: RelayUrl::new("https://relay.example").unwrap(),
                access_token: Some([7; 32]),
            });
            device.contacts.push(StoredContact {
                name: "Anna".into(),
                own_relay: RelayUrl::new("https://relay.example").unwrap(),
                read_token: [9; 32],
                state: b.clone(),
            });
            Ok::<_, io::Error>(())
        })
        .unwrap();

        let mut stored = file.load().unwrap().contacts.remove(0).state;
        assert_eq!(stored.incoming, b.incoming);
        assert_eq!(stored.seen, b.seen);
        // The stored key still decrypts and answers.
        let id = stored.open_request(T).unwrap();
        let response = stored.respond(&id, T + 1, &mut OsRng).unwrap();
        assert!(matches!(
            a.receive(&response, T + 1).unwrap(),
            crate::contact::Event::Response { .. }
        ));
        // The stored replay memory still knows the first request.
        assert_eq!(
            stored.receive(&request, T + 2).unwrap(),
            crate::contact::Event::Ignored(crate::contact::Ignored::Duplicate)
        );
    }

    #[test]
    fn failed_change_is_not_stored() {
        let (_dir, file) = store();
        let result: Result<(), io::Error> = file.update(|device| {
            device.relay = Some(RelayAccount {
                url: RelayUrl::new("https://relay.example").unwrap(),
                access_token: None,
            });
            Err(io::Error::other("refused"))
        });
        assert!(result.is_err());
        assert!(file.load().unwrap().relay.is_none());
    }

    #[test]
    fn create_refuses_to_overwrite() {
        let (_dir, file) = store();
        let err = file.create(&Device::new(&mut OsRng)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn unknown_format_is_refused() {
        let (_dir, file) = store();
        let text = std::fs::read_to_string(file.path()).unwrap();
        std::fs::write(file.path(), text.replace("\"format\": 1", "\"format\": 2")).unwrap();
        assert_eq!(file.load().unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn push_fields_are_optional() {
        let (_dir, file) = store();
        // Written without them as long as they are unset, as before.
        let text = std::fs::read_to_string(file.path()).unwrap();
        assert!(!text.contains("push"));
        file.update(|device| {
            device.push_url = Some("https://push.example/up".into());
            device.push_pending = true;
            Ok::<_, io::Error>(())
        })
        .unwrap();
        let device = file.load().unwrap();
        assert_eq!(device.push_url.as_deref(), Some("https://push.example/up"));
        assert!(device.push_pending);
    }

    /// A state file as written before several devices, with a contact that
    /// has history, and an invitation.
    const OLD_FILE: &str = r#"{
  "format": 1,
  "reader_id": "01010101010101010101010101010101",
  "relay": {
    "url": "https://relay.example",
    "access_token": null
  },
  "contacts": [
    {
      "name": "Anna",
      "own_relay": "https://relay.example",
      "read_token": "0909090909090909090909090909090909090909090909090909090909090909",
      "state": {
        "contact": {
          "key": "0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a",
          "peer_key": "0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b",
          "peer": {
            "relay": "https://other.example",
            "mailbox": "0303030303030303030303030303030303030303030303030303030303030303",
            "send_token": "0404040404040404040404040404040404040404040404040404040404040404"
          },
          "own_mailbox": "0101010101010101010101010101010101010101010101010101010101010101",
          "state": "paired"
        },
        "own_rules": {
          "min_interval_minutes": 360,
          "expiry_minutes": 1440,
          "weekly_limit": 7
        },
        "peer_rules": {
          "min_interval_minutes": 60,
          "expiry_minutes": 120,
          "weekly_limit": 3
        },
        "incoming": {
          "entries": [
            {
              "id": "05050505050505050505050505050505",
              "at": 1794600000,
              "answered_at": 1794600100,
              "reported": "schläft"
            }
          ],
          "last_answer": 1794600100
        },
        "outgoing": {
          "entries": [],
          "last_answer": null
        },
        "seen": [
          {
            "id": "05050505050505050505050505050505",
            "at": 1794600000
          }
        ],
        "ended": false
      }
    }
  ],
  "invitations": [
    {
      "name": "Bert",
      "read_token": "0606060606060606060606060606060606060606060606060606060606060606",
      "pending": {
        "key": "0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c",
        "own": {
          "relay": "https://relay.example",
          "mailbox": "0707070707070707070707070707070707070707070707070707070707070707",
          "send_token": "0808080808080808080808080808080808080808080808080808080808080808"
        },
        "expires_at": 1794600600
      }
    }
  ],
  "push_url": "https://push.example/up"
}"#;

    #[test]
    fn old_file_loads_and_saves_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let file = JsonFile::new(dir.path().join("device.json"));
        std::fs::write(file.path(), OLD_FILE).unwrap();
        let device = file.load().unwrap();
        assert!(device.group.is_none() && device.devices.is_empty() && device.name.is_none());
        let state = &device.contacts[0].state;
        assert_eq!(state.own_rules_stamp, crate::wire::Stamp::default());
        assert!(state.is_paired() && state.contact.secret.is_none());
        // The open invitation has no secret value: version 1, to be deleted.
        assert!(device.invitations[0].pending.is_outdated());
        file.update(|_| Ok::<_, io::Error>(())).unwrap();
        assert_eq!(std::fs::read_to_string(file.path()).unwrap(), OLD_FILE);
    }

    #[test]
    fn several_devices_fields_round_trip() {
        let (_dir, file) = store();
        file.update(|device| {
            device.name = Some("Laptop".into());
            device.group = Some(super::Group::new(
                crate::pairing::Address {
                    relay: RelayUrl::new("https://relay.example").unwrap(),
                    mailbox: [1; 32],
                    send_token: [2; 32],
                },
                [3; 32],
                &mut OsRng,
            ));
            device.devices.push(super::KnownDevice {
                reader: [4; 16],
                name: "Phone".into(),
            });
            device.outbox.push(super::Outgoing {
                envelope: [5; crate::wire::ENVELOPE_LEN],
                created_at: T,
            });
            device.transfers.push(super::Transfer {
                contact: [6; 32],
                transfer: [7; 16],
                count: 2,
                parts: vec![super::TransferPart {
                    index: 1,
                    data: vec![8, 9],
                }],
                first_seen: T,
                discarded: false,
            });
            device.parked.push(super::Parked {
                envelope: [10; crate::wire::ENVELOPE_LEN],
                received_at: T,
            });
            device.contacts.push(StoredContact {
                name: "Anna".into(),
                own_relay: RelayUrl::new("https://relay.example").unwrap(),
                read_token: [0; 32],
                state: dummy_state(),
            });
            device.contacts[0].state.own_rules_stamp = crate::wire::Stamp {
                at: T,
                id: [11; 16],
            };
            Ok::<_, io::Error>(())
        })
        .unwrap();
        let text = std::fs::read_to_string(file.path()).unwrap();
        assert!(text.contains("\"data\": \"0809\""));
        assert!(text.contains("\"own_rules_stamp\""));
        assert!(!text.contains("\"peer_rules_stamp\""));
        let device = file.load().unwrap();
        assert_eq!(device.name.as_deref(), Some("Laptop"));
        assert_eq!(device.devices[0].name, "Phone");
        assert_eq!(device.outbox[0].envelope, [5; crate::wire::ENVELOPE_LEN]);
        assert_eq!(device.transfers[0].parts[0].data, [8, 9]);
        assert_eq!(device.parked[0].received_at, T);
        assert_eq!(device.group.unwrap().read_token, [3; 32]);
        assert_eq!(device.contacts[0].state.own_rules_stamp.id, [11; 16]);
        // Written again unchanged.
        file.update(|_| Ok::<_, io::Error>(())).unwrap();
        assert_eq!(std::fs::read_to_string(file.path()).unwrap(), text);
    }

    #[cfg(unix)]
    #[test]
    fn file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, file) = store();
        let mode = std::fs::metadata(file.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn concurrent_updates_are_not_lost() {
        let (_dir, file) = store();
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let file = file.clone();
                std::thread::spawn(move || {
                    file.update(|device| {
                        device.contacts.push(StoredContact {
                            name: format!("c{i}"),
                            own_relay: RelayUrl::new("https://relay.example").unwrap(),
                            read_token: [0; 32],
                            state: dummy_state(),
                        });
                        Ok::<_, io::Error>(())
                    })
                    .unwrap();
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(file.load().unwrap().contacts.len(), 8);
    }

    fn dummy_state() -> ContactState {
        let address = Address {
            relay: RelayUrl::new("https://relay.example").unwrap(),
            mailbox: [1; 32],
            send_token: [2; 32],
        };
        let (_, invitation) = PendingInvitation::create(address.clone(), None, T, &mut OsRng);
        ContactState::new(accept(&invitation, address, T, &mut OsRng).unwrap().0)
    }
}
