// Dumps a running app's macOS accessibility tree: what VoiceOver sees.
//
//   swift scripts/macos-ax-dump.swift PID [MAX_DEPTH]
//
// Prints one line per element: role, subrole, title/description/value,
// selection state and, for lists and tables, the AXRows / AXSelectedRows
// counts. Menu items also show their key equivalent, which checks the menu
// shortcuts. Needs the Accessibility permission (System Settings › Privacy &
// Security › Accessibility) for the app that runs it.

import AppKit
import ApplicationServices

let args = CommandLine.arguments
guard args.count >= 2, let pid = pid_t(args[1]) else {
    FileHandle.standardError.write("usage: macos-ax-dump.swift PID [MAX_DEPTH]\n".data(using: .utf8)!)
    exit(2)
}
let maxDepth = args.count >= 3 ? Int(args[2]) ?? 30 : 30

guard AXIsProcessTrusted() else {
    FileHandle.standardError.write(
        "error: no Accessibility permission for this process's app\n".data(using: .utf8)!)
    exit(3)
}

func attr(_ e: AXUIElement, _ name: String) -> AnyObject? {
    var value: AnyObject?
    return AXUIElementCopyAttributeValue(e, name as CFString, &value) == .success ? value : nil
}

func text(_ e: AXUIElement, _ name: String) -> String? {
    guard let v = attr(e, name) else { return nil }
    let s: String
    if let str = v as? String { s = str } else if let n = v as? NSNumber { s = n.stringValue } else { return nil }
    if s.isEmpty { return nil }
    return s.count > 120 ? String(s.prefix(117)) + "..." : s
}

func modifiers(_ m: Int) -> String {
    // kAXMenuItemModifier*: bit 0 shift, 1 option, 2 control, 3 no-command.
    var out = ""
    if m & 4 != 0 { out += "⌃" }
    if m & 2 != 0 { out += "⌥" }
    if m & 1 != 0 { out += "⇧" }
    if m & 8 == 0 { out += "⌘" }
    return out
}

var printed = 0
func dump(_ e: AXUIElement, _ depth: Int) {
    guard depth <= maxDepth, printed < 5000 else { return }
    var parts = [text(e, "AXRole") ?? "?"]
    if let s = text(e, "AXSubrole") { parts.append("(\(s))") }
    for key in ["AXTitle", "AXDescription", "AXValue"] {
        if let s = text(e, key) { parts.append("\(key.dropFirst(2))=\"\(s)\"") }
    }
    if let sel = attr(e, "AXSelected") as? Bool, sel { parts.append("SELECTED") }
    if let rows = attr(e, "AXRows") as? [AXUIElement] {
        let selected = (attr(e, "AXSelectedRows") as? [AXUIElement])?.count ?? 0
        parts.append("rows=\(rows.count) selectedRows=\(selected)")
    }
    if let c = text(e, "AXMenuItemCmdChar") {
        let m = (attr(e, "AXMenuItemCmdModifiers") as? NSNumber)?.intValue ?? 0
        parts.append("key=\(modifiers(m))\(c)")
    }
    print(String(repeating: "  ", count: depth) + parts.joined(separator: " "))
    printed += 1
    for child in (attr(e, "AXChildren") as? [AXUIElement]) ?? [] {
        dump(child, depth + 1)
    }
}

let app = AXUIElementCreateApplication(pid)
if let menuBar = attr(app, "AXMenuBar") {
    print("== menu bar")
    dump(menuBar as! AXUIElement, 0)
}
for window in (attr(app, "AXWindows") as? [AXUIElement]) ?? [] {
    print("== window")
    dump(window, 0)
}
