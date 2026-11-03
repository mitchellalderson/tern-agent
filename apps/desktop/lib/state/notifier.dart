import 'dart:io';

import 'package:flutter/services.dart';

/// What a click on a notification asks for.
enum NotificationAction { open, allow, deny }

/// System notifications and the dock badge. macOS uses the `tern/window`
/// channel (UserNotifications in `MainFlutterWindow.swift`); Linux shells
/// out to `notify-send`, without action buttons.
abstract class Notifier {
  factory Notifier.platform() {
    if (Platform.isMacOS) return _MacNotifier();
    if (Platform.isLinux) return _LinuxNotifier();
    return const SilentNotifier();
  }

  /// `id` comes back to [onAction]: a session id, or an approval id when
  /// [approval] is true (which adds Allow / Deny buttons).
  Future<void> show({
    required String id,
    required String title,
    required String body,
    bool approval = false,
  });

  Future<void> badge(int count);

  /// Set by the store.
  void Function(String id, NotificationAction action)? onAction;
}

class SilentNotifier implements Notifier {
  const SilentNotifier();

  @override
  Future<void> show({
    required String id,
    required String title,
    required String body,
    bool approval = false,
  }) async {}

  @override
  Future<void> badge(int count) async {}

  @override
  set onAction(void Function(String, NotificationAction)? _) {}

  @override
  void Function(String, NotificationAction)? get onAction => null;
}

class _MacNotifier implements Notifier {
  _MacNotifier() {
    _channel.setMethodCallHandler((call) async {
      if (call.method != 'notificationAction') return;
      final args = (call.arguments as Map).cast<String, Object?>();
      final action = switch (args['action']) {
        'allow' => NotificationAction.allow,
        'deny' => NotificationAction.deny,
        _ => NotificationAction.open,
      };
      onAction?.call(args['id']! as String, action);
    });
  }

  static const _channel = MethodChannel('tern/window');

  @override
  void Function(String, NotificationAction)? onAction;

  @override
  Future<void> show({
    required String id,
    required String title,
    required String body,
    bool approval = false,
  }) => _channel.invokeMethod<void>('notify', {
    'id': id,
    'title': title,
    'body': body,
    'approval': approval,
  });

  @override
  Future<void> badge(int count) =>
      _channel.invokeMethod<void>('badge', {'count': count});
}

class _LinuxNotifier implements Notifier {
  @override
  void Function(String, NotificationAction)? onAction;

  @override
  Future<void> show({
    required String id,
    required String title,
    required String body,
    bool approval = false,
  }) async {
    try {
      await Process.run('notify-send', ['--app-name=tern', title, body]);
    } on ProcessException {
      // No notification daemon; the app still shows everything.
    }
  }

  @override
  Future<void> badge(int count) async {}
}
