/// How a view's JSON changes between two revisions, as `flotilla_proto::delta` applies it: a
/// field that changed is replaced whole, except the long lists, which go entry by entry (a
/// thread's messages by `seq` and its page marks by page, the sidebar's rows by thread id).
public enum Delta {
    /// The list fields sent entry by entry, and the path to each entry's key.
    public static func keyed(_ view: ViewKey, _ field: String) -> [String]? {
        switch (view, field) {
        case (.thread, "messages"): ["seq"]
        case (.thread, "pages"): ["page"]
        case (.sidebar, "threads"): ["thread", "id"]
        case (.devices, "devices"): ["id"]
        case (.schedules, "schedules"): ["id"]
        default: nil
        }
    }

    public struct Failure: Error, Sendable, CustomStringConvertible {
        public let description: String
        init(_ description: String) { self.description = description }
    }

    /// Apply `ops` to `data` (the value at the delta's base). A failure means the client
    /// doesn't hold what the server thinks it does: it should ask for the view whole. On a
    /// failure `data` may be part-changed, as Rust's is; the replica lets go of it then.
    public static func apply(_ view: ViewKey, to data: inout JSON, _ ops: [Op]) throws(Failure) {
        guard case .object(var fields) = data else { throw Failure("the view isn't an object") }
        // Let go of the old value so `fields` is the only copy and changes in place.
        data = .null
        defer { data = .object(fields) }
        for op in ops {
            switch op {
            case .set(let field, let v):
                fields[field] = v
            case .unset(let field):
                fields[field] = nil
            case .put(let field, let v):
                try edit(view, &fields, field) { path, list throws(Failure) in
                    guard let key = v.at(path) else { throw Failure("an entry of \(field) without its key") }
                    if let i = list.firstIndex(where: { $0.at(path) == key }) {
                        list[i] = v
                    } else {
                        list.append(v)
                    }
                }
            case .del(let field, let keys):
                try edit(view, &fields, field) { path, list throws(Failure) in
                    for key in keys {
                        guard let i = list.firstIndex(where: { $0.at(path) == key }) else { throw Failure("\(field) has no \(key.text) to remove") }
                        list.remove(at: i)
                    }
                }
            case .order(let field, let keys):
                try edit(view, &fields, field) { path, list throws(Failure) in
                    guard keys.count == list.count else { throw Failure("\(field)'s order names \(keys.count) entries, not \(list.count)") }
                    var byKey: [JSON: JSON] = [:]
                    for e in list {
                        guard let k = e.at(path) else { throw Failure("an entry of \(field) without its key") }
                        byKey[k] = e
                    }
                    list.removeAll(keepingCapacity: true)
                    for key in keys {
                        guard let e = byKey.removeValue(forKey: key) else { throw Failure("\(field)'s order names \(key.text), which it hasn't") }
                        list.append(e)
                    }
                }
            case .unknown:
                continue
            }
        }
    }

    /// Change the keyed list `field` in place.
    private static func edit(
        _ view: ViewKey, _ fields: inout [String: JSON], _ field: String,
        _ change: ([String], inout [JSON]) throws(Failure) -> Void
    ) throws(Failure) {
        guard let path = keyed(view, field) else { throw Failure("\(field) isn't a keyed list") }
        guard case .array(var list) = fields[field] else { throw Failure("\(field) isn't a list here") }
        fields[field] = .null
        defer { fields[field] = .array(list) }
        try change(path, &list)
    }
}
