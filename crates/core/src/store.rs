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

use crate::contact::ContactState;
use crate::pairing::PendingInvitation;
use crate::wire::{READER_ID_LEN, RelayUrl, Token};

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
        }
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
