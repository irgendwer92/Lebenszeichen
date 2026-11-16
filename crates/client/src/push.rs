//! The push address of this device at its own mailboxes.
//!
//! The relay wakes a device through the push address of its reader entry
//! (`docs/wire-format.md`). Every own mailbox has its own entry, so a new
//! address goes to all of them. Until every mailbox has it, `push_pending`
//! stays set and the next fetch tries again.

use lz_core::store::Store;

use crate::ctx::{Ctx, Fail, Result};
use crate::relay::{Relay, RelayError};
use crate::sync::own_mailboxes;

/// Most bytes of a push address that the relay accepts.
const MAX_PUSH_URL_LEN: usize = 1024;

fn check(url: &str) -> Result<()> {
    if !url.starts_with("https://") {
        return Err(fail!("Die Push-Adresse muss mit https:// beginnen."));
    }
    if url.len() > MAX_PUSH_URL_LEN {
        return Err(fail!("Die Push-Adresse ist zu lang."));
    }
    Ok(())
}

impl<R: Relay> Ctx<R> {
    /// Sets the push address of this device, or removes it with `None`, and
    /// sends it to every own mailbox.
    ///
    /// The address is stored even if the relay cannot be reached; the next
    /// fetch sends it then. An unchanged address that every mailbox already
    /// has costs no call to the relay.
    pub async fn set_push_url(&mut self, url: Option<&str>) -> Result<()> {
        if let Some(url) = url {
            check(url)?;
        }
        let due = self.store.update(|device| {
            if device.push_url.as_deref() != url {
                device.push_url = url.map(str::to_owned);
                device.push_pending = true;
            }
            Ok::<_, Fail>(device.push_pending)
        })?;
        if due {
            self.send_push_url().await?;
        }
        self.say(match url {
            Some(_) => "Push-Adresse eingetragen.",
            None => "Push-Adresse entfernt.",
        });
        Ok(())
    }

    /// Sends the stored push address again if some mailbox may still have an
    /// older one. Failures only produce a line of output; fetching goes on.
    pub(crate) async fn retry_push_url(&mut self) -> Result<()> {
        if !self.store.load()?.push_pending {
            return Ok(());
        }
        if let Err(error) = self.send_push_url().await {
            self.say(&format!(
                "Push-Adresse noch nicht überall eingetragen: {error}"
            ));
        }
        Ok(())
    }

    /// Sends the stored push address to every own mailbox and clears
    /// `push_pending` once all of them have it.
    async fn send_push_url(&mut self) -> Result<()> {
        let device = self.store.load()?;
        let mailboxes = own_mailboxes(&device);
        let mut first_error = None;
        for (relay, boxes) in &mailboxes {
            for (mailbox, token) in boxes {
                let result = self
                    .relay
                    .put_reader(
                        relay,
                        mailbox,
                        token,
                        &device.reader_id,
                        device.push_url.as_deref(),
                    )
                    .await;
                match result {
                    // Gone mailboxes are noticed and cleaned up by fetching.
                    Ok(()) | Err(RelayError::UnknownMailbox) => {}
                    Err(RelayError::Refused { status: 400, .. }) => {
                        first_error.get_or_insert_with(|| {
                            fail!(
                                "Das Relay {relay} lehnt die Push-Adresse ab. Es nimmt nur \
                                 öffentliche https-Adressen an, manche auch nur bestimmte."
                            )
                        });
                    }
                    Err(error) => {
                        first_error.get_or_insert_with(|| error.into());
                    }
                }
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        // Only if nothing changed in between: a new address or a new mailbox
        // would still need sending.
        self.store.update(|now| {
            if now.push_url == device.push_url && own_mailboxes(now) == mailboxes {
                now.push_pending = false;
            }
            Ok::<_, Fail>(())
        })
    }
}
