import 'models.dart';

/// A webhook routine's hook (T1c): where senders POST, and how they prove
/// who they are. [secret] is set only in the answer that made it.
class Webhook {
  const Webhook({
    required this.routineId,
    required this.auth,
    required this.path,
    this.url,
    this.active = true,
    this.secret,
    required this.createdAt,
    this.rotatedAt,
    this.revokedAt,
    this.lastDeliveryAt,
  });

  factory Webhook.fromJson(Json j) {
    DateTime? time(Object? t) =>
        t == null ? null : DateTime.fromMillisecondsSinceEpoch(t as int);
    return Webhook(
      routineId: j['routine_id'] as String,
      auth: j['auth'] as String,
      path: j['path'] as String,
      url: j['url'] as String?,
      active: j['active'] as bool? ?? true,
      secret: j['secret'] as String?,
      createdAt: time(j['created_at'])!,
      rotatedAt: time(j['rotated_at']),
      revokedAt: time(j['revoked_at']),
      lastDeliveryAt: time(j['last_delivery_at']),
    );
  }

  final String routineId;

  /// `hmac` (senders sign the body) or `bearer` (they send the secret).
  final String auth;

  /// `/v1/hooks/<id>` on the webhook listener.
  final String path;

  /// The full URL; null while `[webhooks]` has no listener or public URL.
  final String? url;

  /// False once revoked: the URL answers 404 until rotated.
  final bool active;
  final String? secret;
  final DateTime createdAt;
  final DateTime? rotatedAt;
  final DateTime? revokedAt;
  final DateTime? lastDeliveryAt;

  /// How a sender authenticates, in words.
  String get howToSign => auth == 'bearer'
      ? 'Send the secret as `Authorization: Bearer <secret>`.'
      : 'Sign each body with HMAC-SHA256: GitHub\'s X-Hub-Signature-256, or '
            'X-Tern-Signature: t=<unix seconds>,v1=<hex of HMAC("<t>.<body>")>.';
}

/// `POST /v1/webhooks`: the routine and its hook, with the secret.
class CreatedWebhook {
  const CreatedWebhook({required this.schedule, required this.webhook});

  factory CreatedWebhook.fromJson(Json j) => CreatedWebhook(
    schedule: Schedule.fromJson(j['schedule'] as Json),
    webhook: Webhook.fromJson(j['webhook'] as Json),
  );

  final Schedule schedule;
  final Webhook webhook;
}
