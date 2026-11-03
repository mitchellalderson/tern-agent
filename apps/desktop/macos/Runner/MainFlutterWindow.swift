import Cocoa
import FlutterMacOS

class MainFlutterWindow: NSWindow {
  override func awakeFromNib() {
    let flutterViewController = FlutterViewController()
    flutterViewController.backgroundColor = .clear
    self.contentViewController = flutterViewController
    self.setFrame(NSRect(x: 0, y: 0, width: 1180, height: 760), display: true)
    self.center()
    self.minSize = NSSize(width: 760, height: 480)
    self.setFrameAutosaveName("tern.main")

    // Content runs under the title bar; the sidebar draws behind the
    // traffic lights, as in native Mac apps.
    self.styleMask.insert(.fullSizeContentView)
    self.titlebarAppearsTransparent = true
    self.titleVisibility = .hidden
    self.title = "tern"
    self.isMovableByWindowBackground = false

    // Flutter draws under the transparent title bar, so it asks the window
    // to drag or zoom when the user grabs a header.
    let channel = FlutterMethodChannel(
      name: "tern/window", binaryMessenger: flutterViewController.engine.binaryMessenger)
    channel.setMethodCallHandler { [weak self] call, result in
      guard let self = self else { return result(nil) }
      switch call.method {
      case "drag":
        if let event = NSApp.currentEvent { self.performDrag(with: event) }
      case "zoom":
        self.zoom(nil)
      default:
        return result(FlutterMethodNotImplemented)
      }
      result(nil)
    }

    RegisterGeneratedPlugins(registry: flutterViewController)

    super.awakeFromNib()
  }
}
