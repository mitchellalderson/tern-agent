import Cocoa
import FlutterMacOS
import UserNotifications

class MainFlutterWindow: NSWindow {
  private var notifications: Notifications?

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
    // to drag or zoom when the user grabs a header. The same channel
    // carries notifications and the dock badge.
    let channel = FlutterMethodChannel(
      name: "tern/window", binaryMessenger: flutterViewController.engine.binaryMessenger)
    let notifications = Notifications(channel: channel)
    self.notifications = notifications
    channel.setMethodCallHandler { [weak self] call, result in
      guard let self = self else { return result(nil) }
      let args = call.arguments as? [String: Any] ?? [:]
      switch call.method {
      case "drag":
        if let event = NSApp.currentEvent { self.performDrag(with: event) }
      case "zoom":
        self.zoom(nil)
      case "notify":
        notifications.show(
          id: args["id"] as? String ?? "",
          title: args["title"] as? String ?? "tern",
          body: args["body"] as? String ?? "",
          approval: args["approval"] as? Bool ?? false)
      case "badge":
        let count = args["count"] as? Int ?? 0
        NSApp.dockTile.badgeLabel = count > 0 ? String(count) : nil
      default:
        return result(FlutterMethodNotImplemented)
      }
      result(nil)
    }

    RegisterGeneratedPlugins(registry: flutterViewController)

    super.awakeFromNib()
  }
}

/// System notifications. Approvals get Allow once / Deny buttons; clicks
/// come back to Flutter as `notificationAction {id, action}`.
class Notifications: NSObject, UNUserNotificationCenterDelegate {
  private let channel: FlutterMethodChannel
  private var authorized = false

  init(channel: FlutterMethodChannel) {
    self.channel = channel
    super.init()
    let center = UNUserNotificationCenter.current()
    center.delegate = self
    let allow = UNNotificationAction(identifier: "allow", title: "Allow once")
    let deny = UNNotificationAction(identifier: "deny", title: "Deny", options: [.destructive])
    center.setNotificationCategories([
      UNNotificationCategory(
        identifier: "approval", actions: [allow, deny], intentIdentifiers: [])
    ])
  }

  func show(id: String, title: String, body: String, approval: Bool) {
    let center = UNUserNotificationCenter.current()
    // Asked once, the first time there's something to say.
    center.requestAuthorization(options: [.alert, .sound, .badge]) { granted, _ in
      guard granted else { return }
      let content = UNMutableNotificationContent()
      content.title = title
      content.body = body
      content.userInfo = ["id": id]
      if approval {
        content.categoryIdentifier = "approval"
        content.sound = .default
      }
      center.add(UNNotificationRequest(identifier: id, content: content, trigger: nil))
    }
  }

  func userNotificationCenter(
    _ center: UNUserNotificationCenter,
    didReceive response: UNNotificationResponse,
    withCompletionHandler completionHandler: @escaping () -> Void
  ) {
    let id = response.notification.request.content.userInfo["id"] as? String ?? ""
    let action: String
    switch response.actionIdentifier {
    case "allow": action = "allow"
    case "deny": action = "deny"
    default:
      action = "open"
      NSApp.activate(ignoringOtherApps: true)
    }
    DispatchQueue.main.async {
      self.channel.invokeMethod("notificationAction", arguments: ["id": id, "action": action])
    }
    completionHandler()
  }

  // Shown even while the app is frontmost; Flutter only sends ones the
  // user can't already see.
  func userNotificationCenter(
    _ center: UNUserNotificationCenter,
    willPresent notification: UNNotification,
    withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
  ) {
    completionHandler([.banner, .sound])
  }
}
