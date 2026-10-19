import AppKit

public typealias QuitCallback = @convention(c) () -> Void

private final class QuitGate: NSObject, NSApplicationDelegate {
    let wrapped: NSApplicationDelegate
    let onQuit: QuitCallback
    init(_ wrapped: NSApplicationDelegate, _ onQuit: @escaping QuitCallback) {
        self.wrapped = wrapped
        self.onQuit = onQuit
    }
    override func responds(to selector: Selector!) -> Bool {
        super.responds(to: selector) || wrapped.responds(to: selector)
    }
    override func forwardingTarget(for selector: Selector!) -> Any? { wrapped }
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        // Tao's native applicationWillTerminate notification is too late to
        // cancel. Route Cmd-Q, Dock Quit and AppleEvent Quit into Rust first;
        // Rust permits its own final exit only after all native resources drop.
        onQuit()
        return .terminateCancel
    }
}
private var quitGate: QuitGate?

@_cdecl("hm_application_install_quit_gate") public func hmApplicationInstallQuitGate(_ callback: @escaping QuitCallback) -> Int32 {
    guard Thread.isMainThread, let delegate = NSApplication.shared.delegate else { return 1 }
    let gate = QuitGate(delegate, callback)
    quitGate = gate
    NSApplication.shared.delegate = gate
    return 0
}

// Explicit synthetic exit probe: exercise AppKit's native Quit path, including
// the delegate's termination decision, rather than calling libc exit directly.
@_cdecl("hm_application_request_quit") public func hmApplicationRequestQuit() {
    NSApplication.shared.terminate(nil)
}

// Write only. The application never reads the user's clipboard.
@_cdecl("hm_clipboard_write") public func hmClipboardWrite(_ text: UnsafePointer<CChar>) -> Int32 {
    let value = String(cString: text)
    let write = { () -> Int32 in
        let board = NSPasteboard.general
        board.clearContents()
        return board.setString(value, forType: .string) ? 0 : 1
    }
    return Thread.isMainThread ? write() : DispatchQueue.main.sync(execute: write)
}
