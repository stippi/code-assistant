# macOS Code Signing & Notarization

The macOS `.app` bundle is produced by `scripts/bundle-macos.sh`. The script is
usable in three ways, driven entirely by environment variables — it has no
CI-specific logic:

| Scenario | What happens |
| --- | --- |
| No env vars set (default, e.g. a local build) | **Ad-hoc** signature only. No Apple account needed. Not notarized. |
| `MACOS_SIGN_IDENTITY` set | Signs with your **Developer ID Application** identity, hardened runtime + entitlements. |
| Signing identity **and** all notary vars set | Additionally **notarizes** with `xcrun notarytool` and **staples** the ticket. |

All signing uses only tools shipping with macOS / Xcode (`codesign`,
`xcrun notarytool`, `xcrun stapler`) — no third-party actions or CLIs.

## Local usage

### Plain build (no signing)

```bash
./scripts/bundle-macos.sh                # host arch, ad-hoc signed
```

### Signed build with your own certificate

If you have a *Developer ID Application* certificate in your login keychain:

```bash
# Find the exact identity string:
security find-identity -v -p codesigning

export MACOS_SIGN_IDENTITY="Developer ID Application: Jane Doe (TEAMID1234)"
./scripts/bundle-macos.sh
```

### Signed + notarized build

Add the notary credentials (an app-specific password created at
<https://appleid.apple.com>):

```bash
export MACOS_SIGN_IDENTITY="Developer ID Application: Jane Doe (TEAMID1234)"
export MACOS_NOTARY_APPLE_ID="jane@example.com"
export MACOS_NOTARY_PASSWORD="abcd-efgh-ijkl-mnop"   # app-specific password
export MACOS_NOTARY_TEAM_ID="TEAMID1234"
./scripts/bundle-macos.sh universal
```

## CI usage

The `Release` workflow (`.github/workflows/release.yml`) does the same thing
automatically on the macOS build jobs. It imports the certificate into an
ephemeral keychain and then calls the exact same bundle script. When the secrets
are **not** configured, the workflow falls back to ad-hoc signing and the
release still succeeds.

### Required GitHub secrets

Configure these under **Settings → Secrets and variables → Actions**:

| Secret | Purpose |
| --- | --- |
| `MACOS_CERT_P12_BASE64` | Your *Developer ID Application* certificate **and private key** exported as a `.p12`, then base64-encoded. |
| `MACOS_CERT_PASSWORD` | The password protecting that `.p12` file. |
| `MACOS_KEYCHAIN_PASSWORD` | Any throwaway password used for the temporary keychain created on the runner. |
| `MACOS_SIGN_IDENTITY` | The identity name, e.g. `Developer ID Application: Jane Doe (TEAMID1234)`. |
| `MACOS_NOTARY_APPLE_ID` | Apple ID email used for notarization. |
| `MACOS_NOTARY_PASSWORD` | App-specific password for that Apple ID. |
| `MACOS_NOTARY_TEAM_ID` | Apple Developer Team ID (10 characters). |

If only the signing secrets (first four) are set, the app is signed but not
notarized. If none are set, the build is ad-hoc signed.

### Producing the certificate secrets

1. In Keychain Access, export your *Developer ID Application* certificate
   (including its private key) as a `.p12` file and set a password.
2. Base64-encode it for storage in a secret:

   ```bash
   base64 -i DeveloperID.p12 | pbcopy   # now paste into MACOS_CERT_P12_BASE64
   ```

3. Put the `.p12` password into `MACOS_CERT_PASSWORD`.

## Entitlements

Signing under the hardened runtime uses
`crates/code_assistant/assets/Entitlements.plist`. Because the app embeds gpui
(Zed's UI framework), the entitlements allow JIT / unsigned executable memory,
which the hardened runtime otherwise blocks at launch. The set mirrors the
hardened-runtime keys used by Zed itself and deliberately avoids the weaker keys
(library-validation / dyld-environment / executable-page-protection) that Zed
also leaves off and that Apple's notary service scrutinizes.
