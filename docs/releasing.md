# Releasing Po

A release ships the `po` binary for every platform and the Android app, and both update
themselves from it: `po` checks the latest release once a day and replaces itself in the
background, the app offers the new version on its home screen.

## Cutting a release

1. Set the new version in `Cargo.toml` (`[workspace.package] version`), commit, push.
2. Tag it and push the tag:

   ```bash
   git tag v0.2.0
   git push origin v0.2.0
   ```

3. The **Release** workflow checks that the tag matches `Cargo.toml`, builds every platform and
   the APK, and opens a **draft** release with:
   - `po-v0.2.0-<target>.zip` / `.tar.gz`: archives for people;
   - `po-<target>[.exe]`: the bare binaries `po update` downloads;
   - `po-v0.2.0-android.apk`: the app;
   - `SHA256SUMS.txt`: the checksums both updaters verify before installing anything.
4. Review the draft (notes, assets), then **Publish**. Only a published release counts as
   "latest": that is when installed copies start updating.

## The Android signing key

An Android app can only be updated by an APK signed with the same key, so every release must be
signed with the same one. The workflow reads it from four repository secrets
(Settings → Secrets and variables → Actions → New repository secret):

| Secret | Value |
|---|---|
| `ANDROID_KEYSTORE_BASE64` | the keystore file, base64-encoded |
| `ANDROID_KEYSTORE_PASSWORD` | the keystore's password |
| `ANDROID_KEY_ALIAS` | the key's alias |
| `ANDROID_KEY_PASSWORD` | the key's password (the same as the keystore's for a PKCS12 keystore) |

With the GitHub CLI:

```bash
gh secret set ANDROID_KEYSTORE_BASE64 < po-release.jks.base64
gh secret set ANDROID_KEYSTORE_PASSWORD < password.txt
gh secret set ANDROID_KEY_ALIAS --body po
gh secret set ANDROID_KEY_PASSWORD < password.txt
```

Without these secrets the APK is signed with a throwaway debug key: it installs, but cannot
update an app signed with another key. **Keep the keystore and its password safe and backed up**:
losing them means users must uninstall the app to install a new one.

To create a keystore:

```bash
keytool -genkeypair -keystore po-release.jks -storetype PKCS12 -alias po \
  -keyalg RSA -keysize 4096 -validity 10000 -dname "CN=Po, O=Po"
base64 -w0 po-release.jks > po-release.jks.base64
```

## How the updates work

- **`po`**: a binary built by the release workflow (`PO_RELEASE` set at compile time) checks the
  latest release at most once a day, from a session or from the background service. It
  downloads the binary for its own platform, verifies it against `SHA256SUMS.txt`, runs it with
  `--version`, and only then puts it in place, the old one stepping aside. The next start says
  so; the service restarts itself on it. A build from source never replaces itself: it only says
  a newer version exists. `po update` updates now, `po update check` only looks,
  `po update off` (or `PO_NO_UPDATE=1`) turns the automatic checks off.
- **The app**: a release build reads the latest release at launch. When it is newer, the home
  screen offers it; the APK is downloaded, verified against `SHA256SUMS.txt`, and installed
  through Android's package installer, which asks for confirmation (the first time, Android
  also asks to allow Po to install apps).
