import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'endpoint.dart';
import 'models.dart';
import 'providers.dart';
import 'sse.dart';
import 'subagents.dart';
import 'tools.dart';

/// An error response from the daemon (`{"error": "..."}`) or a failure to
/// reach it.
class TernException implements Exception {
  const TernException(this.message, {this.status});

  final String message;
  final int? status;

  bool get unreachable => status == null;

  @override
  String toString() => message;
}

/// HTTP 401 from a remote core: the device token was revoked or never
/// valid. Show "Signed out of <core>" with re-pair; retrying won't help.
class SignedOutException extends TernException {
  const SignedOutException(super.message, {required this.core})
    : super(status: 401);

  final String core;
}

/// What redeeming a pairing link returns.
class Pairing {
  const Pairing({
    required this.deviceId,
    required this.scope,
    required this.endpoint,
  });

  final String deviceId;

  /// `owner`, `chat` or `read`.
  final String scope;

  /// Ready to use; keep its token in the Keychain, not in preferences.
  final RemoteEndpoint endpoint;
}

class TernClient {
  TernClient(this.endpoint) : _http = endpoint.httpClient();

  final Endpoint endpoint;
  final HttpClient _http;

  void close() => _http.close(force: true);

  /// Redeem a pairing link (deployment spec §6.2 step 2), pinning the
  /// core's certificate when the link has a fingerprint.
  static Future<Pairing> pair(
    PairingLink link, {
    required String deviceName,
    String? platform,
  }) async {
    // A throwaway token: pairing is the one route that needs none.
    final bootstrap = RemoteEndpoint(
      link.url,
      token: '',
      pin: link.fingerprint,
    );
    final client = TernClient(bootstrap);
    try {
      final j =
          await client._request('POST', '/v1/auth/pair', {
                'code': link.code,
                'device_name': deviceName,
                'platform': ?platform,
              }, auth: false)
              as Json;
      return Pairing(
        deviceId: j['device_id'] as String,
        scope: j['scope'] as String,
        endpoint: RemoteEndpoint(
          link.url,
          token: j['token'] as String,
          pin: link.fingerprint,
        ),
      );
    } on SignedOutException catch (e) {
      // On this route a 401 means the code, not a token.
      throw TernException(e.message, status: 401);
    } finally {
      client.close();
    }
  }

  Future<Health> health() async =>
      Health.fromJson(await _json('GET', '/v1/health'));

  Future<Info> info() async => Info.fromJson(await _json('GET', '/v1/info'));

  /// Every tool the agent has, with its argument schema.
  Future<List<ToolSpec>> tools() async => [
    for (final t in await _json('GET', '/v1/tools') as List)
      ToolSpec.fromJson(t as Json),
  ];

  /// Resolve `model` (the provider may be left off when only one lists
  /// it). Throws [TernException] with the daemon's reason if it's unusable.
  Future<Resolved> checkModel(String model) async => Resolved.fromJson(
    await _json('POST', '/v1/models/check', {'model': model}),
    model,
  );

  /// Configured providers, with masked keys.
  Future<List<ProviderStatus>> providers() async => [
    for (final p in await _json('GET', '/v1/providers') as List)
      ProviderStatus.fromJson(p as Json),
  ];

  /// Model lists, from the daemon's cache unless `refresh`.
  Future<List<ProviderModels>> models({
    String? provider,
    bool refresh = false,
  }) async {
    final q = [
      if (provider != null) 'provider=${Uri.encodeQueryComponent(provider)}',
      if (refresh) 'refresh=true',
    ].join('&');
    return [
      for (final m
          in await _json('GET', '/v1/models${q.isEmpty ? '' : '?$q'}') as List)
        ProviderModels.fromJson(m as Json),
    ];
  }

  /// Check a new provider, or a change to `existing`, with the provider
  /// itself. Saves nothing.
  Future<Verified> verifyProvider(
    ProviderDraft draft, {
    String? existing,
  }) async => Verified.fromJson(
    await _json(
      'POST',
      existing == null
          ? '/v1/providers/verify'
          : '/v1/providers/$existing/verify',
      draft.toJson(),
    ),
  );

  /// Verify, store the key, write the config and reload providers. Throws
  /// [TernException] with the reason when the key is rejected.
  Future<ProviderStatus> saveProvider(
    ProviderDraft draft, {
    String? existing,
    String? defaultModel,
  }) async {
    final body = {...draft.toJson(), 'default_model': ?defaultModel};
    final j =
        await _json(
              existing == null ? 'POST' : 'PATCH',
              existing == null ? '/v1/providers' : '/v1/providers/$existing',
              body,
            )
            as Json;
    return ProviderStatus.fromJson(j['provider'] as Json);
  }

  /// Remove a provider and, with `removeSecret`, its key (unless
  /// something else uses it).
  Future<void> removeProvider(String name, {bool removeSecret = true}) =>
      _json('DELETE', '/v1/providers/$name?remove_secret=$removeSecret');

  /// Make `model` the default for new conversations.
  Future<Resolved> setDefaultModel(String model) async {
    final j =
        await _json('PUT', '/v1/config/default_model', {'model': model})
            as Json;
    return Resolved.fromJson(j, model);
  }

  Future<List<SessionItem>> sessions({int limit = 50}) async => [
    for (final s in await _json('GET', '/v1/sessions?limit=$limit') as List)
      SessionItem.fromJson(s as Json),
  ];

  Future<SessionDetail> session(String id) async =>
      SessionDetail.fromJson(await _json('GET', '/v1/sessions/$id'));

  Future<void> setModel(String session, String model) =>
      _json('PUT', '/v1/sessions/$session/model', {'model': model});

  Future<Usage> usage(String session) async =>
      Usage.fromJson(await _json('GET', '/v1/sessions/$session/usage'));

  /// Send a message. Without `session` the daemon starts a new one. A run
  /// already in progress in that session is cancelled first.
  Future<Started> send(String text, {String? session, String? model}) async =>
      Started.fromJson(
        await _json('POST', '/v1/runs', {
          'text': text,
          'session_id': ?session,
          'model': ?model,
        }),
      );

  Future<bool> cancel(String run) async =>
      (await _json('POST', '/v1/runs/$run/cancel'))['cancelled'] as bool;

  Future<void> resume(String run) => _json('POST', '/v1/runs/$run/resume');

  /// Every sub-agent started from this conversation's runs.
  Future<List<ChildRun>> subagents(String session) async => [
    for (final r
        in await _json('GET', '/v1/sessions/$session/subagents') as List)
      ChildRun.fromJson(r as Json),
  ];

  /// Tell a running sub-agent something, as the user.
  Future<void> messageRun(String run, String text) =>
      _json('POST', '/v1/runs/$run/message', {'text': text});

  Future<List<ActiveRun>> activeRuns() async => [
    for (final r in await _json('GET', '/v1/runs/active') as List)
      ActiveRun.fromJson(r as Json),
  ];

  Future<List<Approval>> pendingApprovals() async => [
    for (final a in await _json('GET', '/v1/approvals') as List)
      Approval.fromJson(a as Json),
  ];

  Future<List<Schedule>> schedules() async => [
    for (final s in await _json('GET', '/v1/schedules') as List)
      Schedule.fromJson(s as Json),
  ];

  /// `deliverTo`: null keeps results in the routine's session; else
  /// `telegram:<chat_id>`.
  Future<Schedule> createSchedule({
    required String cron,
    required String prompt,
    String? deliverTo,
  }) async => Schedule.fromJson(
    await _json('POST', '/v1/schedules', {
      'cron': cron,
      'prompt': prompt,
      'deliver_to': ?deliverTo,
    }),
  );

  /// Pass `deliverTo: 'none'` to stop delivering.
  Future<Schedule> updateSchedule(
    String id, {
    String? cron,
    String? prompt,
    bool? enabled,
    String? deliverTo,
  }) async => Schedule.fromJson(
    await _json('PATCH', '/v1/schedules/$id', {
      'cron': ?cron,
      'prompt': ?prompt,
      'enabled': ?enabled,
      'deliver_to': ?deliverTo,
    }),
  );

  Future<void> deleteSchedule(String id) =>
      _json('DELETE', '/v1/schedules/$id');

  /// Run a routine now; its next scheduled run doesn't move.
  Future<Started> runSchedule(String id) async {
    final j = await _json('POST', '/v1/schedules/$id/run') as Json;
    return Started.fromJson({...j, 'model': ''});
  }

  /// Validate `cron` and get its next three runs. Throws [TernException]
  /// with the reason if it's invalid.
  Future<CronPreview> previewCron(String cron) async => CronPreview.fromJson(
    await _json('POST', '/v1/schedules/preview', {'cron': cron}),
  );

  /// A conversation's runs; with [subagents], the runs they started too.
  Future<List<RunItem>> runs(String session, {bool subagents = false}) async =>
      [
        for (final r
            in await _json(
                  'GET',
                  '/v1/sessions/$session/runs'
                      '${subagents ? '?include=subagents' : ''}',
                )
                as List)
          RunItem.fromJson(r as Json),
      ];

  Future<List<StepItem>> steps(String run) async => [
    for (final s in await _json('GET', '/v1/runs/$run/steps') as List)
      StepItem.fromJson(s as Json),
  ];

  /// Every secret in `secrets.toml` or referred to by config or a
  /// connector. Values are never returned.
  Future<List<SecretStatus>> secrets() async => [
    for (final s in await _json('GET', '/v1/secrets') as List)
      SecretStatus.fromJson(s as Json),
  ];

  /// Store `value` in `secrets.toml`. Takes effect without a restart.
  Future<SecretStatus> setSecret(String name, String value) async =>
      SecretStatus.fromJson(
        await _json('PUT', '/v1/secrets/$name', {'value': value}),
      );

  /// Remove `name` from `secrets.toml`. Returns false if it wasn't there.
  Future<bool> removeSecret(String name) async {
    try {
      await _json('DELETE', '/v1/secrets/$name');
      return true;
    } on TernException catch (e) {
      if (e.status == 404) return false;
      rethrow;
    }
  }

  Future<void> answer(String approval, Decision decision) =>
      _json('POST', '/v1/approvals/$approval', {'decision': decision.wire});

  /// Put a file in the core's workspace (remote cores can't take a path).
  /// Returns its workspace-relative path, to mention in a message. Check
  /// `features` in [health] for `uploads` first.
  Future<String> upload(String name, List<int> bytes) async {
    final boundary = 'tern-${DateTime.now().microsecondsSinceEpoch}';
    final safe = name.replaceAll(RegExp(r'["\r\n]'), '_');
    final body = <int>[
      ...utf8.encode(
        '--$boundary\r\n'
        'Content-Disposition: form-data; name="file"; filename="$safe"\r\n'
        'Content-Type: application/octet-stream\r\n\r\n',
      ),
      ...bytes,
      ...utf8.encode('\r\n--$boundary--\r\n'),
    ];
    final j = await _request(
      'POST',
      '/v1/uploads',
      body,
      contentType: ContentType(
        'multipart',
        'form-data',
        parameters: {'boundary': boundary},
      ),
    );
    return (j as Json)['path'] as String;
  }

  /// Owner only: paired devices.
  Future<List<Device>> devices() async => [
    for (final d in await _json('GET', '/v1/devices') as List)
      Device.fromJson(d as Json),
  ];

  /// Owner only: a pairing link for a new device (the app's Add device).
  Future<PairingOffer> offerPairing({
    String scope = 'chat',
    String? name,
  }) async => PairingOffer.fromJson(
    await _json('POST', '/v1/devices/pair', {'scope': scope, 'name': ?name}),
  );

  /// Owner only: sign a device out at once.
  Future<void> revokeDevice(String id) => _json('DELETE', '/v1/devices/$id');

  /// The live event stream. It ends (with an error) when the connection
  /// drops; callers reconnect and resync, except after a
  /// [SignedOutException], which no retry fixes. Cancelling closes the
  /// connection.
  Stream<TernEvent> events() {
    // Its own HttpClient: cancelling an open response doesn't return until
    // the server ends it, so the connection is force-closed instead.
    final http = endpoint.httpClient();
    final out = StreamController<TernEvent>();
    out.onListen = () async {
      final HttpClientResponse res;
      try {
        res = await _send(
          'GET',
          '/v1/events',
          null,
          client: http,
          accept: 'text/event-stream',
        );
      } on TernException catch (e) {
        out.addError(e);
        await out.close();
        return;
      }
      if (res.statusCode >= 400) {
        final text = await res.transform(utf8.decoder).join();
        String? msg;
        try {
          final j = jsonDecode(text);
          if (j is Json) msg = j['error'] as String?;
        } on FormatException {
          // Not JSON; use the status.
        }
        out.addError(_error(res.statusCode, msg));
        await out.close();
        return;
      }
      res
          .transform(const SseDecoder())
          .listen(
            (data) {
              final Object? decoded;
              try {
                decoded = jsonDecode(data);
              } on FormatException {
                return;
              }
              if (decoded is Json) out.add(TernEvent.fromJson(decoded));
            },
            onError: (Object e) =>
                out.addError(TernException('event stream lost: $e')),
            onDone: out.close,
            cancelOnError: true,
          );
    };
    out.onCancel = () => http.close(force: true);
    return out.stream;
  }

  Future<dynamic> _json(String method, String path, [Object? body]) =>
      _request(method, path, body);

  Future<dynamic> _request(
    String method,
    String path,
    Object? body, {
    ContentType? contentType,
    bool auth = true,
  }) async {
    final res = await _send(
      method,
      path,
      body,
      contentType: contentType,
      auth: auth,
    );
    final text = await res.transform(utf8.decoder).join();
    Object? decoded;
    try {
      decoded = text.isEmpty ? null : jsonDecode(text);
    } on FormatException {
      decoded = text;
    }
    if (res.statusCode >= 400) {
      final msg = decoded is Json ? decoded['error'] as String? : null;
      throw _error(res.statusCode, msg);
    }
    return decoded;
  }

  TernException _error(int status, String? msg) {
    if (status == HttpStatus.unauthorized && endpoint is RemoteEndpoint) {
      return SignedOutException(
        'Signed out of ${endpoint.describe()}: ${msg ?? 'the device token was revoked'}',
        core: endpoint.describe(),
      );
    }
    return TernException(msg ?? 'HTTP $status', status: status);
  }

  Future<HttpClientResponse> _send(
    String method,
    String path,
    Object? body, {
    HttpClient? client,
    String accept = 'application/json',
    ContentType? contentType,
    bool auth = true,
  }) async {
    try {
      final req = await (client ?? _http).openUrl(
        method,
        endpoint.base.resolve(path),
      );
      req.headers.set(HttpHeaders.acceptHeader, accept);
      if (auth) endpoint.headers.forEach(req.headers.set);
      if (body is List<int> && contentType != null) {
        req.headers.contentType = contentType;
        req.contentLength = body.length;
        req.add(body);
      } else if (body != null) {
        req.headers.contentType = ContentType.json;
        req.write(jsonEncode(body));
      }
      return await req.close();
    } on SocketException catch (e) {
      throw TernException(
        "can't reach tern at ${endpoint.describe()}: "
        '${e.osError?.message ?? e.message}',
      );
    } on TlsException catch (e) {
      // With a pin, this is also what a different certificate looks like.
      throw TernException(
        "can't establish a secure connection to ${endpoint.describe()}: "
        '${e.osError?.message ?? e.message} '
        "(the core's certificate isn't the one this device paired with?)",
      );
    } on HttpException catch (e) {
      throw TernException(e.message);
    }
  }
}
