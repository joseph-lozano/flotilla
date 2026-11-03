# Agents

## Pull request checks

Pull requests aren't checked on GitHub. You run the checks, and `gh signoff` ([gh-signoff](https://github.com/basecamp/gh-signoff)) puts a green `signoff` status on the commit when they pass. A pull request can't merge without it. The CI workflows still run on `main` after each merge.

After every push to a pull request's branch, run:

```bash
script/signoff
```

It runs the checks for what the branch changes since `main` (rustfmt, clippy with and without the snapshot feature, and the tests for the Rust side; `script/docs-md --check` for `www/`; `swift test` for FlotillaKit, on a Mac only), then signs off on the pushed commit. It installs gh-signoff if it's missing.

- Commit and push first: it checks the commit that's pushed, and stops if the tree has changes or HEAD isn't pushed.
- When a check fails, it marks the commit red with the step that failed. Fix it, commit, push, and run it again. Each push needs its own signoff: a new commit has none.
- Don't sign off by hand (`gh signoff`, or `-f`) on a commit the checks haven't passed.
- If it skips something, such as `swift test` off a Mac, say so in your reply.
- `script/signoff --no-signoff` runs the checks without posting anything, for before you push. `--all` runs every check, whatever changed.
