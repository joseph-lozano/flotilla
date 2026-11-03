# Releasing Flotilla

For maintainers. Releases are GitHub Releases, built by GitHub Actions when a `v*` tag is pushed. Nothing in the workflows or scripts names the repo, so they work unchanged in a fork or a new repo.

## The workflows

| Workflow | Runs on | Does |
| --- | --- | --- |
| `ci.yml` | pushes to `main` that touch `crates/`, the Cargo files or the shared fixtures | `cargo fmt --all --check`, clippy with and without `--features flotilla-app/snapshot`, and `cargo test --workspace`, on Linux. clippy on macOS too, in a public repo, or when run by hand. |
| `flotillakit.yml` | pushes to `main` that change `ios/FlotillaKit/` or `mobile/fixtures/` | `swift test` on macOS 26 |
| `docs.yml` | pushes to `main` that change `www/` or `script/docs-md` | `script/docs-md --check`: the docs' `.md` pages, `llms.txt` and `llms-full.txt` match the HTML |
| `release.yml` | a pushed `v*` tag, or by hand as a dry run | builds every download and publishes the release |

Pull requests run none of them. `script/signoff` runs the same checks where the work was done and signs off on each one with [gh-signoff](https://github.com/basecamp/gh-signoff), as a `signoff/<check>` status on the commit; the `signoff` ruleset (`gh signoff install rustfmt clippy clippy-snapshot tests docs`) requires them to merge. AGENTS.md has the details.

A release has these files:

| File | What |
| --- | --- |
| `Flotilla.dmg` | The Mac app, for Apple silicon and Intel |
| `Flotilla-amd64.deb`, `Flotilla-arm64.deb` | The Linux desktop app, for Debian and Ubuntu (22.04 or later) |
| `Flotilla-x86_64.AppImage`, `Flotilla-aarch64.AppImage` | The Linux desktop app for any other distribution (Fedora, Arch and the rest). The first start makes it the `flotilla://` handler. |
| `flotilla-cli-linux-x86_64.tar.gz`, `flotilla-cli-linux-aarch64.tar.gz` | The `flotilla` command (server and CLI), linked statically, so it runs on any Linux |
| `flotilla-cli-macos-universal.tar.gz` | The `flotilla` command for the Mac, Apple silicon and Intel. (The app doesn't include it.) |
| `SHA256SUMS` | Every file's SHA-256 |

The names stay the same from release to release, so `https://github.com/<owner>/<repo>/releases/latest/download/<file>` always gets the latest. The release's tag carries the version.

The notes are the tag's message, how to install each file, then the commits since the tag before it.

## Cut a release

1. On a clean, up-to-date `main`, write the release's opening lines in a file, then:

   ```sh
   script/release 0.2.0 --notes /tmp/notes.md
   ```

   It sets the version in `Cargo.toml` and `Cargo.lock`, freezes the docs with `script/docs-release 0.2.0 --default` (so `/docs/` opens them and `llms.txt` lists them; a pre-release's docs aren't made the default), commits "Flotilla 0.2.0", and tags `v0.2.0` with your notes. Without `--notes` the message is "Flotilla 0.2.0." alone. It doesn't push. It doesn't touch the iPhone app's version (`ios/project.yml`).
2. Push the commit and the tag together. The script prints the command:

   ```sh
   git push --atomic origin main v0.2.0
   ```

3. Follow the **Release** run in the Actions tab. When it's green, the release is on the Releases page and is the latest. A version with a hyphen (`0.2.0-rc.1`) is published as a pre-release.

**If the run fails:** for a flaky step, re-run the failed jobs. If the code needs a fix, delete the tag (`git push --delete origin v0.2.0 && git tag -d v0.2.0`), fix `main`, and cut it again: `script/release 0.2.0` makes that version's docs again from `main`'s. To run the release again on an existing tag, start the workflow by hand on the tag with **Dry run** unticked; it updates the files and notes of a release that already exists.

## A dry run

Actions → **Release** → **Run workflow**, on `main`, with **Dry run** ticked (the default), or:

```sh
gh workflow run release.yml --ref main -f dry_run=true
```

It builds everything and keeps the files, `SHA256SUMS` and `release-notes.md` as the run's artifact, `flotilla-<version>-dry-run`, for 7 days. Nothing is published. The run's summary shows the checksums and the notes.

## Signing and notarizing the Mac app

The Mac app and the Mac `flotilla` command are signed with the project's Developer ID and notarized by Apple, so they open with no warning. That takes five secrets in the repo that publishes the releases (only there), set up once:

| Secret | What |
| --- | --- |
| `MACOS_CERT_P12` | The Developer ID Application certificate and its private key, as a .p12, base64-encoded |
| `MACOS_CERT_PASSWORD` | The .p12's password |
| `ASC_KEY_P8` | An App Store Connect API key: the contents of `AuthKey_<KEY ID>.p8` |
| `ASC_KEY_ID` | That key's Key ID |
| `ASC_ISSUER_ID` | Your Issuer ID |

1. **Export the certificate you already have.** In Keychain Access, open the login keychain's **My Certificates**. Find **Developer ID Application: <your name> (<team ID>)** and check that it has a private key under it. Right-click it, choose **Export**, save it as a .p12, and give it a strong password. (Without one, a Developer Program account holder makes it in Xcode: **Settings → Accounts → Manage Certificates → + → Developer ID Application**.)
2. **Make an API key.** In App Store Connect, go to **Users and Access → Integrations → App Store Connect API → Team Keys** and add a key. Notarizing needs **Developer** access or more; give it **App Manager** if the same key will upload iPhone builds to TestFlight. Download `AuthKey_<KEY ID>.p8` (Apple lets you download it only once), and note the Key ID beside it and the Issuer ID above the list.
3. **Add the secrets**, from a checkout of the repo (or add `--repo <owner>/<repo>`):

   ```sh
   base64 -i DeveloperID.p12 | gh secret set MACOS_CERT_P12
   gh secret set MACOS_CERT_PASSWORD          # paste the .p12's password
   gh secret set ASC_KEY_P8 < AuthKey_ABCDE12345.p8
   gh secret set ASC_KEY_ID --body ABCDE12345
   gh secret set ASC_ISSUER_ID --body 00000000-0000-0000-0000-000000000000
   ```

4. Delete the .p12 file, and keep the .p8 somewhere safe.
5. Check with a dry run: in the **macOS (app and CLI)** job, the **Package** step ends both lines with "signed with a Developer ID and notarized", and the **Check** step runs `spctl` on the app.

The run imports the certificate into a temporary keychain, which it deletes at the end. It signs with the hardened runtime and a timestamp, notarizes the DMG with `notarytool` and staples the ticket to it, and notarizes the command in a zip. A bare binary can't hold a ticket, so Gatekeeper checks the command online. The team ID comes with the certificate, so it isn't a secret of its own.

**When the certificate is renewed** (a Developer ID Application certificate lasts five years), export the new one and replace `MACOS_CERT_P12` and `MACOS_CERT_PASSWORD`. Apps signed and notarized before the old one expired keep opening.

**The fallback:** with none of the five secrets (a fork, or a new repo before they're added), the release still builds, but the app and the command are signed ad hoc. The run warns about it, and the notes tell people how to get past Gatekeeper: open the app once, then **System Settings → Privacy & Security → Open Anyway**; for the command, `xattr -d com.apple.quarantine` on it. Some but not all of the secrets is a mistake, so that stops the run.

## Moving to the public repo

The plan: rename this repo to `flotilla-archive`, and start a new public `flotilla` repo with no history.

1. **First, point every existing checkout at the archive**, so no push can send the private history to the new repo: `git remote set-url origin https://github.com/<owner>/flotilla-archive.git`.
2. Rename this repo to `flotilla-archive` in **Settings → General**. GitHub redirects the old name only until a repo takes it.
3. Create the new public `flotilla` repo, empty. Check the tree for anything that shouldn't be public, then make a new checkout of it, with the tree as its one commit:

   ```sh
   mkdir flotilla-public
   git -C flotilla-archive-checkout archive main | tar -x -C flotilla-public
   cd flotilla-public
   git init -b main && git add -A && git commit -m "Flotilla"
   git remote add origin https://github.com/<owner>/flotilla.git
   git push -u origin main
   ```

   Work in this checkout from now on: its `origin` and `main` are the public repo's, so `script/release` and the push it prints go there.

4. **What moves with the tree:** `.github/workflows/` and `script/` (`release`, `release-notes`, `docs-release`, `docs-md`, `bundle-macos`, `bundle-linux`, `package-macos`, `ci/`). They need no edits.
5. **What doesn't move, so set it up again:**
   - The five signing secrets above. Secrets don't move with a repo, and they belong only in the one that publishes: add them in the new repo, then run a dry run there to see the Mac files signed and notarized.
   - Optionally, a tag ruleset (**Settings → Rules**) so only maintainers can push `v*` tags, since a tag publishes a release.
   - The Actions caches and artifacts don't move, and don't need to. The first CI run fills the cache.
6. **The first release** has no tag before it, so its notes are only what you write: say what Flotilla is and what's in the downloads. If `Cargo.toml` already has the version you're releasing, `script/release` leaves it alone and commits only the frozen docs. For example, for a 0.1.0:

   ```sh
   script/release 0.1.0 --notes /tmp/first-release.md
   git push --atomic origin main v0.1.0
   ```

7. Once the release is public, point the site's and the README's download wording at `https://github.com/<owner>/flotilla/releases/latest`.

In a public repo, the standard runners cost nothing, so `ci.yml` checks on macOS for every push to `main`.

## What a run costs

GitHub bills each job's time rounded up to the minute. In a private repo, a Mac minute counts as ten against the plan's included minutes (2,000 a month on Free, 3,000 on Pro), and costs about ten times as much past them: roughly $0.006 a minute on Linux and $0.062 on a Mac, as of 2026. In a public repo, these runners cost nothing.

Measured on the private repo, November 2026:

| Run | Linux | Mac | Past the included minutes |
| --- | --- | --- | --- |
| CI, a push that touches the Rust side, cargo cache warm | 6 min | none | about $0.04 |
| CI, cache cold (the first run, or after `Cargo.lock` or the toolchain changes) | 14 min | none | about $0.08 |
| FlotillaKit, when the package or its fixtures change | none | 1 min | about $0.06 |
| A release or a dry run | 58 min (6 jobs) | 11 min (app and CLI) | about $1 |

Pushes that touch only `docs/`, `design/` or the iPhone app outside FlotillaKit run no CI; `www/` runs only `docs.yml`, a few seconds on Linux. The cargo cache is about 0.9 GB; only `main` saves it, and a cache GitHub hasn't used for 7 days is dropped. A release builds without the cache, so nothing a branch put there can reach a release.
