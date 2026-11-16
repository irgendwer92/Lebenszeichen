# Lebenszeichen – Signed Android App

As of 2026-11-16. Belongs to `roadmap.md` (M4).

The signed APK is built by the workflow `.github/workflows/android-release.yml`, on a tag `v*` or by hand. Pull requests continue to build only the debug APK. You create the key for it yourself, once; it leaves your computer only as a secret at GitHub.

## 1. Create the key

```sh
keytool -genkeypair -v \
  -keystore ~/lebenszeichen-release.jks \
  -alias lebenszeichen \
  -keyalg RSA -keysize 4096 \
  -validity 10000 \
  -dname "CN=Lebenszeichen"
```

- `keytool` asks for a password. Use a long, random one, for example from the password manager. If it asks for a separate password for the key, press Enter: then the same one applies. The workflow expects exactly that.
- 10,000 days is about 27 years. An Android app can **never** change its key. If it is lost, there are no more updates; everyone must uninstall and pair again.
- `-dname` deliberately contains nothing personal: it is readable in every APK.

## 2. Back up

Keep the file `lebenszeichen-release.jks` and the password in two separate places, for example in the password manager and in an encrypted backup. Without both together the key is worthless.

Note the fingerprint, to compare it later with the CI run:

```sh
keytool -list -v -keystore ~/lebenszeichen-release.jks -alias lebenszeichen | grep SHA256
```

## 3. Environment "release" at GitHub

The environment holds the key and can tie every run to an approval. What GitHub offers of this depends on the plan for private repositories:

| Plan | Environment with its own secrets | Approval by you before every run |
|---|---|---|
| Free | no | no |
| Pro, Team | yes | no |
| Enterprise, or public repository | yes | yes |

**With environment** (Pro and higher): Repository → Settings → Environments → New environment, name `release`.

- "Deployment branches and tags": "Selected branches and tags", then add `main` and the tag pattern `v*`. Then no other branch can use the key.
- If offered: "Required reviewers" with yourself. Then every signed build waits for your approval under Actions.

**Without environment** (Free): The secrets go into the repository instead of the environment (commands below without `--env release`). The workflow works the same way, because it reads the secrets from the environment and, if they are not there, from the repository. Protection is then provided only by the triggers: tag or start by hand, never a pull request.

## 4. Store secrets

```sh
base64 -w0 ~/lebenszeichen-release.jks | gh secret set ANDROID_KEYSTORE_BASE64 --env release -R irgendwer92/Lebenszeichen
gh secret set ANDROID_KEYSTORE_PASSWORD --env release -R irgendwer92/Lebenszeichen
gh variable set ANDROID_KEY_ALIAS --env release -R irgendwer92/Lebenszeichen -b lebenszeichen
```

The second command asks for the password; this way it does not end up in the shell history. The alias is not a secret and is therefore stored as a variable: as a secret, GitHub would replace every "lebenszeichen" in the logs with `***`. Check:

```sh
gh secret list --env release -R irgendwer92/Lebenszeichen
gh variable list --env release -R irgendwer92/Lebenszeichen
```

The two secrets and the variable should appear. GitHub never shows the values of the secrets again.

## 5. Build

- **For testing:** `gh workflow run android-release.yml --ref main` (before merging M4: `--ref m4-android`). The APK is attached to the run as an artifact, `versionCode` is the run number.
- **As a release:** raise `version` in `app/src-tauri/tauri.conf.json`, merge, then `git tag v0.1.0 && git push origin v0.1.0`. The workflow checks that tag and version match, and creates a **draft** of a GitHub release with the APK and checksum. You publish it yourself. `versionCode` follows Tauri's formula: 0.1.0 → 1000, 0.1.1 → 1001, 0.2.0 → 2000. Android installs an update only with a higher `versionCode`.

## 6. Verify and install

- In the run under "Check the APK" there is "Certificate" with the SHA-256 fingerprint. It must match the one from step 2.
- The release app and the debug app from the pull requests have the same package name but different keys. Android does not install one over the other: uninstall the debug app before switching. Doing so loses the device and contacts on the phone; pair again afterward.

## Fingerprint

Certificate of the release APK, SHA-256 (since 2026-11-15):

```
61:EE:59:A2:68:54:C5:F2:CD:A0:11:2D:3E:02:D7:6D:F0:6E:06:5E:6A:2C:C0:52:0D:5C:C3:A3:44:3C:F2:AF
```

An APK with a different fingerprint does not come from this project.
