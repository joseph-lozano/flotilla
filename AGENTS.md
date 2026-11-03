# Agents

## Pull request descriptions

When you open a pull request, or update the description of one, follow the template in [`.github/pull_request_template.md`](.github/pull_request_template.md):

- Use its headings, in its order: **What Changed**, **Why**, **Diagrams**, **UI Changes**.
- Fill in **What Changed** and **Why** on every PR. Keep the scope tight: if the PR does several unrelated things, say so, or split it.
- **Diagrams**: when the change touches dataflow or state, add a diagram (a Mermaid block renders on GitHub). Otherwise delete the section.
- **UI Changes**: when the change touches UI, include before and after screenshots, and a short video for motion or interaction. Otherwise delete the section. If you can't capture them yourself, say so in the section rather than deleting it.
- Remove the template's `<!-- -->` comments once you've filled a section in.
- When you push more commits to an open PR, update its description so it still matches the whole change, not just the first commit.

`gh pr create` doesn't apply the template on its own when you pass `--body`, so write the body from the template yourself, for example with `--body-file`.

## Pull request checks

Pull requests aren't checked on GitHub. After every push to a pull request's branch, run `script/signoff`: it runs the checks and signs off on each one with [gh-signoff](https://github.com/basecamp/gh-signoff), and the pull request can't merge without them.

- It exits 1 if a check failed: fix it, commit, push, and run it again.
- `warning: not posted to GitHub` usually means HEAD isn't pushed: push, and run it again. If the error above it says you can't write to the repo (such as a 403, from a fork), don't run it again: a maintainer signs off.
- Say in your reply if it skipped anything, such as `swift test` off a Mac.
