# Agents

## Pull request checks

Pull requests aren't checked on GitHub. You run the checks, and [gh-signoff](https://github.com/basecamp/gh-signoff) puts a status on the commit for each one: `signoff/rustfmt`, `signoff/clippy`, `signoff/clippy-snapshot`, `signoff/tests` and `signoff/docs`, green when it passes and red when it fails. A pull request can't merge until they're all green. The CI workflows still run on `main` after each merge.

After every push to a pull request's branch, run:

```bash
script/signoff
```

| Check | Runs | When the branch changes |
| --- | --- | --- |
| `rustfmt` | `cargo fmt --all --check` | `crates/`, the Cargo files, `rustfmt.toml`, the shared fixtures |
| `clippy` | `cargo clippy --workspace --all-targets --locked -- -D warnings` | the same |
| `clippy-snapshot` | the same clippy, with `--features flotilla-app/snapshot` | the same |
| `tests` | `cargo test --workspace --locked --no-fail-fast` | the same |
| `docs` | `script/docs-md --check` | `www/`, `script/docs-md` |
| `swift` | `swift test` in `ios/FlotillaKit`, on a Mac only. Not required. | `ios/FlotillaKit/`, the shared fixtures |

A check whose paths the branch doesn't change is signed off without running. Every check runs even after one fails. It ends with each check's result, and exits 1 if any failed, 0 if they all passed. It installs gh-signoff if it's missing.

- Commit and push first. The statuses go on the pushed commit, so when the tree has changes or HEAD isn't pushed, it runs the checks and posts nothing.
- When it can't post (`warning: not posted to GitHub`), it still exits 0 if the checks passed. On your own branch that's a problem to fix, usually an unpushed commit: push, and run it again. From a fork it's expected, since only people with write access to the repo can sign off: a maintainer checks the pull request out (`gh pr checkout <number>`), reads the diff, and runs it to sign off.
- When a check fails, its status goes red, saying which. Fix it, commit, push, and run it again. Each push needs its own signoffs: a new commit has none.
- Don't sign off by hand (`gh signoff <check>`, or `-f`) on a check that hasn't passed.
- If it skips something, such as `swift test` off a Mac, say so in your reply.
- `script/signoff --no-signoff` runs the checks without posting anything, for before you push. `--all` runs every check, whatever changed.
