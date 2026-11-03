# Agents

## Pull request checks

Pull requests aren't checked on GitHub. After every push to a pull request's branch, run `script/signoff`: it runs the checks and signs off on each one with [gh-signoff](https://github.com/basecamp/gh-signoff), and the pull request can't merge without them.

- It exits 1 if a check failed: fix it, commit, push, and run it again.
- `warning: not posted to GitHub` usually means HEAD isn't pushed: push, and run it again. If the error above it says you can't write to the repo (such as a 403, from a fork), don't run it again: a maintainer signs off.
- Say in your reply if it skipped anything, such as `swift test` off a Mac.
