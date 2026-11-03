import FlotillaKit
import Foundation

/// New thread's branch picker, as the desktop's (`branch_picker.rs`): a new branch (the
/// default), then the project's open pull requests, then its branches. The search also takes
/// `#12` or a pull request's URL.
struct BranchChoice: Identifiable, Hashable {
    enum Kind: Hashable {
        /// From the default branch, named from the prompt.
        case new(from: String)
        case pr(OpenPR)
        case branch(String)
        /// Loading, or why the list couldn't load.
        case note(String)
    }

    var kind: Kind
    /// Another thread is on this branch already.
    var inUse: Bool
    /// Empty for a new branch, else the branch (`#N` for a note or a fork's pull request).
    var value: String

    var id: String { value }

    /// Can't be picked: a note, or a fork's pull request (boat can't push to it).
    var disabled: Bool {
        switch kind {
        case .note: true
        case .pr(let pr): pr.fromFork
        default: false
        }
    }

    /// The rows for `project`; `chosen` is listed even before the lists load.
    static func rows(_ project: Project, lists: Result<BranchChoices, ServerError>?, inUse: Set<String>, chosen: OnBranch?) -> [BranchChoice] {
        func row(_ kind: Kind, _ value: String) -> BranchChoice { BranchChoice(kind: kind, inUse: !value.isEmpty && inUse.contains(value), value: value) }
        var rows = [row(.new(from: project.defaultBranch), "")]
        switch lists {
        case nil: rows.append(row(.note("Loading pull requests and branches…"), "#loading"))
        case .failure(let e)?: rows.append(row(.note("Couldn't load them: \(e.message)"), "#error"))
        case .success(let l)?:
            for pr in l.prs { rows.append(row(.pr(pr), pr.fromFork ? "#\(pr.number)" : pr.head)) }
            for b in l.branches where !rows.contains(where: { $0.value == b }) { rows.append(row(.branch(b), b)) }
        }
        if let c = chosen, !rows.contains(where: { $0.value == c.name }) {
            rows.insert(row(.branch(c.name), c.name), at: 1)
        }
        return rows
    }

    /// The branch a picked row starts on (none: a new one), with what it merges into: a pull
    /// request's base, else the default branch.
    static func onBranch(_ project: Project, lists: Result<BranchChoices, ServerError>?, value: String) -> OnBranch? {
        guard !value.isEmpty, !value.hasPrefix("#") else { return nil }
        let pr = (try? lists?.get())?.prs.first { !$0.fromFork && $0.head == value }
        return OnBranch(name: value, base: pr?.base ?? project.defaultBranch)
    }

    /// Whether it's found by `query`: `#12` or a pull request's URL picks that pull request
    /// alone; anything else is looked for in its words.
    func matches(_ query: String, owner: String, repo: String) -> Bool {
        let q = query.trimmingCharacters(in: .whitespaces)
        if q.isEmpty { return true }
        if let n = Self.prNumber(q, owner: owner, repo: repo) {
            if case .pr(let pr) = kind, case .success(let n) = n { return pr.number == n }
            return false
        }
        let hay: String
        switch kind {
        case .new: hay = "new branch"
        case .pr(let pr): hay = "#\(pr.number) \(pr.title) \(pr.head)"
        case .branch(let b): hay = b
        case .note: return false
        }
        return hay.lowercased().contains(q.lowercased())
    }

    /// `#12`, `12`'s not one; a URL of this repo's pull request is; another repo's is a
    /// failure (it matches nothing). None: it isn't a pull request reference at all.
    static func prNumber(_ q: String, owner: String, repo: String) -> Result<Int64, ServerError>? {
        if q.hasPrefix("#"), let n = Int64(q.dropFirst()), n > 0 { return .success(n) }
        guard let url = URL(string: q), let host = url.host(), host.hasSuffix("github.com") else { return nil }
        let parts = url.path().split(separator: "/").map(String.init)
        guard parts.count >= 4, parts[2] == "pull", let n = Int64(parts[3]) else { return nil }
        guard parts[0].lowercased() == owner.lowercased(), parts[1].lowercased() == repo.lowercased() else { return .failure(ServerError("another repository's")) }
        return .success(n)
    }
}
